#!/usr/bin/env python3
"""Compare automatic workers with a one-worker upper bound using verified conversions.

Each fresh output is converted and independently verified by the same drmr binary.
Saved JSON contains measurements and allowlisted proof fields, never input paths,
arguments, key-file contents, or raw child output. Python 3.9 or later is required.
"""

import argparse
import hashlib
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
import tempfile
import threading
import time

MAX_JSON_BYTES = 8 * 1024 * 1024
MAX_INPUT_HASH_BYTES = 64 * 1024 * 1024
MAX_BINARY_HASH_BYTES = 256 * 1024 * 1024
CHECKS = ("packet_payloads_equal", "packet_timing_equal", "decoded_content_equal")


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


def terminate(process):
    try:
        if os.name == "posix":
            os.killpg(process.pid, signal.SIGKILL)
        else:
            process.kill()
    except ProcessLookupError:
        pass
    try:
        process.wait(timeout=5)
    except subprocess.TimeoutExpired:
        raise Failure("child-termination-failed") from None


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
        try:
            try:
                process.wait(timeout=timeout)
            except subprocess.TimeoutExpired:
                error = "child-timeout"
                terminate(process)
            except BaseException:
                terminate(process)
                raise
            elapsed = time.perf_counter() - started
        finally:
            finished.set()
            if sampler:
                sampler.join()
        memory = {"status": "sampled" if sample_count else "unmeasured",
                  "sample_interval_seconds": interval, "samples": sample_count,
                  "failed_samples": failed_sample_count,
                  "parent_sampled_peak_rss_bytes": peaks[0] if sample_count else None,
                  "descendant_sampled_peak_rss_bytes": peaks[1] if sample_count else None,
                  "observed_peak_descendant_count": peaks[2] if sample_count else None}
        if not sample_count:
            memory["reason"] = "no-process-samples" if supported else "unsupported-platform"
        measurement = {"status": "failed" if error or process.returncode else "completed",
                       "elapsed_seconds": elapsed, "exit_code": process.returncode, "memory": memory}
        if error or process.returncode:
            measurement["error"] = error or "child-failed"
            return measurement, None
        output.seek(0)
        raw = output.read(MAX_JSON_BYTES + 1)
        try:
            if len(raw) > MAX_JSON_BYTES:
                raise Failure("output-size-limit")
            payload = json.loads(raw)
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
            measured = [row[phase]["memory"] for row in complete if row[phase]["memory"]["status"] == "sampled"]
            phases[phase] = {
                "median_elapsed_seconds": statistics.median(row[phase]["elapsed_seconds"] for row in complete) if complete else None,
                "sampled_runs": len(measured),
                "median_parent_sampled_peak_rss_bytes": statistics.median(row["parent_sampled_peak_rss_bytes"] for row in measured) if measured else None,
                "median_descendant_sampled_peak_rss_bytes": statistics.median(row["descendant_sampled_peak_rss_bytes"] for row in measured) if measured else None,
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
                   "keys_file_supplied": keys is not None,
                   "worker_choices": {"auto": {"max_workers_upper_bound": None}, "one": {"max_workers_upper_bound": 1}},
                   "input_fingerprint": fingerprint(source, MAX_INPUT_HASH_BYTES, "input-entry-file-only-not-referenced-media"),
                   "binary_fingerprint": fingerprint(binary, MAX_BINARY_HASH_BYTES, "explicit-executable-file"),
                   "stability_check": "entry-key-and-binary-file-metadata-before-each-run; referenced-media-not-fingerprinted"})
    version = query(binary, ["--version"], environment).strip()
    match = re.fullmatch(r"drmr ([0-9]+\.[0-9]+\.[0-9]+(?:[-+][A-Za-z0-9.-]+)?)", version)
    report["tools"] = {"drmr": {"status": "reported", "version": match.group(1)} if match else {"status": "unmeasured"},
                       "external_tools": "not-queried"}
    for command in ("convert", "verify"):
        if "--max-workers" not in query(binary, [command, "--help"], environment):
            raise Failure("worker-option-unavailable")
    for index in range(args.repeats):
        order = ("auto", "one") if index % 2 == 0 else ("one", "auto")
        for mode in order:
            if any(identity(path) != before for path, before in initial):
                raise Failure("input-key-or-binary-changed")
            row = {"round": index + 1, "order": len(report["runs"]) + 1, "mode": mode,
                   "invocation_for_mode": "first" if index == 0 else "subsequent", "status": "failed"}
            report["runs"].append(row)
            workers = [] if mode == "auto" else ["--max-workers", "1"]
            key_args = ["--keys-file", keys] if keys else []
            with tempfile.TemporaryDirectory(prefix="drmr-measure-") as directory:
                output = Path(directory) / ("result.mp4" if args.format == "mp4" else "result")
                converted, payload = measured_run(
                    [binary, "convert", source, "--to", args.to, "--format", args.format,
                     "--output", output, *key_args, *workers, "--json"], environment, args.timeout, args.sample_interval)
                row["convert"] = converted
                if converted["status"] != "completed":
                    raise Failure("conversion-failed")
                if not isinstance(payload, dict) or type(payload.get("schema_version")) is not int or payload["schema_version"] != 1:
                    raise Failure("conversion-report-invalid")
                row["conversion_proof"] = validate_proof(payload.get("verification"))
                candidate = output if args.format == "mp4" else output / ("manifest.mpd" if args.format == "dash" else "master.m3u8")
                if not stat.S_ISREG(candidate.lstat().st_mode) or not candidate.resolve().is_relative_to(Path(directory).resolve()):
                    raise Failure("conversion-output-invalid")
                verified, proof = measured_run(
                    [binary, "verify", candidate, "--source", source, *key_args, *workers, "--json"],
                    environment, args.timeout, args.sample_interval)
                row["verify"] = verified
                if verified["status"] != "completed":
                    raise Failure("independent-verification-failed")
                row["independent_proof"] = validate_proof(proof)
                if any(identity(path) != before for path, before in initial):
                    raise Failure("input-key-or-binary-changed")
                row["status"] = "verified"
    report["success"] = True


def main(argv=None):
    report = {"schema_version": 1, "success": False, "runs": [],
              "platform": {"system": platform.system(), "machine": platform.machine()},
              "limits": ["RSS and child counts are process-table samples, not exact maxima; short-lived descendants may be missed.",
                         "Parent and descendant RSS peaks are sampled separately; they need not occur together.",
                         "Windows RSS is unmeasured; no device playback or real-license verification is performed.",
                         "First and subsequent are invocation labels, not evidence of a cold filesystem cache.",
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
