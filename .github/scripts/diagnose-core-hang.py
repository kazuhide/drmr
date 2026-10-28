#!/usr/bin/env python3
"""Bounded Linux diagnostics for the unchanged presentation and TS integration binaries."""
from __future__ import annotations

import argparse
import ctypes
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import signal
import stat
import subprocess
import sys
import tempfile
import time

BASE = "8b1f7981db3688b689af89635aef6bf8a6354f4d"
BASE_TREE = "2de209b0748cd985ab297bc56322d87a067f14ae"
TARGETS = {"conversion_presentations": 5, "conversion_ts": 12}
ALLOWED_CHANGES = {".github/scripts/diagnose-core-hang.py", ".github/workflows/ci.yml"}
SAFE_NAME = re.compile(r"[A-Za-z0-9_:]+")
SUMMARY = re.compile(r"test result: (ok|FAILED)\. (\d+) passed; (\d+) failed; (\d+) ignored; (\d+) measured; (\d+) filtered out; finished in ([0-9.]+)s")
STOP = False


def save(path, value):
    path.write_text(json.dumps(value, indent=2, sort_keys=True) + "\n")


def sha(path):
    value = hashlib.sha256()
    with Path(path).open("rb") as source:
        for block in iter(lambda: source.read(1024 * 1024), b""):
            value.update(block)
    return value.hexdigest()


def command(argv):
    result = subprocess.run(argv, capture_output=True, timeout=10, check=False)
    if result.returncode or len(result.stdout) > 1048576:
        raise RuntimeError("identity-command-failed")
    return result.stdout.decode().strip()


def cgroup_limits():
    result = {}
    for label, path in {
        "cpu_max": "/sys/fs/cgroup/cpu.max",
        "memory_max": "/sys/fs/cgroup/memory.max",
        "cpu_quota_v1": "/sys/fs/cgroup/cpu/cpu.cfs_quota_us",
        "cpu_period_v1": "/sys/fs/cgroup/cpu/cpu.cfs_period_us",
        "memory_limit_v1": "/sys/fs/cgroup/memory/memory.limit_in_bytes",
    }.items():
        try:
            value = Path(path).read_text().strip()
            result[label] = value if re.fullmatch(r"(?:max|-?[0-9]+)(?: [0-9]+)?", value) else None
        except OSError:
            result[label] = None
    return result


def identity(binaries, build_log, channel):
    if command(["git", "rev-parse", BASE + "^{tree}"]) != BASE_TREE:
        raise RuntimeError("base-tree-mismatch")
    changed = set(command(["git", "diff", "--name-only", BASE, "HEAD"]).splitlines())
    if not changed <= ALLOWED_CHANGES or command(["git", "status", "--porcelain", "--untracked-files=all"]):
        raise RuntimeError("product-source-changed")
    tools = {}
    for name in ("rustc", "cargo", "packager", "ffmpeg", "ffprobe", "mp4dump", "gdb", "python3"):
        executable = command(["rustup", "which", name]) if name in ("rustc", "cargo") else shutil.which(name)
        if not executable:
            raise RuntimeError("required-tool-missing")
        version_args = [name, "--version"] if name not in ("ffmpeg", "ffprobe", "mp4dump") else [name, "-version"] if name != "mp4dump" else [name]
        version_result = subprocess.run(version_args, stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
                                        timeout=10, check=False)
        if len(version_result.stdout) > 1048576:
            raise RuntimeError("version-output-limit")
        version = version_result.stdout.decode(errors="replace")
        tools[name] = {"sha256": sha(executable),
                       "version_returncode": version_result.returncode,
                       "version_output_sha256": hashlib.sha256(version.encode()).hexdigest(),
                       "numeric_versions": re.findall(r"(?<![A-Za-z0-9])v?\d+(?:\.\d+){1,3}(?:[A-Za-z0-9.+-]*)?", version)[:8]}
    minimum = command([sys.executable, ".github/scripts/minimum-rust.py"])
    actual = command(["rustc", "--version"]).split()[1]
    if minimum != "1.95.0" or (channel == "minimum" and actual != minimum) or os.uname().machine != "x86_64":
        raise RuntimeError("platform-or-compiler-mismatch")
    return {"base": BASE, "base_tree": BASE_TREE,
            "head": command(["git", "rev-parse", "HEAD"]),
            "tree": command(["git", "rev-parse", "HEAD^{tree}"]),
            "product_tree": command(["git", "rev-parse", "HEAD:crates"]),
            "base_product_tree": command(["git", "rev-parse", BASE + ":crates"]),
            "unchanged_product_source": True, "binary_sha256": {name: sha(path) for name, path in binaries.items()},
            "cargo_lock_sha256": sha("Cargo.lock"), "build_log_sha256": sha(build_log),
            "harness_sha256": sha(__file__), "tool_sha256": tools,
            "minimum_rust": minimum, "actual_rust": actual, "requested_channel": channel,
            "machine": "x86_64", "logical_cpus": os.cpu_count(),
            "affinity_cpus": len(os.sched_getaffinity(0)), "cgroup_limits": cgroup_limits()}


def select_binary(text, expected):
    candidates = []
    for line in text.splitlines():
        row = json.loads(line)
        target = row.get("target", {})
        if (row.get("reason") == "compiler-artifact" and target.get("name") == expected
                and "test" in target.get("kind", []) and row.get("profile", {}).get("test") is True
                and row.get("executable")):
            candidates.append(Path(row["executable"]).resolve())
    if len(candidates) != 1:
        raise RuntimeError("test-artifact-selection")
    return candidates[0]


def read_stat(pid, tid=None):
    try:
        location = Path(f"/proc/{pid}") if tid is None else Path(f"/proc/{pid}/task/{tid}")
        fields = (location / "stat").read_text().rsplit(")", 1)[1].split()
        return {"pid": pid, "state": fields[0], "ppid": int(fields[1]),
                "pgrp": int(fields[2]), "user_ticks": int(fields[11]),
                "system_ticks": int(fields[12]), "threads": int(fields[17]),
                "start_ticks": int(fields[19]), "rss_pages": int(fields[21])}
    except (OSError, ValueError, IndexError):
        return None


class Owner:
    """Retain kernel-bound handles, including descendants adopted by this subreaper."""
    def __init__(self):
        self.rows = {}
        self.fds = {}

    def scan(self):
        rows = {}
        for path in Path("/proc").iterdir():
            if path.name.isdecimal():
                row = read_stat(int(path.name))
                if row:
                    rows[row["pid"]] = row
        selected = {os.getpid()}
        selected.update(pid for pid, old in self.rows.items()
                        if pid in rows and rows[pid]["start_ticks"] == old["start_ticks"])
        while True:
            added = {pid for pid, row in rows.items() if row["ppid"] in selected} - selected
            if not added:
                break
            selected.update(added)
        for pid in selected - {os.getpid()}:
            row = rows.get(pid)
            if row and pid in self.fds and row["start_ticks"] != self.rows[pid]["start_ticks"]:
                os.close(self.fds.pop(pid))
                del self.rows[pid]
            if not row or pid in self.fds:
                continue
            try:
                fd = os.pidfd_open(pid)
                current = read_stat(pid)
                if current and current["start_ticks"] == row["start_ticks"]:
                    self.fds[pid] = fd
                    self.rows[pid] = row
                else:
                    os.close(fd)
            except ProcessLookupError:
                pass

    def alive(self):
        return [pid for pid, row in self.rows.items() if (now := read_stat(pid))
                and now["start_ticks"] == row["start_ticks"] and now["state"] != "Z"]

    def cleanup(self, child):
        confirmed = False
        try:
            for number, seconds in ((signal.SIGTERM, 1), (signal.SIGKILL, 4)):
                until = time.monotonic() + seconds
                while time.monotonic() < until:
                    self.scan()
                    for fd in self.fds.values():
                        try:
                            signal.pidfd_send_signal(fd, number)
                        except ProcessLookupError:
                            pass
                    child.poll()
                    if child.returncode is not None:
                        while True:
                            try:
                                if os.waitpid(-1, os.WNOHANG)[0] == 0:
                                    break
                            except ChildProcessError:
                                break
                    if child.returncode is not None and not self.alive():
                        confirmed = True
                        break
                    time.sleep(0.05)
                if confirmed:
                    break
            return confirmed
        finally:
            for fd in self.fds.values():
                os.close(fd)


def snapshot(owner):
    result = []
    for pid in sorted(owner.alive()):
        entry = read_stat(pid)
        if not entry:
            continue
        threads = []
        try:
            for task in sorted(Path(f"/proc/{pid}/task").iterdir())[:256]:
                row = read_stat(pid, int(task.name))
                if row:
                    wait = (task / "wchan").read_text().strip()
                    row["tid"] = int(task.name)
                    row["wchan"] = wait if re.fullmatch(r"[A-Za-z0-9_]{1,96}", wait) else "unavailable"
                    threads.append(row)
        except OSError:
            pass
        entry["thread_state"] = threads
        counts = {}
        try:
            for fd in list(Path(f"/proc/{pid}/fd").iterdir())[:4096]:
                try:
                    mode = fd.stat().st_mode
                    kind = ("pipe" if stat.S_ISFIFO(mode) else "socket" if stat.S_ISSOCK(mode)
                            else "regular" if stat.S_ISREG(mode) else "directory" if stat.S_ISDIR(mode)
                            else "device" if stat.S_ISCHR(mode) or stat.S_ISBLK(mode) else "other")
                    counts[kind] = counts.get(kind, 0) + 1
                except OSError:
                    pass
        except OSError:
            pass
        entry["fd_kinds"] = counts
        options = packager_options(pid)
        if options is not None:
            entry["selected_tool_options"] = options
        try:
            entry["executable_sha256"] = sha(f"/proc/{pid}/exe")
        except OSError:
            entry["executable_sha256"] = None
        result.append(entry)
    return result


PACKAGER_FLAGS = frozenset((
    "single_threaded", "io_cache_size", "segment_duration", "fragment_duration",
    "enable_raw_key_encryption", "enable_raw_key_decryption", "keys", "iv",
    "protection_scheme", "protection_systems", "clear_lead", "mpd_output",
    "hls_master_playlist_output", "hls_playlist_type", "hls_key_uri",
    "generate_static_live_mpd", "mp4_reset_initial_composition_offset_to_zero",
    "allow_codec_switching", "allow_approximate_segment_timeline", "quiet",
    "v", "vmodule", "version", "dump_stream_info", "temp_dir",
))


def selected_options(arguments, cwd):
    inputs = []
    flags = set()
    single_threaded = None
    cache = None
    for index, argument in enumerate(arguments):
        if argument.startswith("input=") or argument.startswith("in="):
            inputs.append(argument.split(",", 1)[0].split("=", 1)[1])
        if not argument.startswith("--"):
            continue
        name, separator, value = argument[2:].partition("=")
        if name in PACKAGER_FLAGS:
            flags.add(name)
        if name == "single_threaded":
            single_threaded = True if not separator or value == "true" else False if value == "false" else None
        if name == "io_cache_size":
            number = value if separator else arguments[index + 1] if index + 1 < len(arguments) else ""
            cache = int(number) if re.fullmatch(r"[0-9]{1,12}", number) else None
    hashes = set()
    hashed = 0
    # Read only small regular local fixture inputs. URLs and all path strings stay local.
    for value in inputs[:32]:
        if "://" in value or not value:
            continue
        path = Path(value)
        if not path.is_absolute():
            path = cwd / path
        try:
            info = path.stat()
            if stat.S_ISREG(info.st_mode) and info.st_size <= 64 * 1024 * 1024:
                hashes.add(sha(path))
                hashed += 1
        except OSError:
            pass
    return {"tool_role": "packager", "input_descriptor_count": len(inputs),
            "local_input_hashed_count": hashed, "distinct_local_input_sha256_count": len(hashes),
            "single_threaded": single_threaded, "io_cache_size": cache, "flag_names": sorted(flags)}


def packager_options(pid):
    try:
        if "packager" not in Path(os.readlink(f"/proc/{pid}/exe")).name:
            return None
    except OSError:
        return None
    try:
        with Path(f"/proc/{pid}/cmdline").open("rb") as stream:
            data = stream.read(65537)
        if len(data) > 65536:
            return {"tool_role": "packager", "options_unavailable": True}
        arguments = [value.decode(errors="replace") for value in data.split(b"\0")[1:] if value]
        return selected_options(arguments, Path(os.readlink(f"/proc/{pid}/cwd")))
    except OSError:
        return {"tool_role": "packager", "options_unavailable": True}


def stack_priority(pid, root):
    if pid == root:
        return (0, pid)
    try:
        # Inspect locally for selection; never publish a full executable path.
        packager = "packager" in Path(os.readlink(f"/proc/{pid}/exe")).name
    except OSError:
        packager = False
    return (1 if packager else 2, pid)


GDB_SCRIPT = r'''
import gdb, json, os, re
for thread in list(gdb.selected_inferior().threads())[:128]:
    thread.switch()
    frames = []
    frame = gdb.newest_frame()
    for depth in range(48):
        if frame is None: break
        row = {"depth": depth}
        try:
            name = frame.name() or "unknown"
            row["pc"] = hex(frame.pc())
            row["function"] = name if re.fullmatch(r"[A-Za-z0-9_:<> ,.&*~()\[\]{}#=-]{1,240}", name) else "redacted-symbol"
            sal = frame.find_sal()
            if sal.symtab:
                name = os.path.basename(sal.symtab.filename)
                row["file"] = name if re.fullmatch(r"[A-Za-z0-9_.-]{1,96}", name) else "redacted-file"
                row["line"] = int(sal.line)
        except gdb.error: row["unavailable"] = True
        frames.append(row)
        try: frame = frame.older()
        except gdb.error: break
    print("DRMR_STACK " + json.dumps({"thread": int(thread.num), "frames": frames}))
'''


def stacks(pid, remaining):
    arguments = ["gdb", "--nx", "--quiet", "--batch", "-iex", "set auto-load off",
                 "-iex", "set debuginfod enabled off", "-p", str(pid),
                 "-ex", "set pagination off", "-ex", "set print frame-arguments none",
                 "-ex", "set print entry-values no", "-ex", "python exec(" + repr(GDB_SCRIPT) + ")",
                 "-ex", "detach"]
    try:
        with tempfile.TemporaryFile() as out:
            completed = subprocess.run(arguments, stdout=out, stderr=subprocess.DEVNULL,
                                       timeout=min(18, remaining), check=False)
            out.seek(0)
            text = out.read(2097153)
        if len(text) > 2097152:
            return {"pid": pid, "result": "debugger-output-limit", "returncode": completed.returncode}
        frames = []
        for line in text.decode(errors="replace").splitlines():
            if line.startswith("DRMR_STACK "):
                frames.append(json.loads(line[len("DRMR_STACK "):]))
        return {"pid": pid, "result": "captured", "returncode": completed.returncode, "threads": frames}
    except subprocess.TimeoutExpired:
        return {"pid": pid, "result": "debugger-timeout", "returncode": None}
    except (OSError, ValueError):
        return {"pid": pid, "result": "debugger-unavailable", "returncode": None}


def harness_lines(text, allowed):
    lines = []
    summary = None
    for line in text.splitlines():
        match = re.fullmatch(r"test ([A-Za-z0-9_:]+) \.\.\. (ok|FAILED|ignored)", line)
        if match and match[1] in allowed:
            lines.append(line)
        elif re.fullmatch(r"running \d+ tests?", line):
            lines.append(line)
        elif (match := SUMMARY.fullmatch(line)):
            lines.append(line)
            summary = {"status": match[1], "passed": int(match[2]), "failed": int(match[3]),
                       "ignored": int(match[4]), "measured": int(match[5]), "filtered": int(match[6])}
    return lines, summary


def run_case(binary, names, label, output):
    owner = Owner()
    started = time.monotonic()
    result = {"name": label, "expected_tests": len(names), "passed": False}
    child = None
    captured = False
    diagnosed = False
    with tempfile.TemporaryFile() as raw:
        try:
            args = [str(binary), "--nocapture", "--test-threads=4"]
            child = subprocess.Popen(args, stdin=subprocess.DEVNULL, stdout=raw,
                                     stderr=subprocess.STDOUT, start_new_session=True)
            while True:
                owner.scan()
                if child.poll() is not None:
                    break
                elapsed = time.monotonic() - started
                if STOP or elapsed >= 180:
                    result["deadline_exceeded"] = not STOP
                    result["interrupted"] = STOP
                    break
                if raw.tell() > 8388608:
                    result["output_limit"] = True
                    break
                if elapsed >= 60 and not captured:
                    captured = True
                    save(output / (label + "-processes.json"), snapshot(owner))
                if elapsed >= 90 and not diagnosed:
                    diagnosed = True
                    save(output / (label + "-gdb-processes.json"), snapshot(owner))
                    # A global 55-second debugger budget stays inside this run's 180 seconds.
                    until = min(started + 175, time.monotonic() + 55)
                    for pid in sorted(owner.alive(), key=lambda value: stack_priority(value, child.pid))[:3]:
                        if STOP or time.monotonic() >= until:
                            break
                        current = read_stat(pid)
                        if current and current["start_ticks"] == owner.rows[pid]["start_ticks"]:
                            save(output / (label + f"-stacks-{pid}.json"), stacks(pid, until - time.monotonic()))
                    continue
                time.sleep(0.1)
        finally:
            try:
                result["cleanup_confirmed"] = owner.cleanup(child) if child else True
            except OSError:
                result["cleanup_confirmed"] = False
            result["returncode"] = child.returncode if child else None
            raw.seek(0)
            text = raw.read(8388608).decode(errors="replace")
            lines, summary = harness_lines(text, names)
            (output / (label + ".log")).write_text("\n".join(lines) + "\n")
            result["harness_result"] = summary
            completed = [line for line in lines if line.startswith("test ") and line.endswith(" ... ok")]
            result["elapsed_seconds"] = round(time.monotonic() - started, 3)
            result["snapshot_taken"] = captured
            result["gdb_taken"] = diagnosed
            result["passed"] = (result["returncode"] == 0 and result["cleanup_confirmed"]
                and summary is not None and summary["status"] == "ok" and summary["passed"] == len(names)
                and summary["failed"] == summary["ignored"] == summary["measured"] == 0
                and summary["filtered"] == 0
                and len(completed) == len(names)
                and {line[5:-7] for line in completed} == names
                and not result.get("deadline_exceeded") and not result.get("interrupted")
                and not result.get("output_limit"))
            save(output / (label + "-result.json"), result)
    return result


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--build-log", required=True, type=Path)
    parser.add_argument("--output", required=True, type=Path)
    parser.add_argument("--channel", choices=["minimum", "stable"], required=True)
    args = parser.parse_args()
    args.output.mkdir(parents=True, exist_ok=True)
    report = {"passed": False, "runs": []}
    began = time.monotonic()
    try:
        if sys.platform != "linux" or not hasattr(os, "pidfd_open") or not hasattr(signal, "pidfd_send_signal"):
            raise RuntimeError("linux-pidfds-required")
        # Adopt orphaned grandchildren so detached tool groups remain discoverable.
        if ctypes.CDLL(None, use_errno=True).prctl(36, 1, 0, 0, 0) != 0:
            raise RuntimeError("subreaper-unavailable")
        build_text = args.build_log.read_text()
        binaries = {name: select_binary(build_text, name) for name in TARGETS}
        save(args.output / "receipt.json", identity(binaries, args.build_log, args.channel))
        selected = {}
        for target, binary in binaries.items():
            listing = command([str(binary), "--list", "--format", "terse"])
            names = {line[:-6] for line in listing.splitlines() if line.endswith(": test") and SAFE_NAME.fullmatch(line[:-6])}
            if len(names) != TARGETS[target]:
                raise RuntimeError("test-selection-mismatch")
            selected[target] = names
        report["selected_tests"] = {target: sorted(names) for target, names in selected.items()}
        for round_number in (1, 2):
            # Reserve both complete 180-second runs and cleanup before repeating the pair.
            if STOP or time.monotonic() - began >= 575 - 2 * 185:
                report["repeat_skipped_for_budget"] = round_number == 2
                break
            for target, binary in binaries.items():
                result = run_case(binary, selected[target], f"round-{round_number}-{target}", args.output)
                report["runs"].append(result)
                save(args.output / "report.json", report)
                print(json.dumps(result), flush=True)
                if not result["passed"]:
                    break
            if not all(row["passed"] for row in report["runs"]):
                break
        report["passed"] = len(report["runs"]) in (2, 4) and all(row["passed"] for row in report["runs"]) and not STOP
    except Exception as error:
        report["error_type"] = type(error).__name__
    finally:
        report["elapsed_seconds"] = round(time.monotonic() - began, 3)
        save(args.output / "report.json", report)
    return 0 if report["passed"] else 1


def interrupted(_number, _frame):
    global STOP
    STOP = True


if __name__ == "__main__":
    signal.signal(signal.SIGTERM, interrupted)
    signal.signal(signal.SIGINT, interrupted)
    raise SystemExit(main())
