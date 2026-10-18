#!/usr/bin/env python3
"""Build the native host and its matching CLI without a frontend build tool."""

import argparse
import json
import os
from pathlib import Path
import plistlib
import shutil
import subprocess
import sys
import tarfile
import zipfile


ROOT = Path(__file__).resolve().parents[1]
DESKTOP = ROOT / "crates" / "drmr-desktop"


def run(arguments, env, capture=False):
    return subprocess.run(arguments, cwd=ROOT, env=env, check=True,
                          stdout=subprocess.PIPE if capture else None,
                          text=capture)


def collect_notices(metadata, destination):
    """Retain package notices for the host, bundled CLI and their dependencies."""
    packages = {package["id"]: package for package in metadata["packages"]}
    nodes = {node["id"]: node for node in metadata["resolve"]["nodes"]}
    pending = [key for key, package in packages.items()
               if package["name"] in ("drmr", "drmr-desktop")]
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
        shutil.rmtree(destination)
    destination.mkdir(parents=True)
    inventory = []
    for key in sorted(reached):
        package = packages[key]
        home = Path(package["manifest_path"]).parent
        files = set()
        for pattern in ("LICENSE*", "LICENCE*", "COPYING*", "COPYRIGHT*", "NOTICE*", "UNLICENSE*"):
            files.update(path for path in home.glob(pattern) if path.is_file())
        if package.get("license_file"):
            files.add(home / package["license_file"])
        if package["name"].startswith("drmr"):
            files.add(ROOT / "LICENSE")
        if not files and not package.get("license"):
            raise ValueError("dependency-license-missing: " + package["name"])
        name = package["name"] + "-" + package["version"]
        output = destination / name
        output.mkdir()
        for source in sorted(files):
            shutil.copyfile(source, output / source.name)
        declaration = {"name": package["name"], "version": package["version"],
                       "license": package.get("license"), "authors": package["authors"],
                       "repository": package.get("repository"),
                       "notices": sorted(path.name for path in files)}
        if (package.get("source") or "").startswith("registry+"):
            declaration["source_archive"] = ("https://static.crates.io/crates/" + package["name"]
                                              + "/" + name + ".crate")
        if not files:
            # Some publishers provide SPDX metadata without a separate license file.
            declaration["notice_status"] = "publisher-metadata-only"
            (output / "declaration.json").write_text(json.dumps(declaration, indent=2) + "\n", encoding="utf-8")
        else:
            declaration["notice_status"] = "publisher-notices-retained"
        inventory.append(declaration)
    web_assets = ROOT / "crates" / "drmr-server" / "static"
    for source in web_assets.rglob("*"):
        if source.is_file() and source.name.upper().startswith(("LICENSE", "NOTICE", "COPYRIGHT", "OFL")):
            target = destination / "web-assets" / source.relative_to(web_assets)
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(source, target)
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
        def metadata(member):
            member.uid = member.gid = 0
            member.uname = member.gname = ""
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
    collect_notices(metadata, DESKTOP / "bundle-resources")
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
