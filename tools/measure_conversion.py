#!/usr/bin/env python3
"""Compare automatic workers with a one-worker upper bound using verified conversions.

Each fresh output is converted and independently verified by the same drmr binary.
Saved JSON contains measurements and allowlisted proof fields, never input paths,
arguments, key-file contents, or raw child output. Python 3.9 or later is required.
"""

import argparse
import hashlib
import importlib.util
import json
import math
import os
from pathlib import Path
import platform
import re
import signal
import stat
import statistics
import subprocess
import sys
import tempfile
import threading
import time

MAX_JSON_BYTES = 8 * 1024 * 1024
MAX_INPUT_HASH_BYTES = 64 * 1024 * 1024
MAX_BINARY_HASH_BYTES = 256 * 1024 * 1024
CHECKS = ("packet_payloads_equal", "packet_timing_equal", "decoded_content_equal")
OBSERVER_SPEC = importlib.util.spec_from_file_location(
    "measurement_observer", Path(__file__).resolve().with_name("measurement_observer.py"))
OBSERVER = importlib.util.module_from_spec(OBSERVER_SPEC)
OBSERVER_SPEC.loader.exec_module(OBSERVER)


class Failure(Exception):
    """A fixed public failure token without operator input or child diagnostics."""


class Parser(argparse.ArgumentParser):
    def error(self, message):
        raise Failure("arguments-invalid")


def parse_args(argv=None):
    parser = Parser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--drmr", type=Path, required=True, help="Explicit drmr executable.")
    parser.add_argument("--input", type=Path, required=True, help="Local media file or manifest.")
    parser.add_argument("--keys-file", type=Path, help="Protected configuration with input/output key roles.")
    parser.add_argument("--to", choices=("clear", "cenc", "cbcs", "hls-aes128"), required=True)
    parser.add_argument("--format", choices=("mp4", "dash", "hls"), required=True)
    parser.add_argument("--repeats", type=int, default=3, help="Paired repetitions, from 1 to 20 (default: 3).")
    parser.add_argument("--condition", choices=("uncontrolled", "warmed-by-equivalent-verified-run"),
                        default="uncontrolled", help="Cache condition; neither setting proves a physically cold cache.")
    parser.add_argument("--sample-interval", type=float, default=0.1,
                        help="Process-table sampling interval in seconds (default: 0.1).")
    parser.add_argument("--timeout", type=float, default=600,
                        help="Deadline for each conversion or verification, in seconds (default: 600).")
    parser.add_argument("--report", type=Path, help="Create a new JSON report; an existing file is never replaced.")
    args = parser.parse_args(argv)
    if not 1 <= args.repeats <= 20:
        raise Failure("arguments-invalid")
    if not math.isfinite(args.sample_interval) or not 0.02 <= args.sample_interval <= 10:
        raise Failure("arguments-invalid")
    if not math.isfinite(args.timeout) or not 1 <= args.timeout <= 86400:
        raise Failure("arguments-invalid")
    if (args.to == "hls-aes128" and args.format != "hls") or (args.to == "cenc" and args.format == "hls"):
        raise Failure("target-format-not-supported")
    return args


def identity(path):
    metadata = path.stat()
    if not stat.S_ISREG(metadata.st_mode):
        raise Failure("input-not-regular")
    return (metadata.st_dev, metadata.st_ino, metadata.st_size, metadata.st_mtime_ns)


def fingerprint(path, limit, scope):
    """Hash only a bounded, held regular file; referenced resources are not hashed."""
    flags = os.O_RDONLY | getattr(os, "O_NONBLOCK", 0) | getattr(os, "O_NOFOLLOW", 0)
    try:
        with os.fdopen(os.open(path, flags), "rb") as source:
            metadata = os.fstat(source.fileno())
            if not stat.S_ISREG(metadata.st_mode) or metadata.st_size > limit:
                return {"status": "unmeasured", "reason": "file-type-or-size-limit", "scope": scope}
            digest = hashlib.sha256()
            remaining = limit + 1
            total = 0
            while remaining:
                block = source.read(min(65536, remaining))
                if not block:
                    break
                digest.update(block)
                total += len(block)
                remaining -= len(block)
            after = os.fstat(source.fileno())
            if total > limit or total != metadata.st_size or metadata.st_mtime_ns != after.st_mtime_ns:
                raise Failure("input-changed")
            return {"status": "hashed", "sha256": digest.hexdigest(), "bytes": total, "scope": scope}
    except OSError:
        return {"status": "unmeasured", "reason": "file-unreadable", "scope": scope}


def sample_processes(pid):
    result = subprocess.run(["ps", "-axo", "pid=,ppid=,rss="], stdout=subprocess.PIPE,
                            stderr=subprocess.DEVNULL, timeout=2, check=True)
    rows = [tuple(map(int, line.split())) for line in result.stdout.splitlines()]
    by_pid = {child: (parent, rss * 1024) for child, parent, rss in rows}
    if pid not in by_pid:
        return None
    descendants = {pid}
    while True:
        expanded = descendants | {child for child, (parent, _) in by_pid.items() if parent in descendants}
        if expanded == descendants:
            break
        descendants = expanded
    children = descendants - {pid}
    return (by_pid[pid][1], sum(by_pid[child][1] for child in children), len(children))


def terminate(process, grace=5):
    """Request drmr's cooperative cleanup before an explicitly unconfirmed fallback."""
    if process.poll() is not None:
        return {"method": "already-exited", "cleanup": "unconfirmed"}
    if os.name == "posix":
        try:
            # Managed media children own separate groups. Signalling the CLI
            # lets its Stop flag unwind each ManagedChild before it exits.
            process.send_signal(signal.SIGTERM)
        except ProcessLookupError:
            pass
        try:
            code = process.wait(timeout=grace)
            return {"method": "cooperative-signal",
                    "cleanup": "cooperative-exit" if code == 130 else "unconfirmed"}
        except subprocess.TimeoutExpired:
            pass
    try:
        if os.name == "posix":
            os.killpg(process.pid, signal.SIGKILL)
        else:
            process.kill()
    except ProcessLookupError:
        pass
    except PermissionError:
        if process.poll() is None:
            process.kill()
    try:
        process.wait(timeout=5)
    except subprocess.TimeoutExpired:
        raise Failure("child-termination-failed") from None
    return {"method": "force", "cleanup": "unconfirmed"}


def _unique_members(pairs):
    value = dict(pairs)
    if len(value) != len(pairs):
        raise Failure("measurement-invalid")
    return value


def measured_run(argv, environment, timeout, interval):
    """Spool stdout privately, discard stderr, and retain only bounded parsed JSON."""
    supported = platform.system() in ("Darwin", "Linux")
    sample_count = 0
    failed_sample_count = 0
    peaks = [0, 0, 0]
    finished = threading.Event()
    started = time.perf_counter()
    with tempfile.TemporaryFile() as output:
        try:
            process = subprocess.Popen([str(arg) for arg in argv], stdin=subprocess.DEVNULL,
                                       stdout=output, stderr=subprocess.DEVNULL, env=environment,
                                       start_new_session=os.name == "posix")
        except OSError:
            return {"status": "failed", "error": "child-unavailable"}, None
        observer = OBSERVER.start(process, interval, timeout + 5, environment) if os.name == "nt" else None

        def sample():
            nonlocal sample_count, failed_sample_count
            while not finished.is_set():
                try:
                    row = sample_processes(process.pid)
                    if row is not None:
                        sample_count += 1
                        for index, value in enumerate(row):
                            peaks[index] = max(peaks[index], value)
                except (OSError, ValueError, subprocess.SubprocessError):
                    failed_sample_count += 1
                finished.wait(interval)

        sampler = threading.Thread(target=sample, daemon=True) if supported else None
        if sampler:
            sampler.start()
        error = None
        termination = None
        try:
            try:
                process.wait(timeout=timeout)
            except subprocess.TimeoutExpired:
                error = "child-timeout"
                termination = terminate(process)
            except BaseException:
                termination = terminate(process)
                if termination["cleanup"] != "cooperative-exit":
                    raise Failure("child-cleanup-unconfirmed") from None
                raise
            elapsed = time.perf_counter() - started
        finally:
            finished.set()
            if sampler:
                sampler.join()
            windows_memory = OBSERVER.finish(observer, interval=interval) if os.name == "nt" else None
        memory = windows_memory if windows_memory is not None else {
                  "status": ("partial" if failed_sample_count else "sampled") if sample_count else "unmeasured",
                  "metric_kind": "unix-rss", "scope": "ppid-process-table-samples-no-generation-proof",
                  "sample_interval_seconds": interval, "samples": sample_count,
                  "failed_samples": failed_sample_count,
                  "parent_sampled_peak_bytes": peaks[0] if sample_count else None,
                  "descendant_sampled_peak_bytes": peaks[1] if sample_count else None,
                  "observed_peak_descendant_count": peaks[2] if sample_count else None}
        if windows_memory is None and not sample_count:
            memory["reason"] = "no-process-samples" if supported else "unsupported-platform"
        measurement = {"status": "failed" if error or process.returncode else "completed",
                       "elapsed_seconds": elapsed, "exit_code": process.returncode, "memory": memory}
        if termination:
            measurement["termination"] = termination
        if error or process.returncode:
            measurement["error"] = error or "child-failed"
            return measurement, None
        output.seek(0)
        raw = output.read(MAX_JSON_BYTES + 1)
        try:
            if len(raw) > MAX_JSON_BYTES:
                raise Failure("output-size-limit")
            payload = json.loads(raw, object_pairs_hook=_unique_members)
        except (ValueError, UnicodeError, RecursionError, Failure) as problem:
            measurement["status"] = "failed"
            measurement["error"] = str(problem) if isinstance(problem, Failure) else "invalid-json"
            return measurement, None
        return measurement, payload


def validate_proof(value):
    if not isinstance(value, dict) or type(value.get("schema_version")) is not int or value["schema_version"] != 1:
        raise Failure("verification-proof-invalid")
    checks = value.get("checks")
    if (value.get("passed") is not True or value.get("media_verified") is not True
            or value.get("license_playback_verified") is not False or not isinstance(checks, dict)
            or checks.get("stream_metadata") != "pass" or any(checks.get(name) is not True for name in CHECKS)):
        raise Failure("verification-proof-invalid")
    return {"passed": True, "media_verified": True, "license_playback_verified": False,
            "checks": {"stream_metadata": "pass", **{name: True for name in CHECKS}}}


def _shape(value, keys):
    if not isinstance(value, dict) or set(value) != set(keys):
        raise Failure("measurement-invalid")


def _count(value):
    if type(value) is not int or not 0 <= value <= 2**63 - 1:
        raise Failure("measurement-invalid")
    return value


def _seconds(value):
    if type(value) not in (int, float) or not 0 <= value <= 1e15 or not math.isfinite(value):
        raise Failure("measurement-invalid")
    return value


def validate_measurements(value):
    """Rebuild only fixed labels and finite numeric aggregates; never save raw events."""
    _shape(value, ("schema_version", "status", "phase_semantics", "tool_semantics", "coverage",
                   "phases", "tools", "unclassified_tool_calls", "invalid_tool_records", "workspace", "io"))
    constants = {"schema_version": 1,
                 "phase_semantics": "elapsed-scope-sums-overlap-not-command-wall-or-cpu",
                 "tool_semantics": "generic-call-duration-sums-include-admission-capture-and-cleanup",
                 "coverage": "explicit-generic-scopes-only-not-all-rust-or-filesystem-work"}
    if any(type(value[name]) is not type(expected) or value[name] != expected for name, expected in constants.items()):
        raise Failure("measurement-invalid")
    if value["status"] not in ("observed", "partial", "unmeasured"):
        raise Failure("measurement-invalid")
    result = {**constants, "status": value["status"],
              "unclassified_tool_calls": _count(value["unclassified_tool_calls"]),
              "invalid_tool_records": _count(value["invalid_tool_records"])}
    for array, label, names, count, seconds in (
            ("phases", "phase", ("input-preparation", "inspection", "normalization", "packaging", "verification", "publication"),
             "scopes", "scope_elapsed_sum_seconds"),
            ("tools", "tool", ("ffmpeg", "ffprobe", "packager"), "calls", "reported_duration_sum_seconds")):
        rows = value[array]
        if not isinstance(rows, list) or len(rows) != len(names):
            raise Failure("measurement-invalid")
        output = []
        for row, name in zip(rows, names):
            keys = (label, "status", count, seconds) + (("unsuccessful_calls",) if array == "tools" else ())
            _shape(row, keys)
            if row[label] != name or row["status"] not in ("unmeasured", "observed-scopes", "unobserved", "observed-calls"):
                raise Failure("measurement-invalid")
            item = {label: name, "status": row["status"], count: _count(row[count]), seconds: _seconds(row[seconds])}
            if array == "tools":
                item["unsuccessful_calls"] = _count(row["unsuccessful_calls"])
                if item["unsuccessful_calls"] > item[count]:
                    raise Failure("measurement-invalid")
            expected = ("observed-scopes" if item[count] else "unmeasured") if array == "phases" else (
                "observed-calls" if item[count] else "unobserved")
            if item["status"] != expected or (item[count] == 0 and item[seconds] != 0):
                raise Failure("measurement-invalid")
            output.append(item)
        result[array] = output
    workspace = value["workspace"]
    _shape(workspace, ("status", "scope", "checkpoints", "incomplete_checkpoints", "checkpoint_regular_file_bytes_max", "reason"))
    if workspace["scope"] != "selected-quiescent-stage-sets-not-all-live-command-stages":
        raise Failure("measurement-invalid")
    if workspace["status"] not in ("unmeasured", "partial", "observed-checkpoints"):
        raise Failure("measurement-invalid")
    reasons = (None, "no-owned-workspace-checkpoints", "overlapping-owned-roots", "checkpoint-time-budget",
               "checkpoint-depth-limit", "checkpoint-entry-limit", "checkpoint-metadata-unavailable",
               "checkpoint-reparse-or-symlink", "checkpoint-size-overflow", "checkpoint-directory-unavailable",
               "checkpoint-nonregular-entry")
    if workspace["reason"] not in reasons:
        raise Failure("measurement-invalid")
    result["workspace"] = {name: workspace[name] for name in ("status", "scope", "reason")}
    for name in ("checkpoints", "incomplete_checkpoints"):
        result["workspace"][name] = _count(workspace[name])
    size = workspace["checkpoint_regular_file_bytes_max"]
    result["workspace"]["checkpoint_regular_file_bytes_max"] = None if size is None else _count(size)
    if (workspace["checkpoints"] == 0) != (size is None):
        raise Failure("measurement-invalid")
    expected_workspace = "partial" if workspace["incomplete_checkpoints"] else (
        "observed-checkpoints" if workspace["checkpoints"] else "unmeasured")
    if workspace["status"] != expected_workspace:
        raise Failure("measurement-invalid")
    if (workspace["incomplete_checkpoints"] or result["invalid_tool_records"] or result["unclassified_tool_calls"]) and result["status"] == "observed":
        raise Failure("measurement-invalid")
    _shape(value["io"], ("status", "reason"))
    if value["io"] != {"status": "unmeasured", "reason": "total-process-io-not-collected"}:
        raise Failure("measurement-invalid")
    result["io"] = {"status": "unmeasured", "reason": "total-process-io-not-collected"}
    return result


def query(binary, arguments, environment):
    with tempfile.TemporaryFile() as output:
        try:
            result = subprocess.run([str(binary), *arguments], stdin=subprocess.DEVNULL,
                                    stdout=output, stderr=subprocess.DEVNULL,
                                    timeout=10, env=environment, check=False)
        except (OSError, subprocess.SubprocessError):
            raise Failure("binary-query-failed") from None
        output.seek(0)
        data = output.read(1024 * 1024 + 1)
        if result.returncode or len(data) > 1024 * 1024:
            raise Failure("binary-query-failed")
        return data.decode("utf-8", errors="replace")


def summarize(rows):
    summary = {}
    for mode in ("auto", "one"):
        complete = [row for row in rows if row["mode"] == mode and row["status"] == "verified"]
        phases = {}
        for phase in ("convert", "verify"):
            measured = [row[phase]["memory"] for row in complete if row[phase]["memory"]["status"] in ("sampled", "partial")]
            phases[phase] = {
                "median_elapsed_seconds": statistics.median(row[phase]["elapsed_seconds"] for row in complete) if complete else None,
                "sampled_runs": len(measured),
                "median_parent_sampled_peak_bytes": statistics.median(row["parent_sampled_peak_bytes"] for row in measured) if measured else None,
                "median_descendant_sampled_peak_bytes": statistics.median(row["descendant_sampled_peak_bytes"] for row in measured) if measured else None,
                "maximum_observed_descendant_count": max((row["observed_peak_descendant_count"] for row in measured), default=None),
            }
        summary[mode] = {"verified_runs": len(complete), **phases}
    return summary


def benchmark(args, report):
    binary = args.drmr.resolve(strict=True)
    source = args.input.resolve(strict=True)
    keys = args.keys_file.resolve(strict=True) if args.keys_file else None
    initial = [(path, identity(path)) for path in (binary, source, keys) if path is not None]
    environment = {name: value for name, value in os.environ.items() if not name.upper().startswith("DRM_")}
    report.update({"target": args.to, "format": args.format, "repeats_per_mode": args.repeats,
                   "keys_file_supplied": keys is not None, "warmups": [],
                   "condition": {"kind": args.condition, "physical_cold": "not-measured",
                                 "cache_residency": "unverified",
                                 "warmups_per_measured_case": int(args.condition != "uncontrolled")},
                   "worker_choices": {"auto": {"max_workers_upper_bound": None}, "one": {"max_workers_upper_bound": 1}},
                   "input_fingerprint": fingerprint(source, MAX_INPUT_HASH_BYTES, "input-entry-file-only-not-referenced-media"),
                   "binary_fingerprint": fingerprint(binary, MAX_BINARY_HASH_BYTES, "explicit-executable-file"),
                   "stability_check": "entry-key-and-binary-file-metadata-before-each-run; referenced-media-not-fingerprinted"})
    version = query(binary, ["--version"], environment).strip()
    match = re.fullmatch(r"drmr ([0-9]+\.[0-9]+\.[0-9]+(?:[-+][A-Za-z0-9.-]+)?)", version)
    report["tools"] = {"drmr": {"status": "reported", "version": match.group(1)} if match else {"status": "unmeasured"},
                       "external_tools": "not-queried"}
    for command in ("convert", "verify"):
        help_text = query(binary, [command, "--help"], environment)
        if "--max-workers" not in help_text or "--measure" not in help_text:
            raise Failure("measurement-option-unavailable")

    def run_case(mode, index, warmup):
        if any(identity(path) != before for path, before in initial):
            raise Failure("input-key-or-binary-changed")
        rows = report["warmups"] if warmup else report["runs"]
        row = {"round": index + 1, "order": len(rows) + 1, "mode": mode, "status": "failed"}
        rows.append(row)
        workers = [] if mode == "auto" else ["--max-workers", "1"]
        key_args = ["--keys-file", keys] if keys else []
        with tempfile.TemporaryDirectory(prefix="drmr-measure-") as directory:
            output = Path(directory) / ("result.mp4" if args.format == "mp4" else "result")
            converted, payload = measured_run(
                [binary, "convert", source, "--to", args.to, "--format", args.format,
                 "--output", output, *key_args, *workers, "--measure", "--json"],
                environment, args.timeout, args.sample_interval)
            if not warmup:
                row["convert"] = converted
            if converted["status"] != "completed":
                raise Failure("conversion-failed")
            if not isinstance(payload, dict) or type(payload.get("schema_version")) is not int or payload["schema_version"] != 1:
                raise Failure("conversion-report-invalid")
            row["conversion_proof"] = validate_proof(payload.get("verification"))
            work = validate_measurements(payload.get("measurements"))
            if not warmup:
                converted["work_measurements"] = work
            if converted["memory"].get("observer_cleanup") == "unconfirmed":
                raise Failure("observer-cleanup-unconfirmed")
            candidate = output if args.format == "mp4" else output / ("manifest.mpd" if args.format == "dash" else "master.m3u8")
            if not stat.S_ISREG(candidate.lstat().st_mode) or not candidate.resolve().is_relative_to(Path(directory).resolve()):
                raise Failure("conversion-output-invalid")
            verified, proof = measured_run(
                [binary, "verify", candidate, "--source", source, *key_args, *workers, "--measure", "--json"],
                environment, args.timeout, args.sample_interval)
            if not warmup:
                row["verify"] = verified
            if verified["status"] != "completed":
                raise Failure("independent-verification-failed")
            row["independent_proof"] = validate_proof(proof)
            work = validate_measurements(proof.get("measurements"))
            if not warmup:
                verified["work_measurements"] = work
            if verified["memory"].get("observer_cleanup") == "unconfirmed":
                raise Failure("observer-cleanup-unconfirmed")
            if any(identity(path) != before for path, before in initial):
                raise Failure("input-key-or-binary-changed")
            row["status"] = "verified"

    for index in range(args.repeats):
        order = ("auto", "one") if index % 2 == 0 else ("one", "auto")
        for mode in order:
            if args.condition != "uncontrolled":
                run_case(mode, index, True)
            run_case(mode, index, False)
    report["success"] = True


def main(argv=None):
    report = {"schema_version": 2, "success": False, "runs": [],
              "platform": {"system": platform.system(), "machine": platform.machine()},
              "limits": ["RSS and child counts are process-table samples, not exact maxima; short-lived descendants may be missed.",
                         "Parent and descendant RSS peaks are sampled separately; they need not occur together.",
                         "Windows uses working-set samples; Unix uses RSS. Sampling is sequential, not an atomic snapshot.",
                         "Observer cleanup and media cleanup are separate; missing observations are never complete zeroes.",
                         "Cache conditions do not establish physical coldness or full residency. Hash and binary queries can warm caches.",
                         "Workspace values cover selected quiescent stages, not an exact disk peak. Total I/O is unmeasured.",
                         "No device playback or license acquisition is verified.",
                         "Fresh temporary outputs are deleted after each paired case; no existing output is reused."]}
    args = None
    try:
        args = parse_args(argv)
        benchmark(args, report)
    except Failure as error:
        report["error"] = str(error)
    except KeyboardInterrupt:
        report["error"] = "interrupted"
    except (OSError, ValueError, UnicodeError, RuntimeError):
        report["error"] = "io-or-data-failed"
    report["summary"] = summarize(report["runs"])
    encoded = json.dumps(report, indent=2, sort_keys=True, allow_nan=False) + "\n"
    if args and args.report:
        try:
            with os.fdopen(os.open(args.report, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600), "w", encoding="utf-8") as output:
                output.write(encoded)
        except OSError:
            report["success"] = False
            report["error"] = "report-create-failed"
            encoded = json.dumps(report, indent=2, sort_keys=True, allow_nan=False) + "\n"
    print(encoded, end="")
    return 0 if report["success"] else 1


if __name__ == "__main__":
    raise SystemExit(main())
