#!/usr/bin/env python3
"""Check distribution archives against this checkout and target dependency graph.

Native Desktop execution requires the explicit --run-pair-check option.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import plistlib
import re
import stat
import subprocess
import sys
import shutil
import tempfile
import tarfile
import unicodedata
import zipfile

MAX_ENTRIES = 10000
MAX_FILE = 512 * 1024 * 1024
MAX_TOTAL = 2 * 1024 * 1024 * 1024
NOTICE_STARTS = ("LICENSE", "LICENCE", "COPYING", "COPYRIGHT", "NOTICE", "UNLICENSE")
SOURCE_SUFFIXES = (".rs", ".c", ".h", ".py", ".js", ".ts", ".toml", ".yml", ".yaml")

class Failure(Exception):
    pass

def require(ok, token):
    if not ok:
        raise Failure(token)

def sha(data):
    return hashlib.sha256(data).hexdigest()

def unique(pairs):
    result = {}
    for key, value in pairs:
        require(key not in result, "duplicate-json-member")
        result[key] = value
    return result

def loads(data):
    return json.loads(data.decode("utf-8") if isinstance(data, bytes) else data, object_pairs_hook=unique)

def normalized(data):
    return data.replace(b"\r\n", b"\n")

def command(argv, cwd=None, env=None):
    result = subprocess.run(argv, cwd=cwd, env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=120)
    require(result.returncode == 0, "source-or-metadata-command-failed")
    return result.stdout

class Source:
    def __init__(self, path, head=None):
        self.path = Path(path).resolve()
        self.head = command(["git", "rev-parse", "--verify", (head or "HEAD") + "^{commit}"], self.path).decode().strip()
        self.tree = command(["git", "rev-parse", self.head + "^{tree}"], self.path).decode().strip()
        self.paths = set(command(["git", "ls-tree", "-r", "--name-only", self.head], self.path).decode().splitlines())
    def read(self, path):
        require(path in self.paths, "source-file-missing:" + path)
        return command(["git", "show", self.head + ":" + path], self.path)

def target_info(target):
    if target == "x86_64-unknown-linux-gnu": return "linux", "x86_64"
    if target == "aarch64-unknown-linux-gnu": return "linux", "aarch64"
    if target == "aarch64-apple-darwin": return "macos", "aarch64"
    if target == "x86_64-apple-darwin": return "macos", "x86_64"
    if target == "x86_64-pc-windows-msvc": return "windows", "x86_64"
    if target == "aarch64-pc-windows-msvc": return "windows", "aarch64"
    raise Failure("unsupported-target")

def safe_name(name):
    require(isinstance(name, str) and name and "\x00" not in name and "\\" not in name and ":" not in name, "archive-path-invalid")
    require(not name.startswith("/") and not name.endswith("//"), "archive-path-invalid")
    clean = name[:-1] if name.endswith("/") else name
    parts = clean.split("/")
    require(all(p and p not in (".", "..") and p == p.rstrip(" .") for p in parts), "archive-path-invalid")
    require(all(not re.fullmatch(r"(?i)(CON|PRN|AUX|NUL|COM[1-9]|LPT[1-9])(?:\..*)?", p) for p in parts), "archive-path-device")
    return clean

def is_notice(path):
    return path.is_file() and path.name.upper().startswith(NOTICE_STARTS) and path.suffix.lower() not in SOURCE_SUFFIXES

def dependency_graph(metadata, roots):
    packages = {p["id"]: p for p in metadata["packages"]}
    nodes = {n["id"]: n for n in metadata["resolve"]["nodes"]}
    require(len(packages) == len(metadata["packages"]) and len(nodes) == len(metadata["resolve"]["nodes"]), "metadata-duplicate-id")
    initial = [p["id"] for p in packages.values() if p["name"] in roots]
    require({packages[p]["name"] for p in initial} == set(roots), "metadata-root-missing")
    reached = set()
    todo = list(initial)
    while todo:
        identity = todo.pop()
        if identity in reached: continue
        require(identity in nodes and identity in packages, "metadata-node-missing")
        reached.add(identity)
        for edge in nodes[identity]["deps"]:
            if any(k["kind"] != "dev" for k in edge["dep_kinds"]): todo.append(edge["pkg"])
    return [packages[p] for p in sorted(reached)]

def build_expectation(source, kind, target, metadata=None):
    system, _ = target_info(target)
    files = {}
    def add(rel, data, exact=False):
        require(rel not in files, "expected-file-collision:" + rel)
        data = data if exact else normalized(data)
        files[rel] = {"sha256": sha(data), "bytes": len(data), "comparison": "raw" if exact else "lf"}
    docs = sorted(p for p in source.paths if p.startswith("docs/") and (kind == "desktop" or (p.count("/") == 1 and p.endswith(".md"))))
    for p in ["README.md", "LICENSE"] + docs: add(p, source.read(p))
    expected = {"schema_version": 1, "kind": kind, "target": target, "source_head": source.head, "source_tree": source.tree, "files": files}
    require(metadata is not None, "metadata-required")
    roots = ("drmr", "drmr-desktop") if kind == "desktop" else ("drmr",)
    packages = dependency_graph(metadata, roots)
    manifest = loads(source.read("tools/desktop-licenses/manifest.json"))
    require(manifest["schema_version"] == 1, "notice-manifest-invalid")
    inventory = []
    for p in packages:
        name = p["name"] + "-" + p["version"]
        home = Path(p["manifest_path"]).parent
        retained, licenses = {}, set()
        if p["name"].startswith("drmr"):
            retained["LICENSE"] = source.read("LICENSE")
            licenses.add("LICENSE")
        else:
            require(home.is_dir(), "registry-source-unavailable:" + name)
            for f in home.rglob("*"):
                if is_notice(f):
                    retained[f.relative_to(home).as_posix()] = f.read_bytes()
                    if f.parent == home and f.name.upper().startswith(("LICENSE", "LICENCE", "COPYING", "UNLICENSE")): licenses.add(f.name)
            if p.get("license_file"):
                path = home / p["license_file"]
                require(path.is_file() and path.resolve().is_relative_to(home.resolve()), "package-license-file-invalid")
                retained[path.relative_to(home).as_posix()] = path.read_bytes()
                licenses.add(p["license_file"])
        provenance = None
        if not licenses:
            candidates = [e for e in manifest["packages"] if (e["name"], e["version"]) == (p["name"], p["version"])]
            require(len(candidates) == 1, "notice-fallback-missing:" + name)
            entry = candidates[0]
            vcs = loads((home / ".cargo_vcs_info.json").read_bytes())
            require(entry["license"] == p.get("license") and entry["repository"] == p.get("repository") and entry["revision"] == vcs["git"]["sha1"], "notice-provenance-mismatch:" + name)
            require(entry["selected_license"] in entry["license"].split(" OR ") and entry["notices"], "notice-selected-license-invalid")
            for n in entry["notices"]:
                data = source.read("tools/desktop-licenses/" + n["file"])
                require(sha(data) == n["sha256"], "pinned-notice-source-checksum")
                filename = PurePosixPath(n["file"]).name
                require(filename not in retained, "notice-filename-collision")
                retained[filename] = data
            provenance = {k: entry[k] for k in ("selected_license", "repository", "revision", "notices")}
        require(retained, "notice-text-missing:" + name)
        row = {"name": p["name"], "version": p["version"], "license": p.get("license"), "authors": p["authors"], "repository": p.get("repository"), "notices": sorted(retained)}
        if (p.get("source") or "").startswith("registry+"): row["source_archive"] = "https://static.crates.io/crates/" + p["name"] + "/" + name + ".crate"
        if "MPL-2.0" in (p.get("license") or ""):
            require("source_archive" in row, "mpl-source-offer-missing")
            offer = "The complete, unmodified source for " + name + " is available at:\n" + row["source_archive"] + "\n\nLicense declaration: " + p["license"] + "\nMozilla Public License 2.0: https://mozilla.org/MPL/2.0/\n"
            if provenance: offer += "Publisher revision: " + provenance["revision"] + "\n"
            add("licenses/" + name + "/SOURCE.txt", offer.encode())
            row["source_availability_notice"] = "SOURCE.txt"
        row["notice_status"] = "publisher-pinned-notices" if provenance else "publisher-notices-retained"
        if provenance: row["pinned_provenance"] = provenance
        inventory.append(row)
        for n, data in retained.items():
            add("licenses/" + name + "/" + n, data, exact=not p["name"].startswith("drmr"))
    web = "crates/drmr-server/static/"
    for p in sorted(source.paths):
        if p.startswith(web) and PurePosixPath(p).name.upper().startswith(("LICENSE", "NOTICE", "COPYRIGHT", "OFL")):
            add("licenses/web-assets/" + p[len(web):], source.read(p))
    for n in ("COPYRIGHT", "LICENSE-MIT", "LICENSE-APACHE"): add("licenses/process-wrap/" + n, source.read("vendor/process-wrap/" + n))
    expected["inventory"] = inventory
    expected["package_count"] = len(packages)
    expected["metadata_sha256"] = sha(json.dumps(metadata, sort_keys=True).encode())
    if kind == "desktop":
        versions = {p["version"] for p in packages if p["name"] == "drmr-desktop"}
        require(len(versions) == 1, "desktop-version-ambiguous")
        expected["desktop_version"] = versions.pop()
        if system == "macos":
            info = plistlib.loads(source.read("crates/drmr-desktop/Info.plist"))
            info.update({"CFBundleIdentifier": "io.github.kazuhide.drmr", "CFBundleExecutable": "drmr-desktop", "CFBundleName": "drmr", "CFBundlePackageType": "APPL", "CFBundleIconFile": "icon.icns", "CFBundleVersion": expected["desktop_version"], "CFBundleShortVersionString": expected["desktop_version"], "LSMinimumSystemVersion": "11.0", "NSHighResolutionCapable": True})
            expected["info_plist"] = info
            expected["icon"] = {"sha256": sha(source.read("crates/drmr-desktop/icons/icon.icns"))}
    return expected

class Archive:
    def __init__(self, path):
        self.path = Path(path)
        require(self.path.stat().st_size <= MAX_TOTAL, "archive-compressed-limit")
        self.zip = zipfile.is_zipfile(self.path)
        if self.zip:
            # Bound the central directory before ZipFile materializes its entries.
            with self.path.open("rb") as stream:
                stream.seek(max(0, self.path.stat().st_size - 65557))
                tail = stream.read()
            end = tail.rfind(b"PK\x05\x06")
            require(end >= 0 and len(tail) >= end + 22, "zip-end-record-invalid")
            record = tail[end:end + 22]
            require(end + 22 + int.from_bytes(record[20:22], "little") == len(tail), "zip-trailing-data")
            entries = int.from_bytes(record[10:12], "little")
            require(record[4:8] == b"\0" * 4 and int.from_bytes(record[8:10], "little") == entries, "zip-multipart-refused")
            require(entries <= MAX_ENTRIES and entries != 65535 and int.from_bytes(record[12:16], "little") <= 8 * 1024 * 1024, "zip-directory-limit-or-zip64")
        self.handle = zipfile.ZipFile(self.path) if self.zip else tarfile.open(self.path, "r:gz")
        try:
            self.files, self.directories = {}, set()
            seen, total = set(), 0
            items = self.handle.infolist() if self.zip else self.handle
            for index, item in enumerate(items):
                require(index < MAX_ENTRIES, "archive-entry-limit")
                name = safe_name(item.filename if self.zip else item.name)
                require(name not in seen, "archive-duplicate-path")
                seen.add(name)
                directory = item.is_dir() if self.zip else item.isdir()
                mode = (item.external_attr >> 16) if self.zip else item.mode
                if self.zip:
                    require(not item.flag_bits & 1, "archive-encrypted-member")
                    require(stat.S_IFMT(mode) in ((0, stat.S_IFDIR) if directory else (0, stat.S_IFREG)), "archive-link-special-or-type-mismatch")
                else: require(item.isdir() or item.isreg(), "archive-link-or-special-member")
                if directory:
                    self.directories.add(name)
                else:
                    size = item.file_size if self.zip else item.size
                    require(0 <= size <= MAX_FILE, "archive-member-limit")
                    total += size
                    require(total <= MAX_TOTAL, "archive-total-limit")
                    self.files[name] = (item, size, mode)
            for name in self.files:
                require(not any(parent.as_posix() in self.files for parent in PurePosixPath(name).parents if str(parent) != "."), "archive-file-directory-conflict")
        except BaseException:
            self.handle.close()
            raise
    def close(self): self.handle.close()
    def open(self, name):
        item = self.files[name][0]
        return self.handle.open(item) if self.zip else self.handle.extractfile(item)
    def data(self, name, maximum=16 * 1024 * 1024):
        require(self.files[name][1] <= maximum, "archive-data-limit")
        with self.open(name) as stream: data = stream.read(maximum + 1)
        require(len(data) == self.files[name][1], "archive-size-mismatch")
        return data
    def digest(self, name):
        digest, size = hashlib.sha256(), 0
        with self.open(name) as stream:
            for chunk in iter(lambda: stream.read(65536), b""):
                digest.update(chunk)
                size += len(chunk)
        require(size == self.files[name][1], "archive-size-mismatch")
        return digest.hexdigest()


def binary(archive, name, target):
    system, arch = target_info(target)
    with archive.open(name) as stream:
        header = stream.read(4096)
        require(len(header) >= 64, "binary-header-truncated")
        if system == "windows":
            require(header[:2] == b"MZ", "binary-not-pe")
            offset = int.from_bytes(header[60:64], "little")
            require(64 <= offset <= 1024 * 1024, "binary-pe-offset")
            stream.seek(offset); pe = stream.read(26)
            require(len(pe) == 26 and pe[:4] == b"PE\0\0", "binary-pe-signature")
            require(int.from_bytes(pe[4:6], "little") == (0x8664 if arch == "x86_64" else 0xaa64), "binary-architecture")
            require(int.from_bytes(pe[24:26], "little") == 0x20b, "binary-not-pe64")
        elif system == "linux":
            require(header[:7] == b"\x7fELF\x02\x01\x01" and int.from_bytes(header[16:18], "little") in (2, 3), "binary-not-elf64")
            require(int.from_bytes(header[18:20], "little") == (62 if arch == "x86_64" else 183), "binary-architecture")
        else:
            require(header[:4] == b"\xcf\xfa\xed\xfe", "binary-not-thin-macho64")
            require(int.from_bytes(header[4:8], "little") == (0x0100000c if arch == "aarch64" else 0x01000007), "binary-architecture")
            require(int.from_bytes(header[12:16], "little") == 2, "binary-not-executable")
    if system != "windows":
        mode = archive.files[name][2]
        require(mode & 0o500 == 0o500 and not mode & 0o7022, "binary-executable-permissions")
    return {"sha256": archive.digest(name), "bytes": archive.files[name][1], "mode": oct(archive.files[name][2] & 0o777), "header_target": target}

def layout(archive, kind, target):
    system, _ = target_info(target)
    if kind == "desktop":
        prefix = "drmr.app/" if system == "macos" else "drmr-desktop/"
        resources = prefix + "Contents/Resources/" if system == "macos" else prefix
        binary_root = prefix + "Contents/MacOS/" if system == "macos" else prefix
    else:
        name = "drmr.exe" if system == "windows" else "drmr"
        roots = {path.split("/")[0] for path in archive.files}
        prefix = "" if name in archive.files else next(iter(roots)) + "/" if len(roots) == 1 else None
        require(prefix is not None, "archive-root-ambiguous")
        resources = binary_root = prefix
    suffix = ".exe" if system == "windows" else ""
    binaries = {name: binary_root + name + suffix for name in (["drmr", "drmr-desktop"] if kind == "desktop" else ["drmr"])}
    return prefix, resources, binaries


def check_manifest(value, source, target, binaries):
    require(type(value) is dict and set(value) == {"schema_version", "source_head", "source_tree", "target", "rustc", "binaries"}, "build-manifest-schema")
    require(type(value["schema_version"]) is int and value["schema_version"] == 1, "build-manifest-version")
    require(value["source_head"] == source.head and value["source_tree"] == source.tree and value["target"] == target, "build-manifest-source-target-mismatch")
    require(type(value["rustc"]) is str and value["rustc"].startswith("rustc ") and len(value["rustc"]) < 512, "build-manifest-compiler")
    require(value["binaries"] == {name: row["sha256"] for name, row in binaries.items()}, "build-manifest-binary-mismatch")


def run_pair_check(archive, paths, target):
    # Execute only by explicit request after every archive and source check passes.
    require(command(["rustc", "-vV"]).decode().split("host: ")[1].splitlines()[0] == target, "pair-check-requires-native-target")
    with tempfile.TemporaryDirectory(prefix="drmr-bundle-check-") as directory:
        root = Path(directory)
        for name in paths.values():
            destination = root.joinpath(*PurePosixPath(name).parts)
            destination.parent.mkdir(parents=True, exist_ok=True)
            with archive.open(name) as original, destination.open("xb") as output:
                shutil.copyfileobj(original, output, 65536)
            destination.chmod(0o700)
        host = root.joinpath(*PurePosixPath(paths["drmr-desktop"]).parts)
        result = subprocess.run([str(host), "--verify-bundle"], stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=20)
        require(result.returncode == 0 and result.stdout.strip() == b"bundle-cli-verified", "desktop-runtime-pair-mismatch")
        # The real host must refuse a changed CLI. This verifies the gate is active.
        cli = root.joinpath(*PurePosixPath(paths["drmr"]).parts)
        with cli.open("ab") as stream: stream.write(b"invalid-pair")
        result = subprocess.run([str(host), "--verify-bundle"], stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=20)
        require(result.returncode == 1 and b"bundled-cli-mismatch" in result.stderr, "desktop-runtime-pair-negative-failed")
    return "native-compiled-expectation-and-wrong-pair-refusal"


def qualify(path, source, expected, run_pair=False):
    require(expected.get("schema_version") == 1 and expected["source_head"] == source.head and expected["source_tree"] == source.tree, "expectation-source-mismatch")
    kind, target = expected["kind"], expected["target"]
    require(kind in ("cli", "desktop"), "expectation-kind")
    system, _ = target_info(target)
    archive = Archive(path)
    try:
        prefix, resources, paths = layout(archive, kind, target)
        wanted = {resources + name for name in expected["files"]} | set(paths.values())
        wanted.update({resources + "licenses/inventory.json", resources + "build-manifest.json"})
        if kind == "desktop" and system == "macos": wanted.update({prefix + "Contents/Info.plist", resources + "icon.icns"})
        require(set(archive.files) == wanted, "archive-file-set-mismatch")
        allowed_directories = {str(parent) for name in wanted for parent in PurePosixPath(name).parents if str(parent) != "."}
        require(archive.directories <= allowed_directories, "archive-extra-directory")
        if system in ("windows", "macos"):
            aliases = [unicodedata.normalize("NFC", name).casefold() for name in wanted | archive.directories]
            require(len(aliases) == len(set(aliases)), "archive-filesystem-alias")
        for name, specification in expected["files"].items():
            data = archive.data(resources + name)
            if specification["comparison"] == "lf": data = normalized(data)
            require(len(data) == specification["bytes"] and sha(data) == specification["sha256"], "archive-source-bytes-mismatch:" + name)
        inventory = loads(archive.data(resources + "licenses/inventory.json"))
        require(inventory == expected["inventory"], "archive-dependency-inventory-mismatch")
        binaries = {name: binary(archive, location, target) for name, location in paths.items()}
        check_manifest(loads(archive.data(resources + "build-manifest.json")), source, target, binaries)
        if kind == "desktop" and system == "macos":
            require(plistlib.loads(archive.data(prefix + "Contents/Info.plist")) == expected["info_plist"], "desktop-info-plist-mismatch")
            require(sha(archive.data(resources + "icon.icns")) == expected["icon"]["sha256"], "desktop-icon-mismatch")
        pair = run_pair_check(archive, paths, target) if kind == "desktop" and run_pair else "not-executed" if kind == "desktop" else "not-applicable"
        return {"success": kind != "desktop" or run_pair, "source_head": source.head, "source_tree": source.tree,
                "target": target, "kind": kind, "archive_sha256": file_digest(path), "files": len(wanted),
                "packages": expected["package_count"], "binaries": binaries, "pair_check": pair,
                "raw_notice_files": sum(row["comparison"] == "raw" for row in expected["files"].values()),
                "limits": "Archive/source and optional native CLI-pair checks only; no GUI, media, signature, build-origin or legal certification."}
    finally:
        archive.close()


def file_digest(path):
    digest = hashlib.sha256()
    with Path(path).open("rb") as stream:
        for chunk in iter(lambda: stream.read(65536), b""): digest.update(chunk)
    return digest.hexdigest()


def metadata(source, kind, target):
    require(command(["git", "rev-parse", "HEAD"], source.path).decode().strip() == source.head, "metadata-requires-current-head")
    require(not command(["git", "status", "--porcelain", "--untracked-files=no"], source.path).strip(), "metadata-requires-clean-source")
    argv = ["cargo", "metadata", "--format-version", "1", "--locked", "--offline", "--filter-platform", target]
    if kind == "desktop": argv.extend(["--features", "drmr-desktop/desktop"])
    return loads(command(argv, source.path))


def manifest(source, directory, kind, target):
    suffix = ".exe" if target_info(target)[0] == "windows" else ""
    binary_root = directory / "Contents/MacOS" if kind == "desktop" and target_info(target)[0] == "macos" else directory
    value = {"schema_version": 1, "source_head": source.head, "source_tree": source.tree, "target": target,
             "rustc": command(["rustc", "--version"]).decode().strip(),
             "binaries": {name: file_digest(binary_root / (name + suffix)) for name in (["drmr", "drmr-desktop"] if kind == "desktop" else ["drmr"])}}
    resources = directory / "Contents/Resources" if kind == "desktop" and target_info(target)[0] == "macos" else directory
    with (resources / "build-manifest.json").open("x", encoding="utf-8") as stream: json.dump(value, stream, indent=2); stream.write("\n")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("operation", choices=("manifest", "verify"))
    parser.add_argument("--source", type=Path, default=Path(__file__).resolve().parents[1])
    parser.add_argument("--kind", choices=("cli", "desktop"), required=True)
    parser.add_argument("--target", required=True)
    parser.add_argument("--directory", type=Path)
    parser.add_argument("--archive", type=Path)
    parser.add_argument("--report", type=Path)
    parser.add_argument("--run-pair-check", action="store_true", help="Execute the checked native Desktop binary and a wrong-pair negative; only use trusted local builds")
    args = parser.parse_args()
    source = Source(args.source)
    if args.operation == "manifest":
        require(args.directory is not None and args.archive is None and not args.run_pair_check, "manifest-arguments")
        manifest(source, args.directory, args.kind, args.target)
    else:
        require(args.archive is not None and args.report is not None and args.directory is None, "verify-arguments")
        require(not args.report.exists(), "report-destination-exists")
        expected = build_expectation(source, args.kind, args.target, metadata(source, args.kind, args.target))
        result = qualify(args.archive, source, expected, args.run_pair_check)
        with args.report.open("x", encoding="utf-8") as stream: json.dump(result, stream, indent=2); stream.write("\n")
        print(json.dumps(result))
        if not result["success"]: raise Failure("desktop-pair-check-not-executed")


if __name__ == "__main__":
    try:
        main()
    except Failure as error:
        print("archive: " + str(error), file=sys.stderr)
        sys.exit(1)
    except (OSError, ValueError, KeyError, TypeError, subprocess.SubprocessError):
        print("archive: input-or-tool-failed", file=sys.stderr)
        sys.exit(1)
