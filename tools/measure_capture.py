"""Measure fresh capture-test processes separately from compilation and child memory."""

import argparse
import json
import os
from pathlib import Path
import platform
import shutil
import statistics
import subprocess
import time


def build_worker(root):
    command = [
        "cargo", "test", "-p", "drmr-core", "--test", "toolrun_resources",
        "--no-run", "--locked", "--message-format=json",
    ]
    process = subprocess.Popen(command, cwd=root, stdout=subprocess.PIPE, text=True)
    executable = None
    for line in process.stdout:
        item = json.loads(line)
        if item.get("reason") == "compiler-artifact" and item["target"]["name"] == "toolrun_resources":
            executable = item.get("executable") or executable
    if process.wait() != 0 or executable is None:
        raise RuntimeError("could not build the capture measurement worker")
    return executable


def measure(executable, amount):
    process = subprocess.Popen(
        [str(executable), "--exact", "capture_measurement_worker", "--nocapture"],
        env={**os.environ, "DRMR_CAPTURE_MEASURE_BYTES": str(amount)},
        stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True,
    )
    supported = platform.system() in ("Darwin", "Linux") and shutil.which("ps") is not None
    peak = 0
    samples = 0
    while process.poll() is None:
        if supported:
            sample = subprocess.run(
                ["ps", "-p", str(process.pid), "-o", "rss="],
                capture_output=True, text=True, check=False,
            )
            try:
                peak = max(peak, int(sample.stdout.strip()) * 1024)
                samples += 1
            except ValueError:
                pass
        time.sleep(0.01)
    stdout, stderr = process.communicate()
    if process.returncode != 0:
        raise RuntimeError(f"capture measurement worker failed:\n{stdout}\n{stderr}")
    reports = [json.loads(line) for line in stdout.splitlines() if line.startswith("{")]
    if len(reports) != 1:
        raise RuntimeError("capture worker did not return exactly one result")
    return {
        "result": reports[0],
        "memory": {
            "status": "sampled" if samples else "unmeasured",
            "parent_sampled_peak_rss_bytes": peak if samples else None,
            "samples": samples,
            "reason": None if samples else "process RSS sampler unavailable or process too short",
        },
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--samples", type=int, default=5)
    parser.add_argument("--bytes", type=int, default=32 * 1024 * 1024)
    parser.add_argument("--binary", type=Path, help="use an already built capture-test executable")
    parser.add_argument("--output", type=Path, default=Path("capture-measurements.json"))
    args = parser.parse_args()
    if not 1 <= args.samples <= 20:
        parser.error("samples must be between 1 and 20")
    if not 0 < args.bytes <= 1024 * 1024 * 1024 or args.bytes % 65536:
        parser.error("bytes must be a positive multiple of 65536, at most 1 GiB")
    root = Path(__file__).resolve().parent.parent
    executable = args.binary or Path(build_worker(root))
    runs = [measure(executable.resolve(), args.bytes) for _ in range(args.samples)]
    measured = [run["memory"]["parent_sampled_peak_rss_bytes"] for run in runs if run["memory"]["status"] == "sampled"]
    report = {
        "schema_version": "1",
        "platform": {"system": platform.system(), "machine": platform.machine()},
        "child_output_bytes_per_stream": args.bytes,
        "sample_interval_seconds": 0.01,
        "parent_peak_rss_median_bytes": statistics.median(measured) if measured else None,
        "runs": runs,
    }
    args.output.write_text(json.dumps(report, indent=2) + "\n")
    print(json.dumps(report))


if __name__ == "__main__":
    main()
