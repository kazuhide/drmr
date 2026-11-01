#!/usr/bin/env python3
"""Run a once-only compiled core test plan with bounded POSIX concurrency."""
import argparse
import importlib.util
import json
import os
from pathlib import Path
import re
import signal
import subprocess
import time


SPEC = importlib.util.spec_from_file_location("native_test_plan", Path(__file__).with_name("native-test-plan.py"))
PLAN = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(PLAN)


def executables(text):
    paths = []
    for line in text.splitlines():
        item = json.loads(line)
        if item.get("reason") == "compiler-artifact" and item.get("profile", {}).get("test") and item.get("executable"):
            paths.append(item["executable"])
    if not paths or len(paths) != len(set(paths)):
        raise ValueError("Compiled native test inventory is missing or duplicated")
    return paths


def budget(processors, memory_bytes):
    workers = max(1, min(2, processors, memory_bytes // (2 * 1024 ** 3)))
    return workers, max(1, min(2, processors // workers))


def memory_bytes():
    try:
        if Path("/proc/meminfo").exists():
            value = re.search(r"^MemAvailable:\s+(\d+) kB$", Path("/proc/meminfo").read_text(), re.MULTILINE)
            return int(value.group(1)) * 1024
        return int(subprocess.run(["sysctl", "-n", "hw.memsize"], capture_output=True, text=True,
                                  timeout=5, check=True).stdout)
    except (OSError, ValueError, AttributeError, subprocess.SubprocessError):
        return 0


def terminate_session(process):
    # ToolRunner creates separate process groups inside this session. Keep the
    # unreaped session leader's PID owned until all its live members have exited.
    try:
        os.killpg(process.pid, signal.SIGKILL)
    except ProcessLookupError:
        pass
    deadline = time.monotonic() + 10
    while True:
        census = subprocess.run(["ps", "-A", "-o", "pid=,stat="], check=True,
                                capture_output=True, text=True, timeout=5).stdout
        if len(census) > 2 * 1024 * 1024:
            raise RuntimeError("Native test process census exceeded its bound")
        live = []
        for line in census.splitlines():
            pid, state = line.split()
            if state.startswith("Z"):
                continue
            try:
                pid = int(pid)
                if os.getsid(pid) == process.pid:
                    live.append(pid)
                    os.kill(pid, signal.SIGKILL)
            except (ProcessLookupError, PermissionError):
                continue
        if not live:
            process.wait(timeout=10)
            return
        if time.monotonic() > deadline:
            raise RuntimeError("Native test session termination is unconfirmed")
        time.sleep(.01)


def execute(groups, directory, *, workers, threads):
    cancelled = False
    def cancel(_signum, _frame):
        nonlocal cancelled
        cancelled = True
    # Install once, before owning children. Signals request cancellation; only
    # safe points raise it, after a new handle is registered. exec resets these
    # caught handlers in children, without masking their normal signal delivery.
    handlers = {sig: signal.getsignal(sig) for sig in (signal.SIGINT, signal.SIGTERM)}
    try:
        for sig in handlers:
            signal.signal(sig, cancel)
        return execute_owned(groups, directory, workers=workers, threads=threads, cancelled=lambda: cancelled)
    finally:
        for sig, handler in handlers.items():
            signal.signal(sig, handler)


def execute_owned(groups, directory, *, workers, threads, cancelled):
    if workers not in (1, 2) or not 1 <= threads <= 2:
        raise ValueError("Invalid native concurrency budget")
    priorities = sum(bool(group["priority"]) for group in groups)
    if any(not item["priority"] for item in groups[:priorities]):
        raise ValueError("Invalid native priority boundary")
    directory.mkdir()
    active, results, next_index = [], [], 0
    started = time.monotonic()
    try:
        while next_index < len(groups) or active:
            if cancelled():
                raise KeyboardInterrupt
            if priorities and len(results) == priorities and not active and any(item["exit_code"] for item in results):
                break
            while next_index < len(groups) and len(active) < workers:
                if next_index == priorities and len(results) < priorities:
                    break
                index = next_index
                item = groups[index]
                path = directory / f"group-{index:04}"
                temp = path / "temp"
                temp.mkdir(parents=True)
                env = {**os.environ, "TMPDIR": str(temp), "TMP": str(temp), "TEMP": str(temp)}
                output, error = (path / "stdout.log").open("wb"), (path / "stderr.log").open("wb")
                record = {"index": index, "directory": str(path), "process": None, "output": output, "error": error}
                active.append(record)
                args = [item["executable"], *item["arguments"], "--test-threads", str(threads), "--nocapture"]
                record["process"] = subprocess.Popen(args, env=env, stdout=output, stderr=error, start_new_session=True)
                next_index += 1
                if cancelled():
                    raise KeyboardInterrupt
                print(f"Native group {index + 1}/{len(groups)} started", flush=True)
            for record in active[:]:
                process = record["process"]
                code = process.poll()
                if code is None:
                    continue
                process.wait()
                record["output"].close()
                record["error"].close()
                result = {"index": record["index"], "directory": record["directory"], "exit_code": code}
                if "tests" in groups[record["index"]] and not complete(groups[record["index"]], result):
                    result["exit_code"] = code or 1
                results.append(result)
                active.remove(record)
                print(f"Native group {record['index'] + 1}/{len(groups)} finished; exit {code}; elapsed {time.monotonic() - started:.1f}s", flush=True)
            if active:
                time.sleep(.05)
    finally:
        cleanup_errors = []
        for record in active:
            try:
                if record["process"] is not None:
                    terminate_session(record["process"])
            except BaseException as error:
                cleanup_errors.append(error)
            finally:
                record["output"].close()
                record["error"].close()
        if cleanup_errors:
            raise RuntimeError("Native test process cleanup is unconfirmed") from cleanup_errors[0]
    if cancelled():
        raise KeyboardInterrupt
    return sorted(results, key=lambda result: result["index"])


def complete(group, result):
    text = (Path(result["directory"]) / "stdout.log").read_text(encoding="utf-8", errors="replace")
    summaries = re.findall(r"^test result: (ok|FAILED)\. (\d+) passed; (\d+) failed; (\d+) ignored; (\d+) measured; (\d+) filtered out(?:;.*)?$", text, re.MULTILINE)
    return (result["exit_code"] == 0 and len(summaries) == 1 and summaries[0][0] == "ok"
            and int(summaries[0][1]) == len(group["tests"])
            and all(int(value) == 0 for value in summaries[0][2:5]))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--build", type=Path, required=True)
    parser.add_argument("--directory", type=Path, required=True)
    parser.add_argument("--log", type=Path, required=True)
    args = parser.parse_args()
    groups = PLAN.plan(executables(args.build.read_text(encoding="utf-8")))
    workers, threads = budget(os.cpu_count() or 1, memory_bytes())
    print(f"Native schedule: {sum(len(item['tests']) for item in groups)} tests; {workers} groups and {threads} test threads per group", flush=True)
    with args.directory.with_suffix(".json").open("x", encoding="utf-8") as output:
        json.dump(groups, output)
        output.write("\n")
    # SIGTERM must use the same finally cleanup as an interrupted foreground run.
    previous = signal.signal(signal.SIGTERM, lambda _signum, _frame: (_ for _ in ()).throw(KeyboardInterrupt()))
    try:
        results = execute(groups, args.directory, workers=workers, threads=threads)
    finally:
        signal.signal(signal.SIGTERM, previous)
        with args.log.open("x", encoding="utf-8") as output:
            for path in sorted(args.directory.glob("group-*/*.log")):
                text = path.read_text(encoding="utf-8", errors="replace")
                output.write(text)
                print(text, end="", flush=True)
    return 0 if len(results) == len(groups) and all(complete(groups[item["index"]], item) for item in results) else 1


if __name__ == "__main__":
    raise SystemExit(main())
