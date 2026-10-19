"""Fence one owned Cargo target cache by actual compilation inputs, not mtimes."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import stat
import subprocess
import tempfile

MARKER = ".drmr-compile-inputs.json"
MAX_FILES = 100000
MAX_BYTES = 2 * 1024 * 1024 * 1024


def compile_inputs(source, toolchain):
    value = hashlib.sha256(b"drmr-cargo-inputs-v1\0" + toolchain.encode("utf-8"))
    files = []
    pending = [source / "Cargo.toml", source / "Cargo.lock", source / "crates", source / "vendor"]
    entries = 0
    while pending:
        path = pending.pop()
        entries += 1
        if entries > MAX_FILES:
            raise ValueError("compile-input-size-limit")
        metadata = path.lstat()
        if stat.S_ISLNK(metadata.st_mode):
            raise ValueError("compile-input-link-refused")
        if stat.S_ISDIR(metadata.st_mode):
            with os.scandir(path) as directory:
                for child in directory:
                    pending.append(Path(child.path))
                    if len(pending) + entries > MAX_FILES:
                        raise ValueError("compile-input-size-limit")
        elif stat.S_ISREG(metadata.st_mode):
            files.append((path, metadata))
        else:
            raise ValueError("compile-input-kind-refused")
    total = 0
    for path, metadata in sorted(files):
        relative = path.relative_to(source).as_posix().encode("utf-8")
        total += metadata.st_size
        if total > MAX_BYTES:
            raise ValueError("compile-input-size-limit")
        value.update(len(relative).to_bytes(8, "big") + relative)
        value.update(stat.S_IMODE(metadata.st_mode).to_bytes(4, "big"))
        value.update(metadata.st_size.to_bytes(8, "big"))
        with path.open("rb") as stream:
            for block in iter(lambda: stream.read(65536), b""):
                value.update(block)
    return {"schema_version": 1, "inputs_sha256": value.hexdigest(), "files": len(files), "bytes": total}


def prepare(source, target, toolchain):
    source, target = Path(source), Path(target)
    if (not source.is_absolute() or ".." in source.parts or source.is_symlink()
            or target != source / "target" or target.is_symlink()
            or not source.is_dir() or not target.is_dir()
            or target.parent.resolve() != source.resolve()
            or not shutil.rmtree.avoids_symlink_attacks):
        raise ValueError("owned-target-cache-required")
    expected = compile_inputs(source, toolchain)
    try:
        marker = target / MARKER
        previous = (json.loads(marker.read_text())
                    if not marker.is_symlink() and marker.is_file() and marker.stat().st_size <= 1024 else None)
    except (FileNotFoundError, ValueError, OSError):
        previous = None
    changed = previous != expected
    if changed:
        # BuildKit sharing=locked excludes concurrent Cargo writers. Never remove
        # the cache mount itself or follow links from its owned children.
        for child in target.iterdir():
            if child.is_dir() and not child.is_symlink():
                shutil.rmtree(child)
            else:
                child.unlink()
        with tempfile.NamedTemporaryFile(mode="w", dir=target, prefix=MARKER, delete=False) as stream:
            temporary = Path(stream.name)
            json.dump(expected, stream, sort_keys=True)
            stream.write("\n")
        temporary.replace(target / MARKER)
    return {**expected, "target_reset": changed}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--source", type=Path, required=True)
    parser.add_argument("--target", type=Path, required=True)
    parser.add_argument("--report", type=Path)
    arguments = parser.parse_args()
    toolchain = subprocess.check_output(["rustc", "-vV"], text=True)
    result = prepare(arguments.source, arguments.target, toolchain)
    if arguments.report:
        arguments.report.parent.mkdir(parents=True, exist_ok=True)
        arguments.report.write_text(json.dumps(result, indent=2) + "\n")
    print(json.dumps(result, sort_keys=True))


if __name__ == "__main__":
    main()
