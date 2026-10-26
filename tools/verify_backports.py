#!/usr/bin/env python3
"""Verify the complete pinned publisher source and the exact reviewed GLib backport."""
import argparse
import hashlib
import json
from pathlib import Path
import re

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


def verify_private_fs(root=ROOT):
    root = Path(root)
    def unique(pairs):
        result = {}
        for key, value in pairs:
            if key in result:
                raise Failure("native-patch-identity")
            result[key] = value
        return result
    pin = json.loads((root / "tools/backports/private-fs-0.2.1.json").read_text(encoding="utf-8"),
                     object_pairs_hook=unique)
    if (type(pin) is not dict or set(pin) != {"schema_version", "package", "version", "source_archive",
                                          "source_sha256", "upstream_revision", "original_files", "patches"}
            or type(pin.get("schema_version")) is not int or pin["schema_version"] != 1
            or pin.get("package") != "private-fs"
            or pin.get("version") != "0.2.1"
            or pin.get("source_archive") != "https://static.crates.io/crates/private-fs/private-fs-0.2.1.crate"
            or type(pin.get("source_sha256")) is not str
            or re.fullmatch(r"[0-9a-f]{64}", pin.get("source_sha256", "")) is None
            or type(pin.get("upstream_revision")) is not str
            or re.fullmatch(r"[0-9a-f]{40}", pin.get("upstream_revision", "")) is None):
        raise Failure("native-patch-identity")
    if (type(pin["original_files"]) is not dict
            or any(type(name) is not str or type(digest) is not str
                   or re.fullmatch(r"[0-9a-f]{64}", digest) is None
                   for name, digest in pin["original_files"].items())):
        raise Failure("native-patch-identity")
    home = root / "vendor/private-fs"
    entries = list(home.rglob("*"))
    paths = {p.relative_to(home).as_posix() for p in entries if p.is_file()}
    if paths != set(pin["original_files"]) or any(p.is_symlink() for p in entries):
        raise Failure("native-patch-file-set")
    patches = pin["patches"]
    if type(patches) is not dict or set(patches) != {"src/lib.rs", "src/windows/mod.rs", "src/windows/nt.rs"}:
        raise Failure("native-patch-shape")
    for name, digest in pin["original_files"].items():
        data = (home / name).read_bytes()
        if name in patches:
            patch = patches[name]
            if (type(patch) is not dict or set(patch) != {"patched_sha256", "replacements"}
                    or type(patch["patched_sha256"]) is not str
                    or re.fullmatch(r"[0-9a-f]{64}", patch["patched_sha256"]) is None
                    or type(patch["replacements"]) is not list or len(patch["replacements"]) != 1):
                raise Failure("native-patch-shape")
            if hashlib.sha256(data).hexdigest() != patch["patched_sha256"]:
                raise Failure("native-patch-modified-source")
            change = patch["replacements"][0]
            if (type(change) is not dict or set(change) != {"before", "after"}
                    or any(type(value) is not str for value in change.values())):
                raise Failure("native-patch-shape")
            before, after = change["before"].encode("utf-8"), change["after"].encode("utf-8")
            if (not after or before == after or data.count(after) != 1
                    or len(re.findall(rb"\bunsafe\b", after)) > len(re.findall(rb"\bunsafe\b", before))):
                raise Failure("native-patch-shape")
            data = data.replace(after, before, 1)
        if hashlib.sha256(data).hexdigest() != digest:
            raise Failure("native-patch-publisher-source")
    return {"package": pin["package"], "version": pin["version"], "files": len(paths),
            "patched_files": len(patches), "source_archive_sha256": pin["source_sha256"],
            "upstream_revision": pin["upstream_revision"]}


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--source", type=Path, default=ROOT)
    args = parser.parse_args()
    try:
        report = verify(args.source)
        report["native_handle_patch"] = verify_private_fs(args.source)
        print(json.dumps(report, sort_keys=True))
    except (Failure, OSError, ValueError, KeyError, TypeError):
        parser.exit(1, "backport: source-verification-failed\n")
