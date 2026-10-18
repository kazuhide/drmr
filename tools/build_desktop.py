#!/usr/bin/env python3
"""Build the native host and its matching CLI without a frontend build tool."""

import argparse
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import plistlib
import shutil
import subprocess
import sys
import tarfile
import zipfile
from urllib.parse import urlsplit


ROOT = Path(__file__).resolve().parents[1]
DESKTOP = ROOT / "crates" / "drmr-desktop"


def run(arguments, env, capture=False):
    return subprocess.run(arguments, cwd=ROOT, env=env, check=True,
                          stdout=subprocess.PIPE if capture else None,
                          text=capture)


def is_notice(path):
    """Recognize notice documents without mistaking source modules for licenses."""
    return (path.is_file() and
            path.name.upper().startswith(("LICENSE", "LICENCE", "COPYING", "COPYRIGHT", "NOTICE", "UNLICENSE")) and
            path.suffix.lower() not in (".rs", ".c", ".h", ".py", ".js", ".ts", ".toml", ".yml", ".yaml"))


def pinned_notices(package):
    """Resolve an exact publisher-version fallback, never a metadata-only grant."""
    missing = "dependency-license-missing: " + package["name"]
    notice_root = ROOT / "tools" / "desktop-licenses"
    manifest = notice_root / "manifest.json"
    if not manifest.is_file():
        raise ValueError(missing)
    data = json.loads(manifest.read_text(encoding="utf-8"))
    if data.get("schema_version") != 1:
        raise ValueError("dependency-license-manifest-invalid")
    matches = [entry for entry in data["packages"]
               if (entry["name"], entry["version"]) == (package["name"], package["version"])]
    if len(matches) != 1:
        raise ValueError(missing)
    entry = matches[0]
    home = Path(package["manifest_path"]).parent
    vcs = home / ".cargo_vcs_info.json"
    revision = json.loads(vcs.read_text(encoding="utf-8"))["git"]["sha1"] if vcs.is_file() else None
    if (entry["license"] != package.get("license") or
            entry["repository"] != package.get("repository") or
            entry["revision"] != revision or entry["selected_license"] not in entry["license"].split(" OR ") or not entry["notices"]):
        raise ValueError("dependency-license-provenance-mismatch: " + package["name"])
    files = []
    for notice in entry["notices"]:
        relative = PurePosixPath(notice["file"])
        if (not relative.parts or relative.is_absolute() or ".." in relative.parts or
                "\\" in notice["file"] or ":" in notice["file"]):
            raise ValueError("dependency-license-path-invalid")
        path = notice_root.joinpath(*relative.parts)
        if path.is_symlink() or not path.resolve().is_relative_to(notice_root.resolve()):
            raise ValueError("dependency-license-path-invalid")
        contents = path.read_bytes()
        url = urlsplit(notice["source_url"])
        if not contents or url.scheme != "https" or not url.hostname or url.username or url.password:
            raise ValueError("dependency-license-provenance-invalid")
        if hashlib.sha256(contents).hexdigest() != notice["sha256"]:
            raise ValueError("dependency-license-checksum-mismatch: " + package["name"])
        files.append(path)
    if len({path.name for path in files}) != len(files):
        raise ValueError("dependency-license-filename-collision")
    return files, {key: entry[key] for key in ("selected_license", "repository", "revision", "notices")}


def collect_notices(metadata, destination, roots=("drmr", "drmr-desktop"), replace=False):
    """Retain package notices for the host, bundled CLI and their dependencies."""
    packages = {package["id"]: package for package in metadata["packages"]}
    nodes = {node["id"]: node for node in metadata["resolve"]["nodes"]}
    pending = [key for key, package in packages.items()
               if package["name"] in roots]
    if {packages[key]["name"] for key in pending} != set(roots):
        raise ValueError("dependency-license-root-missing")
    reached = set()
    while pending:
        key = pending.pop()
        if key in reached:
            continue
        reached.add(key)
        for dependency in nodes[key]["deps"]:
            if any(kind["kind"] != "dev" for kind in dependency["dep_kinds"]):
                pending.append(dependency["pkg"])
    if destination.exists():
        if not replace:
            raise ValueError("dependency-notice-destination-exists")
        shutil.rmtree(destination)
    destination.mkdir(parents=True)
    inventory = []
    for key in sorted(reached):
        package = packages[key]
        home = Path(package["manifest_path"]).parent
        files = set()
        license_texts = set()
        extra_notices = set()
        if package["name"].startswith("drmr"):
            files.add(ROOT / "LICENSE")
            license_texts.update(files)
        else:
            files.update(path for path in home.iterdir() if is_notice(path))
            license_texts.update(path for path in files if path.name.upper().startswith(("LICENSE", "LICENCE", "COPYING", "UNLICENSE")))
            extra_notices.update(path for path in home.rglob("*") if is_notice(path))
            if package.get("license_file"):
                files.add(home / package["license_file"])
                license_texts.add(home / package["license_file"])
        provenance = None
        fallback = []
        if not license_texts:
            fallback, provenance = pinned_notices(package)
            files.update(fallback)
        files.update(extra_notices)
        name = package["name"] + "-" + package["version"]
        output = destination / name
        output.mkdir()
        retained = []
        for source in sorted(files):
            relative = source.name if source in fallback or not source.is_relative_to(home) else source.relative_to(home)
            target = output / relative
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(source, target)
            retained.append(str(Path(relative).as_posix()))
        declaration = {"name": package["name"], "version": package["version"],
                       "license": package.get("license"), "authors": package["authors"],
                       "repository": package.get("repository"),
                       "notices": sorted(retained)}
        if (package.get("source") or "").startswith("registry+"):
            declaration["source_archive"] = ("https://static.crates.io/crates/" + package["name"]
                                              + "/" + name + ".crate")
        if "MPL-2.0" in (package.get("license") or ""):
            if "source_archive" not in declaration:
                raise ValueError("dependency-source-offer-missing: " + package["name"])
            source_notice = ("The complete, unmodified source for " + name + " is available at:\n" +
                             declaration["source_archive"] + "\n\nLicense declaration: " + package["license"] +
                             "\nMozilla Public License 2.0: https://mozilla.org/MPL/2.0/\n")
            if provenance:
                source_notice += "Publisher revision: " + provenance["revision"] + "\n"
            (output / "SOURCE.txt").write_text(source_notice, encoding="utf-8")
            declaration["source_availability_notice"] = "SOURCE.txt"
        declaration["notice_status"] = "publisher-pinned-notices" if provenance else "publisher-notices-retained"
        if provenance:
            declaration["pinned_provenance"] = provenance
        inventory.append(declaration)
    web_assets = ROOT / "crates" / "drmr-server" / "static"
    for source in web_assets.rglob("*"):
        if source.is_file() and source.name.upper().startswith(("LICENSE", "NOTICE", "COPYRIGHT", "OFL")):
            target = destination / "web-assets" / source.relative_to(web_assets)
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(source, target)
    # Retain the vendored notices in every CLI archive, including targets that do not link it.
    vendored = ROOT / "vendor" / "process-wrap"
    if vendored.is_dir():
        target = destination / "process-wrap"
        target.mkdir()
        for name in ("COPYRIGHT", "LICENSE-MIT", "LICENSE-APACHE"):
            shutil.copyfile(vendored / name, target / name)
    (destination / "inventory.json").write_text(json.dumps(inventory, indent=2) + "\n", encoding="utf-8")


def bundle(profile, suffix, version, target_directory):
    """Create an unsigned native app or a portable directory, without an installer."""
    output = target_directory / "desktop-bundles"
    output.mkdir(exist_ok=True)
    destination = output / ("drmr.app" if sys.platform == "darwin" else "drmr-desktop")
    if destination.exists():
        raise ValueError("bundle-destination-exists; move the previous bundle before rebuilding")
    if sys.platform == "darwin":
        binaries = destination / "Contents" / "MacOS"
        resources = destination / "Contents" / "Resources"
    else:
        binaries = destination
        resources = destination
    binaries.mkdir(parents=True)
    resources.mkdir(parents=True, exist_ok=True)
    for name in ("drmr", "drmr-desktop"):
        shutil.copy2(profile / (name + suffix), binaries / (name + suffix))
    shutil.copytree(DESKTOP / "bundle-resources", resources / "licenses")
    shutil.copytree(ROOT / "docs", resources / "docs")
    for name in ("LICENSE", "README.md"):
        shutil.copyfile(ROOT / name, resources / name)
    if sys.platform == "darwin":
        shutil.copyfile(DESKTOP / "icons" / "icon.icns", resources / "icon.icns")
        info = plistlib.loads((DESKTOP / "Info.plist").read_bytes())
        info.update({"CFBundleIdentifier": "io.github.kazuhide.drmr",
                          "CFBundleExecutable": "drmr-desktop", "CFBundleName": "drmr",
                          "CFBundlePackageType": "APPL", "CFBundleIconFile": "icon.icns", "CFBundleVersion": version,
                          "CFBundleShortVersionString": version,
                          "LSMinimumSystemVersion": "11.0", "NSHighResolutionCapable": True})
        with (destination / "Contents" / "Info.plist").open("wb") as stream:
            plistlib.dump(info, stream)
    return destination


def archive_bundle(destination):
    """Keep native executable modes inside the uploaded artifact."""
    if sys.platform == "win32":
        archive = destination.with_suffix(".zip")
        with zipfile.ZipFile(archive, "x", compression=zipfile.ZIP_DEFLATED) as output:
            for source in sorted(destination.rglob("*")):
                output.write(source, source.relative_to(destination.parent))
    else:
        archive = destination.parent / (destination.name + ".tar.gz")
        binary_root = PurePosixPath(destination.name)
        if sys.platform == "darwin":
            binary_root = binary_root / "Contents" / "MacOS"
        executables = {str(binary_root / name) for name in ("drmr", "drmr-desktop")}
        def metadata(member):
            member.uid = member.gid = 0
            member.uname = member.gname = ""
            # Target executables need POSIX modes even when the host cannot represent them.
            if member.isfile() and member.name in executables:
                member.mode = 0o755
            return member
        with tarfile.open(archive, "x:gz") as output:
            output.add(destination, arcname=destination.name, filter=metadata)
    return archive


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--release", action="store_true", help="Build optimized binaries")
    parser.add_argument("--prepare-only", action="store_true", help="Stage the matching CLI and notices before a native Cargo build")
    parser.add_argument("--bundle", action="store_true", help="Create an unsigned macOS app or a portable Windows/Linux directory")
    args = parser.parse_args()
    if args.prepare_only and args.bundle:
        parser.error("--prepare-only and --bundle cannot be combined")
    env = os.environ.copy()
    if not env.get("RUSTUP_TOOLCHAIN"):
        parser.error("Set RUSTUP_TOOLCHAIN explicitly to the supported Rust toolchain")
    env.setdefault("CARGO_TARGET_DIR", str(ROOT / "target" / "desktop"))
    host = next(line.split(": ", 1)[1] for line in run(["rustc", "-vV"], env, True).stdout.splitlines()
                if line.startswith("host: "))
    cargo = ["cargo", "build", "--locked", "--target", host]
    if args.release:
        cargo.append("--release")
    run(cargo + ["-p", "drmr"], env)
    metadata = json.loads(run(["cargo", "metadata", "--format-version", "1", "--locked",
                              "--filter-platform", host, "--features", "drmr-desktop/desktop"], env, True).stdout)
    target_directory = Path(metadata["target_directory"])
    profile = target_directory / host / ("release" if args.release else "debug")
    suffix = ".exe" if "windows" in host else ""
    stage = DESKTOP / "binaries"
    stage.mkdir(exist_ok=True)
    shutil.copy2(profile / ("drmr" + suffix), stage / ("drmr-" + host + suffix))
    collect_notices(metadata, DESKTOP / "bundle-resources", replace=True)
    if args.prepare_only:
        print("Desktop CLI and dependency notices prepared.")
        return
    run(cargo + ["-p", "drmr-desktop", "--features", "desktop"], env)
    if args.bundle:
        version = next(package["version"] for package in metadata["packages"] if package["name"] == "drmr-desktop")
        destination = bundle(profile, suffix, version, target_directory)
        print(archive_bundle(destination))
    else:
        print("Desktop host and matching CLI built.")


if __name__ == "__main__":
    main()
