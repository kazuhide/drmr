#!/usr/bin/env python3
"""Run two prebuilt Linux codec fuzzers and publish only bounded typed evidence.

The workflow builds instrumented binaries separately. This command never builds
the CLI, changes a corpus in the source tree, or forwards child diagnostics.
"""
import argparse
from contextlib import contextmanager
import hashlib
import json
import os
from pathlib import Path
import re
import signal
import stat
import sys
import threading
import time

import fuzz_contract as CONTRACT

SUPPORT = CONTRACT.module("qualify_cli_archive")
CHECK = CONTRACT.module("check_release")
ROOT = Path(__file__).resolve().parents[1]
# ASan debug executables are about 67 MiB on the measured native build. Keep
# a fixed 256 MiB admission cap while hashing only 64 KiB at a time.
MAX_BINARY_BYTES = 256 * 1024 * 1024


@contextmanager
def cancellation():
    event = threading.Event()
    previous = {}
    try:
        for kind in (signal.SIGINT, signal.SIGTERM):
            previous[kind] = signal.signal(kind, lambda *_: event.set())
        yield event
    finally:
        for kind, handler in previous.items():
            signal.signal(kind, handler)


def checkpoint(event):
    CONTRACT.require(not event.is_set(), "fuzz-cancelled")


def check_source(source, identity):
    current = CONTRACT.VERIFY.Source(source.path)
    CONTRACT.require((current.head, current.tree) == (source.head, source.tree)
                     and not CONTRACT.VERIFY.command(["git", "status", "--porcelain=v1", "--untracked-files=all"], cwd=source.path)
                     and CONTRACT.identity(current) == identity, "fuzz-source-changed")


def statistics(raw, target, elapsed):
    CONTRACT.require(type(raw) is bytes and len(raw) <= SUPPORT.MAX_OUTPUT, "fuzz-statistics-invalid")
    text = raw.decode("ascii", errors="replace")
    admission = re.findall(r"^DRMR_FUZZ_CORPUS target=([a-z0-9]+) accepted=([0-9]{1,12}) refused=([0-9]{1,12})$", text, re.M)
    done = re.findall(r"^#([0-9]{1,12})[ \t]+DONE[ \t]+cov:[ \t]*([0-9]{1,12})\b[^\r\n]*$", text, re.M)
    summary = re.findall(r"^Done ([0-9]{1,12}) runs in ([0-9]{1,12}) second\(s\)$", text, re.M)
    units = re.findall(r"^stat::number_of_executed_units:[ \t]*([0-9]{1,12})$", text, re.M)
    rss = re.findall(r"^stat::peak_rss_mb:[ \t]*([0-9]{1,12})$", text, re.M)
    expected = [(name, str(accepted), str(refused)) for name, accepted, refused in CONTRACT.TARGETS if name == target]
    CONTRACT.require(len(expected) == 1 and admission == expected and len(done) == len(summary) == len(units) == len(rss) == 1,
                     "fuzz-statistics-invalid")
    executions, coverage = map(int, done[0])
    seconds = int(summary[0][1])
    CONTRACT.require(executions == int(summary[0][0]) == int(units[0]) and executions > sum(map(int, expected[0][1:]))
                     and coverage > 0 and type(elapsed) is int and 60000 <= elapsed <= 90000
                     and 60 <= seconds <= 90 and abs(elapsed - seconds * 1000) <= 3000
                     and 0 < int(rss[0]) <= CONTRACT.BOUNDS["rss_mib"],
                     "fuzz-statistics-invalid")
    return {"seed_admission": {"accepted": int(expected[0][1]), "refused": int(expected[0][2])},
            "executions": executions, "coverage": coverage, "elapsed_milliseconds": elapsed, "fuzzer_seconds": seconds,
            "peak_rss_mib": int(rss[0])}


def binary_digest(binary):
    """Retain read_regular's identity checks without buffering an executable."""
    binary = Path(binary)
    flags = os.O_RDONLY | getattr(os, "O_NONBLOCK", 0) | getattr(os, "O_NOFOLLOW", 0)
    try:
        CONTRACT.require(not binary.is_symlink(), "fuzz-binary-invalid")
        with os.fdopen(os.open(binary, flags), "rb") as stream:
            before = os.fstat(stream.fileno())
            CONTRACT.require(stat.S_ISREG(before.st_mode) and 0 < before.st_size <= MAX_BINARY_BYTES
                             and not getattr(before, "st_file_attributes", 0) & 0x400, "fuzz-binary-invalid")
            digest, size = hashlib.sha256(), 0
            while True:
                block = stream.read(min(65536, before.st_size + 1 - size))
                if not block:
                    break
                size += len(block)
                CONTRACT.require(size <= before.st_size, "fuzz-binary-changed")
                digest.update(block)
            after, current = os.fstat(stream.fileno()), binary.lstat()
            def identity(value):
                return value.st_dev, value.st_ino, value.st_size, value.st_mtime_ns, value.st_ctime_ns
            CONTRACT.require(size == before.st_size and identity(before) == identity(after) == identity(current)
                             and stat.S_ISREG(current.st_mode) and not binary.is_symlink(), "fuzz-binary-changed")
        return digest.hexdigest()
    except OSError:
        raise CONTRACT.Failure("fuzz-binary-invalid") from None


def run_target(runner, binary, target, corpus):
    before = binary_digest(binary)
    # No -jobs/-workers: libFuzzer's default is one in-process campaign. Those
    # switches would introduce a separate supervisor and worker process.
    arguments = ["/bin/sh", "-c", 'exec "$@" 2>&1', "drmr-fuzz", str(binary), str(corpus),
                 "-max_total_time=60", "-timeout=2", "-rss_limit_mb=512", "-malloc_limit_mb=128",
                 "-max_len=65536", "-seed=1", "-print_final_stats=1", "-artifact_prefix=" + str(runner.workspace) + "/"]
    started = time.monotonic()
    raw = runner.run(arguments, "fuzz-" + target, timeout=90)
    elapsed = round((time.monotonic() - started) * 1000)
    CONTRACT.require(binary_digest(binary) == before, "fuzz-binary-changed")
    return {"target": target, "binary_sha256": before, **statistics(raw, target, elapsed),
            "exit_code": 0, "cancelled": False, "timed_out": False}


def preflight(runner, source, identity, database, cargo_audit):
    nightly = CONTRACT.TOOLCHAIN["nightly"]
    compiler = runner.run(["rustup", "run", nightly, "rustc", "--version"], "fuzz-compiler", 30)
    CONTRACT.require(compiler.startswith(b"rustc ") and b"-nightly " in compiler, "fuzz-toolchain-mismatch")
    version = runner.run(["cargo", "+" + nightly, "fuzz", "--version"], "fuzz-tool", 30)
    CONTRACT.require(version.strip() == b"cargo-fuzz 0.13.2", "fuzz-toolchain-mismatch")
    CONTRACT.require(runner.run([cargo_audit, "--version"], "fuzz-auditor", 30).strip() == b"cargo-audit 0.22.2",
                     "fuzz-auditor-mismatch")
    CONTRACT.require(runner.run([sys.executable, source.path / "fuzz/seed_corpus.py", "--check"], "fuzz-seeds", 30).strip()
                     == b"Verified 26 public codec seeds", "fuzz-seed-identity-invalid")
    meta = runner.json(["cargo", "+" + nightly, "metadata", "--locked", "--offline", "--format-version", "1",
                       "--manifest-path", source.path / "fuzz/Cargo.toml", "--filter-platform", CONTRACT.TOOLCHAIN["target"]],
                      "fuzz-metadata", 120)
    packages = CONTRACT.VERIFY.dependency_graph(meta, ("drmr-codec-fuzz",))
    core = [item for item in packages if item["name"] == "drmr-core"]
    fuzzer = [item for item in packages if item["name"] == "libfuzzer-sys"]
    version = CHECK.canonical_version(source.read("Cargo.toml").decode("utf-8"))
    CONTRACT.require(len(core) == len(fuzzer) == 1 and core[0]["version"] == version
                     and core[0].get("source") is None
                     and Path(core[0]["manifest_path"]).resolve() == source.path / "crates/drmr-core/Cargo.toml"
                     and Path(meta["workspace_root"]).resolve() == source.path / "fuzz"
                     and fuzzer[0]["version"] == CONTRACT.TOOLCHAIN["libfuzzer_sys"], "fuzz-metadata-invalid")
    nodes = {item["id"]: item for item in meta["resolve"]["nodes"]}
    CONTRACT.require("fuzzing" in nodes[core[0]["id"]]["features"], "fuzz-metadata-invalid")
    for package in packages:
        expected = {"private-fs": "vendor/private-fs/Cargo.toml", "process-wrap": "vendor/process-wrap/Cargo.toml"}
        if package["name"] in expected:
            CONTRACT.require(package.get("source") is None, "fuzz-vendor-identity-invalid")
        if package.get("source") is not None:
            continue
        try:
            path = Path(package["manifest_path"]).resolve().relative_to(source.path).as_posix()
        except ValueError:
            raise CONTRACT.Failure("fuzz-metadata-invalid") from None
        CONTRACT.require(path in source.paths and source.read(path) == SUPPORT.read_regular(source.path / path, 1024 * 1024),
                         "fuzz-metadata-invalid")
        if package["name"] in expected:
            CONTRACT.require(path == expected[package["name"]], "fuzz-vendor-identity-invalid")
    return audit(runner, source, identity, packages, database, cargo_audit)


def audit(runner, source, identity, packages, database, cargo_audit):
    # Reuse the existing source-backport checks and RustSec discovery. Desktop
    # dispositions remain in the separate required runtime audit; this graph has
    # no Desktop root and accepts no unresolved fuzz dependency finding.
    auditor = CONTRACT.module("audit_dependencies")
    origin = runner.run(["git", "-C", database, "remote", "get-url", "origin"], "fuzz-audit-origin", 30).strip()
    CONTRACT.require(origin.removesuffix(b".git") == b"https://github.com/RustSec/advisory-db", "fuzz-audit-database-invalid")
    revision = runner.run(["git", "-C", database, "rev-parse", "HEAD"], "fuzz-audit-revision", 30).decode("ascii").strip()
    CONTRACT.require(CONTRACT.hexadecimal(revision, 40)
                     and not runner.run(["git", "-C", database, "status", "--porcelain", "--untracked-files=all"], "fuzz-audit-clean", 30),
                     "fuzz-audit-database-invalid")
    auditor.verify_private_fs(source.path)
    original = source.read("fuzz/Cargo.lock")
    pin = auditor.read_json(source.read("tools/backports/private-fs-0.2.1.json"))
    normalized = auditor.normalized_private_fs_lock(original, pin)
    for name, data in (("actual", original), ("registry", normalized)):
        checkpoint(runner.cancel_event)
        lock = runner.workspace / (name + "-Cargo.lock")
        SUPPORT.private_write(lock, data)
        raw = runner.run([cargo_audit, "audit", "--file", lock, "--db", database,
                          "--no-fetch", "--deny", "warnings", "--json"], "fuzz-audit-" + name,
                         timeout=180, accepted_exits=(0, 1))
        value = auditor.read_json(raw)
        CONTRACT.require(not auditor.scoped_findings(value, packages), "fuzz-dependency-advisory")
    checkpoint(runner.cancel_event)
    CONTRACT.require(runner.run(["git", "-C", database, "rev-parse", "HEAD"], "fuzz-audit-unchanged", 30).strip() == revision.encode("ascii"),
                     "fuzz-audit-database-changed")
    CONTRACT.require(not runner.run(["git", "-C", database, "status", "--porcelain", "--untracked-files=all"], "fuzz-audit-still-clean", 30),
                     "fuzz-audit-database-changed")
    return {"database_revision": revision, "actual_lock_sha256": identity["lock_sha256"],
            "registry_identity_lock_sha256": CONTRACT.sha(normalized), "findings": 0, "backports_verified": True}


def qualify(source_path, binary_dir, database, cargo_audit):
    SUPPORT.require_native(CONTRACT.TOOLCHAIN["target"])
    source = CONTRACT.VERIFY.Source(source_path)
    identity = CONTRACT.identity(source)
    version = CHECK.canonical_version(source.read("Cargo.toml").decode("utf-8"))
    check_source(source, identity)
    binary_dir = Path(binary_dir).resolve()
    with cancellation() as event:
        checkpoint(event)
        workspace = SUPPORT.Workspace()
        try:
            runner = SUPPORT.Runner(workspace.path, cancel_event=event)
            audit_proof = preflight(runner, source, identity, database, cargo_audit)
            checkpoint(event)
            records = []
            seeds = CONTRACT.seeds(source)
            for target, _, _ in CONTRACT.TARGETS:
                checkpoint(event)
                corpus = workspace.path / target
                corpus.mkdir(mode=0o700)
                for item in seeds:
                    if not item["path"].startswith(target + "/"):
                        continue
                    data = source.read("fuzz/corpus/" + item["path"])
                    CONTRACT.require(len(data) == item["bytes"] and CONTRACT.sha(data) == item["sha256"], "fuzz-seed-identity-invalid")
                    SUPPORT.private_write(corpus / item["path"].split("/")[1], data)
                records.append(run_target(runner, binary_dir / target, target, corpus))
            checkpoint(event)
            check_source(source, identity)
        finally:
            workspace.close()
        checkpoint(event)
        report = {"schema_version": 1, "kind": "source-codec-fuzz", "success": True,
                  "source_head": source.head, "source_tree": source.tree, "version": version,
                  "fuzz": identity, "toolchain": dict(CONTRACT.TOOLCHAIN), "bounds": dict(CONTRACT.BOUNDS),
                  "audit": audit_proof, "targets": records, "seed_check": True, "source_unchanged": True,
                  "owned_workspace_cleanup": "confirmed", "limits": CONTRACT.LIMITS}
        return CONTRACT.validate(report, source.head, source.tree, version, identity)


class Parser(argparse.ArgumentParser):
    def error(self, _):
        raise CONTRACT.Failure("arguments-invalid")


def main(argv=None):
    path = None
    try:
        parser = Parser(description=__doc__)
        parser.add_argument("--source", type=Path, default=ROOT)
        parser.add_argument("--binary-dir", type=Path, required=True)
        parser.add_argument("--database", type=Path, required=True)
        parser.add_argument("--cargo-audit", type=Path, required=True)
        parser.add_argument("--report", type=Path)
        args = parser.parse_args(argv)
        path = args.report
        if path is not None:
            CONTRACT.require(not path.exists() and not path.is_symlink() and path.parent.is_dir()
                             and not path.parent.is_symlink()
                             and args.source.resolve() not in path.resolve().parents, "fuzz-report-destination-invalid")
        result = qualify(args.source.resolve(), args.binary_dir, args.database, args.cargo_audit)
    except KeyboardInterrupt:
        result = {"schema_version": 1, "kind": "source-codec-fuzz", "success": False, "error": "fuzz-cancelled"}
    except Exception as error:
        code = str(error) if isinstance(error, CONTRACT.Failure) else {
            "command-cancelled": "fuzz-cancelled", "command-deadline": "fuzz-timeout",
            "command-cleanup-unconfirmed": "fuzz-cleanup-unconfirmed", "owned-cleanup-unconfirmed": "fuzz-cleanup-unconfirmed"
        }.get(str(error) if isinstance(error, SUPPORT.Refusal) else "", "fuzz-qualification-failed")
        result = {"schema_version": 1, "kind": "source-codec-fuzz", "success": False, "error": code}
    encoded = (json.dumps(result, sort_keys=True) + "\n").encode("ascii")
    if path is not None:
        try:
            SUPPORT.private_write(path, encoded)
        except OSError:
            result = {"schema_version": 1, "kind": "source-codec-fuzz", "success": False, "error": "fuzz-report-write-failed"}
    print(json.dumps(result, sort_keys=True))
    return 0 if result["success"] else 1


if __name__ == "__main__":
    sys.exit(main())
