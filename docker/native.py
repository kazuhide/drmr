"""Build pinned native tools and retain their pristine sources and notices."""
import argparse
import hashlib
import json
from pathlib import Path, PurePosixPath
import shutil
import subprocess
import tarfile
import urllib.request

RECIPE = Path(__file__).resolve().parent
PREFIX = Path("/opt/drmr")
WORK = Path("/usr/src/media")

def run(*argv, cwd=None, capture=False):
    return subprocess.run(argv, cwd=cwd, check=True, text=True,
                          stdout=subprocess.PIPE if capture else None).stdout

def digest(path):
    value = hashlib.sha256()
    with path.open("rb") as stream:
        for block in iter(lambda: stream.read(65536), b""):
            value.update(block)
    return value.hexdigest()

def verify_file(path, expected):
    if digest(path) != expected:
        raise ValueError("source-checksum-mismatch: " + path.name)

def collect_notices(source, destination, license_files=None):
    """Retain source-wide notices; selected link targets are recorded separately."""
    files = []
    for path in sorted(source.rglob("*")):
        relative = path.relative_to(source)
        if any(part in (".git", "CMakeFiles") for part in relative.parts) or path.is_symlink():
            continue
        name = path.name.upper()
        if path.is_file() and (any(word in name for word in ("LICENSE", "LICENCE", "COPYING", "COPYRIGHT", "NOTICE"))
                               or "LICENSES" in (part.upper() for part in relative.parts)
                               or name in ("AUTHORS", "THANKS")):
            files.append(path)
    collected = {path.relative_to(source).as_posix() for path in files}
    if license_files is not None:
        primary = bool(license_files) and all(name in collected for name in license_files)
    else:
        primary = any(path.name.upper().startswith(("LICENSE", "LICENCE", "COPYING", "UNLICENSE")) for path in files)
    if not primary:
        raise ValueError("component-license-missing: " + source.name)
    records = []
    for path in files:
        relative = path.relative_to(source)
        target = destination / relative
        target.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(path, target)
        records.append({"path": relative.as_posix(), "sha256": digest(target), "bytes": target.stat().st_size})
    return records

def fetch_archive(pin):
    archive = PREFIX / "sources" / pin["archive"]
    archive.parent.mkdir(parents=True, exist_ok=True)
    with urllib.request.urlopen(pin["url"], timeout=60) as source, archive.open("xb") as target:
        shutil.copyfileobj(source, target)
    verify_file(archive, pin["sha256"])
    return archive

def unpack(archive, destination):
    destination.mkdir(parents=True)
    # Bookworm's Python 3.11.2 predates extraction filters. These pinned source
    # releases need only regular files and directories; reject every link/device.
    entries = set()
    total = 0
    with tarfile.open(archive) as source:
        for member in source:
            relative = PurePosixPath(member.name)
            if (relative.is_absolute() or ".." in relative.parts or not relative.parts
                    or ":" in relative.parts[0] or "\\" in member.name
                    or not (member.isfile() or member.isdir()) or member.sparse
                    or relative in entries):
                raise ValueError("source-archive-entry-invalid")
            entries.add(relative)
            total += member.size
            if len(entries) > 100000 or total > 512 * 1024 * 1024:
                raise ValueError("source-archive-size-limit")
            target = destination.joinpath(*relative.parts)
            if member.isdir():
                target.mkdir(parents=True, exist_ok=True)
            else:
                target.parent.mkdir(parents=True, exist_ok=True)
                with source.extractfile(member) as incoming, target.open("xb") as outgoing:
                    shutil.copyfileobj(incoming, outgoing)
                target.chmod(0o755 if member.mode & 0o111 else 0o644)
    entries = list(destination.iterdir())
    if len(entries) != 1 or not entries[0].is_dir():
        raise ValueError("source-archive-root-invalid")
    return entries[0]

def record(name, pin, source, extra=None):
    pinned = {notice["path"] for notice in pin.get("notices", [])}
    primary = pin.get("license_files")
    if primary is not None and (not primary or any(path not in pinned for path in primary)):
        raise ValueError("component-license-pin-missing: " + name)
    for notice in pin.get("notices", []):
        verify_file(source / notice["path"], notice["sha256"])
    notices = collect_notices(source, PREFIX / "licenses" / name, primary)
    result = {"name": name, "pin": pin, "notices": notices, **(extra or {})}
    (PREFIX / "metadata").mkdir(parents=True, exist_ok=True)
    (PREFIX / "metadata" / (name + ".json")).write_text(json.dumps(result, indent=2) + "\n")



def git_archive(source, output):
    with output.open("xb") as stream:
        subprocess.run(["git", "archive", "--format=tar.gz", "HEAD"], cwd=source, stdout=stream, check=True)
