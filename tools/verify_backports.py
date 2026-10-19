#!/usr/bin/env python3
"""Verify the complete pinned publisher source and the exact reviewed GLib backport."""
import argparse
import hashlib
import json
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]


class Failure(Exception):
    pass


def verify(root=ROOT):
    root = Path(root)
    pin = json.loads((root / "tools/backports/glib-0.18.5.json").read_text(encoding="utf-8"))
    home = root / "vendor/glib"
    paths = {p.relative_to(home).as_posix() for p in home.rglob("*") if p.is_file()}
    if paths != set(pin["original_files"]) or any(p.is_symlink() for p in home.rglob("*")):
        raise Failure("backport-file-set")
    for name, digest in pin["original_files"].items():
        data = (home / name).read_bytes()
        if name == pin["changed_file"]:
            if hashlib.sha256(data).hexdigest() != pin["patched_sha256"]:
                raise Failure("backport-patched-source")
            if data.count(b"let mut p: *mut libc::c_char = std::ptr::null_mut();") != 1 or data.count(b"                &mut p,") != 1:
                raise Failure("backport-change-shape")
            data = data.replace(b"let mut p: *mut libc::c_char = std::ptr::null_mut();", b"let p: *mut libc::c_char = std::ptr::null_mut();", 1)
            data = data.replace(b"                &mut p,", b"                &p,", 1)
        if hashlib.sha256(data).hexdigest() != digest:
            raise Failure("backport-publisher-source")
    return {"package": pin["package"], "version": pin["version"], "files": len(paths),
            "source_archive_sha256": pin["source_sha256"], "patched_source_sha256": pin["patched_sha256"],
            "upstream_revision": pin["upstream_revision"], "advisory": pin["advisory"]}


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--source", type=Path, default=ROOT)
    args = parser.parse_args()
    try:
        print(json.dumps(verify(args.source), sort_keys=True))
    except (Failure, OSError, ValueError, KeyError):
        parser.exit(1, "backport: source-verification-failed\n")
