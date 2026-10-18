"""Read-only Windows working-set samples, with bounded, private observer output.

The observer creates no children or threads. Process identities are internal only.
Python 3.9+ and the operating system's existing DLLs are sufficient.
"""
import argparse
import ctypes
import json
import math
import os
from pathlib import Path
import subprocess
import sys
import time

MAX_SUMMARY_BYTES = 2048
MAX_PROCESSES = 4096
MAX_SNAPSHOT_ROWS = 65536
REASONS = {"no-process-samples", "observer-query-unavailable", "process-identity-mismatch",
           "observation-limit", "observer-deadline", "incomplete-process-observation",
           "observer-timeout", "observer-output-invalid", "observer-cleanup-unconfirmed",
           "observer-unavailable"}


class Unavailable(Exception):
    """A fixed measurement failure; never forward operating-system diagnostics."""


def missing(reason, interval=0.1):
    return {"schema_version": 1, "metric_kind": "windows-working-set",
            "scope": "observed-parent-generations;sequential-pass-not-atomic;short-lived-may-be-missed",
            "status": "unmeasured", "reason": reason, "sample_interval_seconds": interval,
            "samples": 0, "failed_samples": 0, "incomplete_samples": 0,
            "identity_rejections": 0, "parent_sampled_peak_bytes": None,
            "descendant_sampled_peak_bytes": None, "observed_peak_descendant_count": None}


def validate(value):
    template = missing("no-process-samples")
    if not isinstance(value, dict) or set(value) != set(template):
        raise Unavailable("observer-output-invalid")
    for name in ("schema_version", "metric_kind", "scope"):
        if type(value[name]) is not type(template[name]) or value[name] != template[name]:
            raise Unavailable("observer-output-invalid")
    interval = value["sample_interval_seconds"]
    if type(interval) not in (int, float) or not 0.02 <= interval <= 10 or not math.isfinite(interval):
        raise Unavailable("observer-output-invalid")
    if type(value["status"]) is not str or value["status"] not in ("sampled", "partial", "unmeasured"):
        raise Unavailable("observer-output-invalid")
    if value["reason"] is not None and (type(value["reason"]) is not str or value["reason"] not in REASONS):
        raise Unavailable("observer-output-invalid")
    for name in ("samples", "failed_samples", "incomplete_samples", "identity_rejections"):
        if type(value[name]) is not int or not 0 <= value[name] <= 2**63 - 1:
            raise Unavailable("observer-output-invalid")
    for name in ("parent_sampled_peak_bytes", "descendant_sampled_peak_bytes", "observed_peak_descendant_count"):
        expected_null = value["samples"] == 0
        if expected_null:
            if value[name] is not None:
                raise Unavailable("observer-output-invalid")
        elif type(value[name]) is not int or not 0 <= value[name] <= 2**63 - 1:
            raise Unavailable("observer-output-invalid")
    if (value["samples"] == 0) != (value["status"] == "unmeasured"):
        raise Unavailable("observer-output-invalid")
    if value["status"] != "sampled" and value["reason"] is None:
        raise Unavailable("observer-output-invalid")
    if value["status"] == "sampled" and (value["reason"] is not None or any(
            value[name] for name in ("failed_samples", "incomplete_samples", "identity_rejections"))):
        raise Unavailable("observer-output-invalid")
    return value


class Sampler:
    def __init__(self, api, root_pid, creation, interval):
        self.api = api
        self.root_pid = root_pid
        self.handles = {}
        self.value = missing("no-process-samples", interval)
        self.limit_hit = False
        root = api.open(root_pid)
        try:
            if root.times()[0] != creation:
                raise Unavailable("process-identity-mismatch")
        except BaseException:
            root.close()
            raise
        self.handles[root_pid] = root

    @property
    def root(self):
        return self.handles[self.root_pid]

    def sample(self):
        incomplete = False
        try:
            rows = self.api.snapshot()
            if len(rows) > MAX_SNAPSHOT_ROWS:
                self.limit_hit = True
                raise Unavailable("observation-limit")
            pending = dict(rows)
            if self.root_pid not in pending:
                raise Unavailable("observer-query-unavailable")
            seen = {self.root_pid}
            rejected = set()
            for _ in range(64):
                changed = False
                for pid, parent_pid in list(pending.items()):
                    if pid in seen or parent_pid not in self.handles:
                        continue
                    seen.add(pid)
                    changed = True
                    handle = None
                    try:
                        handle = self.api.open(pid)
                        birth, _ = handle.times()
                        parent_birth, parent_exit = self.handles[parent_pid].times()
                        if parent_pid in rejected:
                            self.value["identity_rejections"] += 1
                            incomplete = True
                            rejected.add(pid)
                            continue
                        if parent_pid in pending:
                            parent_now = self.api.open(parent_pid)
                            try:
                                if parent_now.times()[0] != parent_birth:
                                    self.value["identity_rejections"] += 1
                                    incomplete = True
                                    rejected.add(pid)
                                    continue
                            finally:
                                parent_now.close()
                        previous = self.handles.get(pid)
                        if (birth < parent_birth or (parent_exit and birth > parent_exit)
                                or (previous and previous.times()[0] != birth)):
                            self.value["identity_rejections"] += 1
                            incomplete = True
                            rejected.add(pid)
                            continue
                        if previous is None:
                            if len(self.handles) >= MAX_PROCESSES:
                                self.limit_hit = True
                                raise Unavailable("observation-limit")
                            self.handles[pid] = handle
                            handle = None
                    except (OSError, Unavailable, KeyError):
                        incomplete = True
                    finally:
                        if handle is not None:
                            handle.close()
                if not changed:
                    break
            else:
                incomplete = True
                self.limit_hit = True
            parent_bytes = self.root.resident_bytes()
            children_bytes = 0
            child_count = 0
            for pid in seen - {self.root_pid}:
                handle = self.handles.get(pid)
                # A rejected reused PID must not sample its older held generation.
                if handle is None or pid not in pending or pid in rejected:
                    continue
                try:
                    current = self.api.open(pid)
                    try:
                        if current.times()[0] != handle.times()[0]:
                            incomplete = True
                            continue
                    finally:
                        current.close()
                    if handle.is_running():
                        children_bytes += handle.resident_bytes()
                        child_count += 1
                except (OSError, Unavailable, KeyError):
                    incomplete = True
            self.value["samples"] += 1
            for name, value in (("parent_sampled_peak_bytes", parent_bytes),
                                ("descendant_sampled_peak_bytes", children_bytes),
                                ("observed_peak_descendant_count", child_count)):
                self.value[name] = max(self.value[name] or 0, value)
            if incomplete:
                self.value["incomplete_samples"] += 1
        except (OSError, Unavailable, KeyError):
            self.value["failed_samples"] += 1

    def summary(self, reason=None):
        reason = reason or ("observation-limit" if self.limit_hit else None)
        result = dict(self.value)
        if result["samples"]:
            partial = reason is not None or any(result[name] for name in (
                "failed_samples", "incomplete_samples", "identity_rejections"))
            result["status"] = "partial" if partial else "sampled"
            result["reason"] = reason or ("incomplete-process-observation" if partial else None)
        elif reason:
            result["reason"] = reason
        return validate(result)

    def close(self):
        closed = True
        for handle in self.handles.values():
            try:
                handle.close()
            except (OSError, Unavailable):
                closed = False
        self.handles.clear()
        return closed


def _pairs(values):
    result = dict(values)
    if len(result) != len(values):
        raise Unavailable("observer-output-invalid")
    return result


def finish(process, timeout=2):
    """Read only after the sole writer has exited; never communicate or start reader threads."""
    if process is None:
        result = missing("observer-unavailable")
        result["observer_cleanup"] = "not-started"
        return result
    try:
        try:
            process.wait(timeout=timeout)
        except subprocess.TimeoutExpired:
            process.terminate()
            try:
                process.wait(timeout=2)
            except subprocess.TimeoutExpired:
                result = missing("observer-cleanup-unconfirmed")
                result["observer_cleanup"] = "unconfirmed"
                return result
            result = missing("observer-timeout")
            result["observer_cleanup"] = "confirmed-termination"
            return result
        raw = process.stdout.read(MAX_SUMMARY_BYTES + 1)
        if process.returncode or len(raw) > MAX_SUMMARY_BYTES or not raw.endswith(b"\n") or raw.count(b"\n") != 1:
            raise Unavailable("observer-output-invalid")
        value = json.loads(raw.decode("ascii"), object_pairs_hook=_pairs)
        value = validate(value)
        value["observer_cleanup"] = "confirmed-exit"
        return value
    except (OSError, ValueError, UnicodeError, OverflowError, RecursionError, Unavailable):
        stopped = process.poll() is not None
        result = missing("observer-output-invalid" if stopped else "observer-cleanup-unconfirmed")
        result["observer_cleanup"] = "confirmed-exit" if stopped else "unconfirmed"
        return result
    finally:
        process.stdout.close()


class WindowsApi:
    def __init__(self):
        from ctypes import wintypes as w
        self.w = w
        self.kernel = ctypes.WinDLL("kernel32", use_last_error=True)

        class Entry(ctypes.Structure):
            _fields_ = [("size", w.DWORD), ("usage", w.DWORD), ("pid", w.DWORD),
                        ("heap", ctypes.c_size_t), ("module", w.DWORD), ("threads", w.DWORD),
                        ("parent", w.DWORD), ("priority", w.LONG), ("flags", w.DWORD),
                        ("name", w.WCHAR * 260)]

        class Memory(ctypes.Structure):
            _fields_ = [("size", w.DWORD), ("faults", w.DWORD)] + [
                (name, ctypes.c_size_t) for name in ("peak", "working", "peak_paged", "paged",
                                                    "peak_nonpaged", "nonpaged", "pagefile", "peak_pagefile")]
        self.Entry, self.Memory = Entry, Memory
        signatures = {
            "OpenProcess": ([w.DWORD, w.BOOL, w.DWORD], w.HANDLE),
            "CloseHandle": ([w.HANDLE], w.BOOL),
            "WaitForSingleObject": ([w.HANDLE, w.DWORD], w.DWORD),
            "GetProcessTimes": ([w.HANDLE] + [ctypes.POINTER(w.FILETIME)] * 4, w.BOOL),
            "K32GetProcessMemoryInfo": ([w.HANDLE, ctypes.POINTER(Memory), w.DWORD], w.BOOL),
            "CreateToolhelp32Snapshot": ([w.DWORD, w.DWORD], w.HANDLE),
            "Process32FirstW": ([w.HANDLE, ctypes.POINTER(Entry)], w.BOOL),
            "Process32NextW": ([w.HANDLE, ctypes.POINTER(Entry)], w.BOOL),
        }
        for name, (arguments, result) in signatures.items():
            function = getattr(self.kernel, name)
            function.argtypes, function.restype = arguments, result

    def times(self, handle):
        values = [self.w.FILETIME() for _ in range(4)]
        if not self.kernel.GetProcessTimes(handle, *[ctypes.byref(value) for value in values]):
            raise Unavailable("observer-query-unavailable")
        status = self.kernel.WaitForSingleObject(handle, 0)
        if status == 0:
            # ExitTime is undefined for a running process. Query again only after
            # its retained process object is signalled, including an exit race.
            if not self.kernel.GetProcessTimes(handle, *[ctypes.byref(value) for value in values]):
                raise Unavailable("observer-query-unavailable")
        elif status != 258:
            raise Unavailable("observer-query-unavailable")
        creation = (values[0].dwHighDateTime << 32) | values[0].dwLowDateTime
        exited = ((values[1].dwHighDateTime << 32) | values[1].dwLowDateTime) if status == 0 else 0
        return creation, exited

    def open(self, pid):
        handle = self.kernel.OpenProcess(0x1000 | 0x100000, False, pid)
        if not handle:
            raise Unavailable("observer-query-unavailable")
        return WindowsHandle(self, handle)

    def snapshot(self):
        snapshot = self.kernel.CreateToolhelp32Snapshot(2, 0)
        if snapshot in (None, ctypes.c_void_p(-1).value):
            raise Unavailable("observer-query-unavailable")
        try:
            entry = self.Entry()
            entry.size = ctypes.sizeof(entry)
            rows = []
            more = self.kernel.Process32FirstW(snapshot, ctypes.byref(entry))
            while more:
                if len(rows) >= MAX_SNAPSHOT_ROWS:
                    raise Unavailable("observation-limit")
                rows.append((entry.pid, entry.parent))
                more = self.kernel.Process32NextW(snapshot, ctypes.byref(entry))
            if ctypes.get_last_error() != 18:
                raise Unavailable("observer-query-unavailable")
            return rows
        finally:
            if not self.kernel.CloseHandle(snapshot):
                raise Unavailable("observer-query-unavailable")


class WindowsHandle:
    def __init__(self, api, handle):
        self.api, self.handle = api, handle

    def times(self):
        return self.api.times(self.handle)

    def resident_bytes(self):
        value = self.api.Memory()
        value.size = ctypes.sizeof(value)
        if not self.api.kernel.K32GetProcessMemoryInfo(self.handle, ctypes.byref(value), value.size):
            raise Unavailable("observer-query-unavailable")
        return value.working

    def is_running(self):
        value = self.api.kernel.WaitForSingleObject(self.handle, 0)
        if value not in (0, 258):
            raise Unavailable("observer-query-unavailable")
        return value == 258

    def wait(self, seconds):
        value = self.api.kernel.WaitForSingleObject(self.handle, max(0, int(seconds * 1000)))
        if value not in (0, 258):
            raise Unavailable("observer-query-unavailable")
        return value == 0

    def close(self):
        if self.handle is not None:
            if not self.api.kernel.CloseHandle(self.handle):
                raise Unavailable("observer-query-unavailable")
            self.handle = None


def start(process, interval, timeout, environment):
    """The CLI already exists and cannot inherit this observer's pipe writer."""
    try:
        api = WindowsApi()
        # Popen retains the handle returned by CreateProcess, not a PID lookup.
        creation = api.times(int(process._handle))[0]
        return subprocess.Popen([sys.executable, str(Path(__file__).resolve()),
                                 "--pid", str(process.pid), "--creation", str(creation),
                                 "--interval", str(interval), "--timeout", str(timeout)],
                                stdin=subprocess.DEVNULL, stdout=subprocess.PIPE,
                                stderr=subprocess.DEVNULL, close_fds=True, env=environment)
    except (OSError, Unavailable):
        return None


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--pid", type=int, required=True)
    parser.add_argument("--creation", type=int, required=True)
    parser.add_argument("--interval", type=float, required=True)
    parser.add_argument("--timeout", type=float, required=True)
    args = parser.parse_args()
    if (not 0 < args.pid <= 2**32 - 1 or not 0 < args.creation < 2**64
            or not math.isfinite(args.interval) or not 0.02 <= args.interval <= 10
            or not math.isfinite(args.timeout) or not 1 <= args.timeout <= 86410):
        return 1
    sampler = None
    try:
        sampler = Sampler(WindowsApi(), args.pid, args.creation, args.interval)
        deadline = time.monotonic() + args.timeout
        while sampler.root.is_running() and time.monotonic() < deadline:
            sampler.sample()
            sampler.root.wait(min(args.interval, max(0, deadline - time.monotonic())))
        reason = "observer-deadline" if sampler.root.is_running() else None
        result = sampler.summary(reason)
    except (OSError, Unavailable):
        result = missing("observer-query-unavailable", args.interval)
    finally:
        if sampler and not sampler.close():
            result = missing("observer-query-unavailable", args.interval)
    encoded = (json.dumps(result, separators=(",", ":"), ensure_ascii=True, allow_nan=False) + "\n").encode("ascii")
    if len(encoded) > MAX_SUMMARY_BYTES:
        return 1
    return 0 if os.write(1, encoded) == len(encoded) else 1


if __name__ == "__main__":
    raise SystemExit(main())
