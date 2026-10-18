"""Measure fresh capture-test processes separately from compilation and child memory."""

import argparse
import json
import math
import os
from pathlib import Path
import platform
import shutil
import signal
import statistics
import subprocess
import tempfile
import time

MAX_STDOUT_BYTES = 8 * 1024 * 1024
MAX_STDERR_BYTES = 1024 * 1024


def terminate(process):
    """Reap the worker; on POSIX also terminate its inherited process group."""
    try:
        if os.name == "posix":
            os.killpg(process.pid, signal.SIGKILL)
        else:
            process.kill()
    except ProcessLookupError:
        pass
    except PermissionError:
        # Darwin can report EPERM for an exited, unreaped process group. Reap
        # that status; if it is still alive, retain the direct-child kill path.
        if process.poll() is None:
            try:
                process.kill()
            except OSError:
                raise RuntimeError("capture-child-termination-failed") from None
    try:
        process.wait(timeout=5)
    except subprocess.TimeoutExpired:
        raise RuntimeError("capture-child-termination-failed") from None


def run_captured(command, *, cwd=None, env=None, timeout=120, sample=False):
    """Use private regular files, a bounded parser, and a parent deadline.

    Spool limits are checked every polling interval, so disk usage can exceed
    them by bytes written between checks. No oversized output is parsed or
    accepted. Process groups do not cover descendants that create new groups.
    """
    supported = sample and platform.system() in ("Darwin", "Linux") and shutil.which("ps") is not None
    peak = 0
    samples = 0
    deadline = time.monotonic() + timeout
    with tempfile.TemporaryFile() as stdout, tempfile.TemporaryFile() as stderr:
        try:
            process = subprocess.Popen(
                command, cwd=cwd, env=env, stdin=subprocess.DEVNULL,
                stdout=stdout, stderr=stderr, start_new_session=os.name == "posix",
            )
        except OSError:
            raise RuntimeError("capture-child-unavailable") from None
        try:
            while True:
                if (os.fstat(stdout.fileno()).st_size > MAX_STDOUT_BYTES
                        or os.fstat(stderr.fileno()).st_size > MAX_STDERR_BYTES):
                    raise RuntimeError("capture-output-size-limit")
                status = process.poll()
                if status is not None:
                    break
                remaining = deadline - time.monotonic()
                if remaining <= 0:
                    raise RuntimeError("capture-child-timeout")
                if supported:
                    try:
                        result = subprocess.run(
                            ["ps", "-p", str(process.pid), "-o", "rss="],
                            stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
                            text=True, check=False, timeout=min(0.25, remaining),
                        )
                        if result.returncode == 0:
                            peak = max(peak, int(result.stdout.strip()) * 1024)
                            samples += 1
                    except (OSError, ValueError, subprocess.TimeoutExpired):
                        pass
                time.sleep(min(0.01, max(0, deadline - time.monotonic())))
            process.wait()
            if (os.fstat(stdout.fileno()).st_size > MAX_STDOUT_BYTES
                    or os.fstat(stderr.fileno()).st_size > MAX_STDERR_BYTES):
                raise RuntimeError("capture-output-size-limit")
            if status != 0:
                raise RuntimeError("capture-child-failed")
            stdout.seek(0)
            raw = stdout.read(MAX_STDOUT_BYTES + 1)
            if len(raw) > MAX_STDOUT_BYTES:
                raise RuntimeError("capture-output-size-limit")
            return raw, {
                "status": "sampled" if samples else "unmeasured",
                "parent_sampled_peak_rss_bytes": peak if samples else None,
                "samples": samples,
                "reason": None if samples else "process RSS sampler unavailable or process too short",
            }
        except BaseException:
            terminate(process)
            raise


def build_worker(root):
    command = [
        "cargo", "test", "-p", "drmr-core", "--test", "toolrun_resources",
        "--no-run", "--locked", "--message-format=json",
    ]
    raw, _ = run_captured(command, cwd=root, timeout=900)
    executable = None
    for line in raw.splitlines():
        try:
            item = json.loads(line)
        except (ValueError, UnicodeError, RecursionError):
            raise RuntimeError("capture-build-report-invalid") from None
        if not isinstance(item, dict):
            raise RuntimeError("capture-build-report-invalid")
        if item.get("reason") == "compiler-artifact" and item.get("target", {}).get("name") == "toolrun_resources":
            executable = item.get("executable") or executable
    if executable is None:
        raise RuntimeError("capture-worker-unavailable")
    return executable


def measure(executable, amount, timeout=120):
    stdout, memory = run_captured(
        [str(executable), "--exact", "capture_measurement_worker", "--nocapture"],
        env={**os.environ, "DRMR_CAPTURE_MEASURE_BYTES": str(amount)},
        timeout=timeout, sample=True,
    )
    try:
        reports = [json.loads(line) for line in stdout.splitlines() if line.startswith(b"{")]
    except (ValueError, UnicodeError, RecursionError):
        raise RuntimeError("capture-worker-report-invalid") from None
    if (len(reports) != 1 or not isinstance(reports[0], dict)
            or type(reports[0].get("success")) is not bool):
        raise RuntimeError("capture-worker-report-invalid")
    return {
        "result": reports[0],
        "memory": memory,
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--samples", type=int, default=5)
    parser.add_argument("--bytes", type=int, default=32 * 1024 * 1024)
    parser.add_argument("--binary", type=Path, help="use an already built capture-test executable")
    parser.add_argument("--timeout", type=float, default=120,
                        help="worker deadline in seconds (default: 120; build deadline: 900)")
    parser.add_argument("--output", type=Path, default=Path("capture-measurements.json"))
    args = parser.parse_args()
    if not math.isfinite(args.timeout) or not 0 < args.timeout <= 3600:
        parser.error("timeout must be positive and at most 3600 seconds")
    if not 1 <= args.samples <= 20:
        parser.error("samples must be between 1 and 20")
    if not 0 < args.bytes <= 1024 * 1024 * 1024 or args.bytes % 65536:
        parser.error("bytes must be a positive multiple of 65536, at most 1 GiB")
    root = Path(__file__).resolve().parent.parent
    executable = args.binary or Path(build_worker(root))
    runs = [measure(executable.resolve(), args.bytes, args.timeout) for _ in range(args.samples)]
    measured = [run["memory"]["parent_sampled_peak_rss_bytes"] for run in runs if run["memory"]["status"] == "sampled"]
    report = {
        "schema_version": "1",
        "platform": {"system": platform.system(), "machine": platform.machine()},
        "child_output_bytes_per_stream": args.bytes,
        "worker_timeout_seconds": args.timeout,
        "sample_interval_seconds": 0.01,
        "parent_peak_rss_median_bytes": statistics.median(measured) if measured else None,
        "runs": runs,
    }
    args.output.write_text(json.dumps(report, indent=2) + "\n")
    print(json.dumps(report))


if __name__ == "__main__":
    try:
        main()
    except RuntimeError as error:
        print(json.dumps({"error": str(error)}))
        raise SystemExit(1) from None
    except KeyboardInterrupt:
        print(json.dumps({"error": "capture-interrupted"}))
        raise SystemExit(1) from None
    except (OSError, ValueError, UnicodeError):
        print(json.dumps({"error": "capture-io-or-data-failed"}))
        raise SystemExit(1) from None
