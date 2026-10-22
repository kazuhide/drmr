"""Bind the final image's binaries, raw notices and source archives to its source revision."""
import argparse
import hashlib
import json
from pathlib import Path
import re
import shutil
import struct
import subprocess

ROOT = Path("/usr/share/drmr")

def digest(path):
    result = hashlib.sha256()
    with path.open("rb") as stream:
        for block in iter(lambda: stream.read(65536), b""):
            result.update(block)
    return result.hexdigest()

def inventory(directory):
    return [{"path": path.relative_to(directory).as_posix(), "bytes": path.stat().st_size, "sha256": digest(path)}
            for path in sorted(directory.rglob("*")) if path.is_file() and path != directory / "inventory.json"]

def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--revision", required=True)
    parser.add_argument("--tree", required=True)
    parser.add_argument("--architecture", choices=("arm64", "amd64"), required=True)
    args = parser.parse_args()
    if not all(re.fullmatch("[0-9a-f]{40}", value) for value in (args.revision, args.tree)):
        parser.error("revision and tree must be full lowercase Git identifiers")
    os_rows = []
    for line in (ROOT / "runtime-os.tsv").read_text().splitlines():
        name, version, architecture, source, source_version = line.split("\t")
        origin = ROOT / "licenses/os" / name.split(":")[0] / "copyright"
        if not origin.is_file():
            raise ValueError("runtime-package-copyright-missing: " + name)
        os_rows.append({"name": name, "version": version, "architecture": architecture, "source": source, "source_version": source_version})
    (ROOT / "licenses/os/packages.json").write_text(json.dumps(os_rows, indent=2) + "\n")
    binaries = {}
    for name in ("drmr", "ffmpeg", "ffprobe", "packager"):
        path = Path("/usr/local/bin") / name
        header = path.read_bytes()[:20]
        machine = struct.unpack("<H", header[18:20])[0]
        if header[:6] != b"\x7fELF\x02\x01" or machine != {"arm64": 183, "amd64": 62}[args.architecture]:
            raise ValueError("runtime-binary-architecture-mismatch: " + name)
        result = subprocess.run(["ldd", str(path)], capture_output=True, text=True, check=True)
        if "not found" in result.stdout:
            raise ValueError("runtime-shared-library-missing: " + name)
        binaries[name] = {"path": str(path), "sha256": digest(path), "shared_libraries": result.stdout.splitlines()}
    for name in ("licenses", "sources"):
        (ROOT / name / "inventory.json").write_text(json.dumps(inventory(ROOT / name), indent=2) + "\n")
    manifest = {"schema_version": 1, "source_commit": args.revision, "source_tree": args.tree,
                "target_architecture": args.architecture, "binaries": binaries,
                "tool_sources": json.loads((ROOT / "sources/source-pins.json").read_text()),
                "licenses_inventory": "licenses/inventory.json", "sources_inventory": "sources/inventory.json",
                "os_packages": os_rows, "rust": json.loads((ROOT / "metadata/rust.json").read_text()),
                "qualification": "Build identity and retained source/notices; media qualification is a separate recorded check"}
    (ROOT / "build-manifest.json").write_text(json.dumps(manifest, indent=2) + "\n")

if __name__ == "__main__":
    main()
