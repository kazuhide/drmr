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
SCOPES = {
    "windows-working-set": "observed-parent-generations;sequential-pass-not-atomic;short-lived-may-be-missed",
    "unix-rss": "ppid-process-table-samples-no-generation-proof",
}
COUNTERS = ("samples", "failed_samples", "root_missing_samples", "parent_zero_samples",
            "no_descendant_observation_samples", "zero_descendant_memory_samples",
            "positive_descendant_memory_samples", "incomplete_samples", "identity_rejections")
PEAKS = ("parent_sampled_peak_bytes", "descendant_sampled_peak_bytes", "observed_peak_descendant_count")
EXTERNAL_REASONS = {"observer-timeout", "observer-output-invalid", "observer-cleanup-unconfirmed",
                    "observer-unavailable", "process-identity-mismatch", "observer-query-unavailable"}
REASONS = EXTERNAL_REASONS | {"no-process-samples", "observation-limit", "observer-deadline",
                            "provider-query-failed", "root-not-observed", "no-positive-memory-samples",
                            "parent-memory-not-observed", "descendant-rss-zero-only",
                            "descendant-memory-not-observed", "incomplete-process-observation"}


class Unavailable(Exception):
    """A fixed measurement failure; never forward operating-system diagnostics."""


class ObservationLimit(Unavailable):
    """A bounded provider or attribution traversal could not observe every row."""


class IdentityMismatch(Unavailable):
    """The retained root does not have the expected creation identity."""


def missing(reason, interval=0.1, metric_kind="windows-working-set"):
    return {"schema_version": 1, "metric_kind": metric_kind, "scope": SCOPES[metric_kind],
            "status": "unmeasured", "reason": reason, "sample_interval_seconds": interval,
            **{name: 0 for name in COUNTERS}, **{name: None for name in PEAKS}}


def record(value, parent_bytes, descendant_bytes, child_count, incomplete=False):
    """Record observed rows; zero resident rows never establish a lifetime peak of zero."""
    if (any(type(number) is not int or not 0 <= number <= 2**63 - 1
            for number in (parent_bytes, descendant_bytes, child_count))
            or (child_count == 0 and descendant_bytes != 0)):
        raise Unavailable("provider-query-failed")
    value["samples"] += 1
    if parent_bytes == 0:
        value["parent_zero_samples"] += 1
    else:
        value["parent_sampled_peak_bytes"] = max(value["parent_sampled_peak_bytes"] or 0, parent_bytes)
    if child_count == 0:
        value["no_descendant_observation_samples"] += 1
    else:
        value["observed_peak_descendant_count"] = max(value["observed_peak_descendant_count"] or 0, child_count)
        if descendant_bytes == 0:
            value["zero_descendant_memory_samples"] += 1
        else:
            value["positive_descendant_memory_samples"] += 1
            value["descendant_sampled_peak_bytes"] = max(value["descendant_sampled_peak_bytes"] or 0, descendant_bytes)
    if incomplete:
        value["incomplete_samples"] += 1


def _reason(value, override=None):
    if override in ("observation-limit", "observer-deadline"):
        return override
    for condition, reason in (
            (value["failed_samples"] > 0, "provider-query-failed"),
            (value["root_missing_samples"] > 0, "root-not-observed"),
            (value["samples"] == 0, "no-process-samples"),
            (all(value[name] is None for name in PEAKS[:2]), "no-positive-memory-samples"),
            (value[PEAKS[0]] is None, "parent-memory-not-observed"),
            (value[PEAKS[1]] is None and value[PEAKS[2]] is not None, "descendant-rss-zero-only"),
            (value[PEAKS[1]] is None, "descendant-memory-not-observed"),
            (any(value[name] for name in ("incomplete_samples", "identity_rejections", "parent_zero_samples",
                                          "zero_descendant_memory_samples")), "incomplete-process-observation")):
        if condition:
            return reason
    return None


def summarize(value, override=None):
    result = dict(value)
    result["reason"] = _reason(result, override)
    result["status"] = ("unmeasured" if all(result[name] is None for name in PEAKS[:2]) else
                        "partial" if result["reason"] is not None else "sampled")
    return validate(result)


def validate(value):
    if not isinstance(value, dict) or type(value.get("metric_kind")) is not str or value["metric_kind"] not in SCOPES:
        raise Unavailable("observer-output-invalid")
    template = missing("no-process-samples", metric_kind=value["metric_kind"])
    if set(value) != set(template):
        raise Unavailable("observer-output-invalid")
    for name in ("schema_version", "metric_kind", "scope"):
        if type(value[name]) is not type(template[name]) or value[name] != template[name]:
            raise Unavailable("observer-output-invalid")
    interval = value["sample_interval_seconds"]
    if type(interval) not in (int, float) or not 0.02 <= interval <= 10 or not math.isfinite(interval):
        raise Unavailable("observer-output-invalid")
    if type(value["status"]) is not str or value["status"] not in ("sampled", "partial", "unmeasured"):
        raise Unavailable("observer-output-invalid")
    reason = value["reason"]
    if reason is not None and (type(reason) is not str or reason not in REASONS):
        raise Unavailable("observer-output-invalid")
    for name in COUNTERS:
        if type(value[name]) is not int or not 0 <= value[name] <= 2**63 - 1:
            raise Unavailable("observer-output-invalid")
    for name in PEAKS:
        if value[name] is not None and (type(value[name]) is not int or not 0 < value[name] <= 2**63 - 1):
            raise Unavailable("observer-output-invalid")
    if (sum(value[name] for name in ("no_descendant_observation_samples", "zero_descendant_memory_samples",
                                     "positive_descendant_memory_samples")) != value["samples"]
            or value["parent_zero_samples"] > value["samples"] or value["incomplete_samples"] > value["samples"]):
        raise Unavailable("observer-output-invalid")
    attempts = value["samples"] + value["failed_samples"] + value["root_missing_samples"]
    if attempts > 2**63 - 1 or (value["identity_rejections"] and attempts == 0):
        raise Unavailable("observer-output-invalid")
    if ((value[PEAKS[0]] is None) != (value["samples"] == value["parent_zero_samples"])
            or (value[PEAKS[1]] is None) != (value["positive_descendant_memory_samples"] == 0)
            or (value[PEAKS[2]] is None) != (value["zero_descendant_memory_samples"] + value["positive_descendant_memory_samples"] == 0)):
        raise Unavailable("observer-output-invalid")
    if reason in EXTERNAL_REASONS:
        if any(value[name] for name in COUNTERS) or any(value[name] is not None for name in PEAKS) or value["status"] != "unmeasured":
            raise Unavailable("observer-output-invalid")
    else:
        if reason == "observation-limit" and not (value["incomplete_samples"] or value["failed_samples"]):
            raise Unavailable("observer-output-invalid")
        expected_reason = _reason(value, reason if reason in ("observation-limit", "observer-deadline") else None)
        expected_status = ("unmeasured" if all(value[name] is None for name in PEAKS[:2]) else
                           "partial" if expected_reason is not None else "sampled")
        if reason != expected_reason or value["status"] != expected_status:
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
                raise IdentityMismatch("process-identity-mismatch")
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
                raise ObservationLimit("observation-limit")
            pending = dict(rows)
            if self.root_pid not in pending:
                self.value["root_missing_samples"] += 1
                return
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
                                raise ObservationLimit("observation-limit")
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
            record(self.value, parent_bytes, children_bytes, child_count, incomplete)
        except (OSError, Unavailable, KeyError) as problem:
            if isinstance(problem, ObservationLimit):
                self.limit_hit = True
            self.value["failed_samples"] += 1

    def summary(self, reason=None):
        return summarize(self.value, "observation-limit" if self.limit_hit else reason)

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


def finish(process, timeout=2, interval=0.1):
    """Read only after the sole writer has exited; never communicate or start reader threads."""
    if process is None:
        result = missing("observer-unavailable", interval)
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
                result = missing("observer-cleanup-unconfirmed", interval)
                result["observer_cleanup"] = "unconfirmed"
                return result
            result = missing("observer-timeout", interval)
            result["observer_cleanup"] = "confirmed-termination"
            return result
        raw = process.stdout.read(MAX_SUMMARY_BYTES + 1)
        if process.returncode or len(raw) > MAX_SUMMARY_BYTES or not raw.endswith(b"\n") or raw.count(b"\n") != 1:
            raise Unavailable("observer-output-invalid")
        value = json.loads(raw.decode("ascii"), object_pairs_hook=_pairs)
        value = validate(value)
        if value["metric_kind"] != "windows-working-set":
            raise Unavailable("observer-output-invalid")
        value["observer_cleanup"] = "confirmed-exit"
        return value
    except (OSError, ValueError, UnicodeError, OverflowError, RecursionError, Unavailable):
        stopped = process.poll() is not None
        result = missing("observer-output-invalid" if stopped else "observer-cleanup-unconfirmed", interval)
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
                    raise ObservationLimit("observation-limit")
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
    except IdentityMismatch:
        result = missing("process-identity-mismatch", args.interval)
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
