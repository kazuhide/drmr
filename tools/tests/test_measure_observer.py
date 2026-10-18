"""Process-generation attribution and bounded observer completion, without media secrets."""
import importlib.util
import io
import ctypes
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import time
import unittest
from unittest import mock

SPEC = importlib.util.spec_from_file_location(
    "measurement_observer", Path(__file__).resolve().parents[1] / "measurement_observer.py"
)
observer = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(observer)


class Handle:
    def __init__(self, state):
        self.state = state
        self.closed = False

    def times(self):
        return self.state["birth"], self.state.get("exit", 0)

    def resident_bytes(self):
        return self.state["bytes"]

    def is_running(self):
        return not self.state.get("exit", 0)

    def close(self):
        self.closed = True


class Api:
    def __init__(self):
        self.states = {10: {"birth": 100, "bytes": 1000},
                       20: {"birth": 101, "bytes": 2000},
                       30: {"birth": 102, "bytes": 3000},
                       40: {"birth": 99, "bytes": 4000},
                       50: {"birth": 102, "bytes": 5000}}
        self.rows = [(10, 1), (20, 10), (30, 999), (40, 10), (50, 20)]
        self.handles = []

    def open(self, pid):
        handle = Handle(self.states[pid])
        self.handles.append(handle)
        return handle

    def snapshot(self):
        return self.rows


class AttributionTests(unittest.TestCase):
    def test_a_provider_snapshot_cap_is_a_sticky_limit_not_an_ordinary_query_failure(self):
        api = Api()
        api.rows = [(10, 1), (20, 10)]
        sampler = observer.Sampler(api, 10, 100, 0.1)
        try:
            sampler.sample()
            class Entry(ctypes.Structure):
                _fields_ = [("size", ctypes.c_uint32), ("pid", ctypes.c_uint32), ("parent", ctypes.c_uint32)]
            class Kernel:
                closed = False
                def CreateToolhelp32Snapshot(self, *_):
                    return 99
                def Process32FirstW(self, _handle, entry):
                    entry._obj.pid, entry._obj.parent = 10, 1
                    return True
                def Process32NextW(self, _handle, entry):
                    entry._obj.pid += 1
                    return True
                def CloseHandle(self, _handle):
                    self.closed = True
                    return True
            provider = object.__new__(observer.WindowsApi)
            provider.Entry, provider.kernel = Entry, Kernel()
            api.snapshot = provider.snapshot
            with mock.patch.object(observer, "MAX_SNAPSHOT_ROWS", 2):
                sampler.sample()
            self.assertTrue(provider.kernel.closed)
            value = sampler.summary("observer-deadline")
            self.assertEqual(value["status"], "partial")
            self.assertEqual(value["reason"], "observation-limit")
            self.assertEqual(value["failed_samples"], 1)
            self.assertEqual(value["descendant_sampled_peak_bytes"], 2000)
        finally:
            sampler.close()

    def test_counter_nullable_and_status_inconsistencies_are_invalid(self):
        value = observer.missing("no-process-samples")
        observer.record(value, 100, 200, 1)
        value = observer.summarize(value)
        mutations = (("samples", 2), ("parent_zero_samples", 2), ("incomplete_samples", 2),
                     ("positive_descendant_memory_samples", 0), ("failed_samples", True),
                     ("failed_samples", 2**63), ("parent_sampled_peak_bytes", None),
                     ("parent_sampled_peak_bytes", 0), ("descendant_sampled_peak_bytes", None),
                     ("observed_peak_descendant_count", None), ("metric_kind", []),
                     ("reason", "provider-query-failed"), ("reason", "observation-limit"))
        for name, changed in mutations:
            with self.subTest(name=name, changed=changed), self.assertRaises(observer.Unavailable):
                observer.validate(dict(value, **{name: changed}))
        for numbers in ((-1, 0, 0), (True, 1, 1), (100, -1, 1), (100, 1, 0),
                        (100, 1, -1), (2**63, 1, 1), (100, 1, True)):
            fresh = observer.missing("no-process-samples")
            with self.subTest(numbers=numbers), self.assertRaises(observer.Unavailable):
                observer.record(fresh, *numbers)
            self.assertEqual(fresh["samples"], 0)
        value["failed_samples"] = 1
        failed = observer.summarize(value)
        self.assertEqual(failed["status"], "partial")
        self.assertEqual(failed["reason"], "provider-query-failed")
        self.assertEqual(failed["descendant_sampled_peak_bytes"], 200)
        for bad in (dict(observer.missing("provider-query-failed"), failed_samples=2**63 - 1, root_missing_samples=1),
                    dict(observer.missing("no-process-samples"), identity_rejections=1)):
            with self.assertRaises(observer.Unavailable):
                observer.validate(bad)

    def test_missing_zero_and_failed_memory_observations_are_distinct_and_never_peak_zeroes(self):
        for kind in ("no-child", "zero-child", "root-missing", "query-failed", "all-zero"):
            api = Api()
            api.rows = [(10, 1), (20, 10)]
            if kind == "no-child":
                api.rows = [(10, 1)]
            elif kind == "zero-child":
                api.states[20]["bytes"] = 0
            elif kind == "root-missing":
                api.rows = []
            elif kind == "query-failed":
                def unavailable():
                    raise observer.Unavailable("observer-query-unavailable")
                api.snapshot = unavailable
            else:
                api.states[10]["bytes"] = api.states[20]["bytes"] = 0
            sampler = observer.Sampler(api, 10, 100, 0.1)
            try:
                sampler.sample()
                value = sampler.summary()
                with self.subTest(kind=kind):
                    self.assertIsNone(value["descendant_sampled_peak_bytes"])
                    self.assertEqual(value["positive_descendant_memory_samples"], 0)
                    if kind in ("no-child", "zero-child"):
                        self.assertEqual(value["status"], "partial")
                    else:
                        self.assertEqual(value["status"], "unmeasured")
                    if kind == "no-child":
                        self.assertIsNone(value["observed_peak_descendant_count"])
                        self.assertEqual(value["no_descendant_observation_samples"], 1)
                        self.assertEqual(value["reason"], "descendant-memory-not-observed")
                    elif kind in ("zero-child", "all-zero"):
                        self.assertEqual(value["observed_peak_descendant_count"], 1)
                        self.assertEqual(value["zero_descendant_memory_samples"], 1)
                        self.assertEqual(value["reason"], "descendant-rss-zero-only" if kind == "zero-child" else "no-positive-memory-samples")
                    elif kind == "root-missing":
                        self.assertEqual(value["root_missing_samples"], 1)
                        self.assertEqual(value["failed_samples"], 0)
                        self.assertEqual(value["reason"], "root-not-observed")
                    else:
                        self.assertEqual(value["failed_samples"], 1)
                        self.assertEqual(value["root_missing_samples"], 0)
                        self.assertEqual(value["reason"], "provider-query-failed")
            finally:
                sampler.close()

    def test_exhausted_ancestry_budget_reports_the_known_missing_descendants(self):
        api = Api()
        api.states = {pid: {"birth": pid + 100, "bytes": 1} for pid in range(10, 81)}
        api.rows = [(pid, pid - 1) for pid in reversed(range(11, 81))] + [(10, 1)]
        sampler = observer.Sampler(api, 10, 110, 0.1)
        try:
            sampler.sample()
            result = sampler.summary()
            self.assertEqual(result["status"], "partial")
            self.assertEqual(result["reason"], "observation-limit")
            self.assertGreater(result["incomplete_samples"], 0)
        finally:
            sampler.close()

    def test_running_exit_time_is_undefined_and_never_used_as_an_edge_boundary(self):
        class FileTime(ctypes.Structure):
            _fields_ = [("dwLowDateTime", ctypes.c_uint32), ("dwHighDateTime", ctypes.c_uint32)]
        class Types:
            FILETIME = FileTime
        class Kernel:
            calls = 0
            status = 258
            def GetProcessTimes(self, _handle, creation, exited, _kernel, _user):
                self.calls += 1
                creation._obj.dwLowDateTime = 100
                exited._obj.dwLowDateTime = 999 if self.calls == 1 else 150
                return True
            def WaitForSingleObject(self, _handle, _timeout):
                return self.status
        api = object.__new__(observer.WindowsApi)
        api.w, api.kernel = Types(), Kernel()
        self.assertEqual(api.times(1), (100, 0))
        api.kernel = Kernel()
        api.kernel.status = 0
        self.assertEqual(api.times(1), (100, 150))
        self.assertEqual(api.kernel.calls, 2)

    def test_only_descendants_with_valid_parent_generations_are_counted(self):
        api = Api()
        sampler = observer.Sampler(api, 10, 100, 0.1)
        try:
            sampler.sample()
            result = sampler.summary()
            self.assertEqual(result["parent_sampled_peak_bytes"], 1000)
            self.assertEqual(result["descendant_sampled_peak_bytes"], 7000)
            self.assertEqual(result["observed_peak_descendant_count"], 2)
            self.assertEqual(result["identity_rejections"], 1)
            self.assertEqual(result["status"], "partial")
        finally:
            sampler.close()
        self.assertTrue(all(handle.closed for handle in api.handles))

    def test_child_born_after_its_known_parent_exited_is_excluded(self):
        api = Api()
        api.rows = [(10, 1), (20, 10)]
        sampler = observer.Sampler(api, 10, 100, 0.1)
        try:
            sampler.sample()
            api.states[20]["exit"] = 103
            api.states[50]["birth"] = 104
            api.rows.append((50, 20))
            sampler.sample()
            result = sampler.summary()
            self.assertEqual(result["descendant_sampled_peak_bytes"], 2000)
            self.assertGreater(result["identity_rejections"], 0)
        finally:
            sampler.close()

    def test_reused_pid_and_wrong_root_generation_never_acquire_foreign_memory(self):
        api = Api()
        with self.assertRaises(observer.Unavailable):
            observer.Sampler(api, 10, 99, 0.1)
        self.assertTrue(api.handles[-1].closed)
        api.rows = [(10, 1), (20, 10)]
        sampler = observer.Sampler(api, 10, 100, 0.1)
        try:
            sampler.sample()
            api.states[20] = {"birth": 999, "bytes": 999999999}
            sampler.sample()
            self.assertEqual(sampler.summary()["descendant_sampled_peak_bytes"], 2000)
            self.assertGreater(sampler.summary()["identity_rejections"], 0)
        finally:
            sampler.close()

    def test_unobserved_is_null_and_summary_validation_is_exact(self):
        sampler = observer.Sampler(Api(), 10, 100, 0.1)
        try:
            value = sampler.summary()
            self.assertEqual(value["status"], "unmeasured")
            self.assertIsNone(value["parent_sampled_peak_bytes"])
            self.assertEqual(observer.validate(value), value)
            bad = dict(value, argv="never-retain-this")
            with self.assertRaises(observer.Unavailable):
                observer.validate(bad)
            bad = dict(value, sample_interval_seconds=float("nan"))
            with self.assertRaises(observer.Unavailable):
                observer.validate(bad)
        finally:
            sampler.close()


class CompletionTests(unittest.TestCase):
    def test_windows_finish_refuses_a_valid_unix_memory_payload(self):
        value = observer.missing("no-process-samples", metric_kind="unix-rss")
        observer.record(value, 100, 200, 1)
        value = observer.summarize(value)
        class Finished:
            returncode = 0
            stdout = io.BytesIO((json.dumps(value) + "\n").encode("ascii"))
            def wait(self, timeout):
                return 0
            def poll(self):
                return 0
        result = observer.finish(Finished())
        self.assertEqual(result["status"], "unmeasured")
        self.assertEqual(result["reason"], "observer-output-invalid")
        self.assertEqual(result["metric_kind"], "windows-working-set")

    def test_bounded_large_integer_and_deep_json_are_fixed_invalid_reports(self):
        value = observer.missing("no-process-samples")
        value["sample_interval_seconds"] = 10**400
        raw_values = [(json.dumps(value) + "\n").encode("ascii"), b"[" * 1000 + b"]" * 1000 + b"\n"]
        for raw in raw_values:
            self.assertLessEqual(len(raw), observer.MAX_SUMMARY_BYTES)
            class Finished:
                returncode = 0
                stdout = io.BytesIO(raw)
                def wait(self, timeout):
                    return 0
                def poll(self):
                    return 0
            result = observer.finish(Finished())
            self.assertEqual(result["reason"], "observer-output-invalid")
            self.assertEqual(result["observer_cleanup"], "confirmed-exit")

    def start(self, body):
        return subprocess.Popen([sys.executable, "-c", body], stdin=subprocess.DEVNULL,
                                stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, close_fds=True)

    def test_oversized_or_additional_output_is_invalid_after_confirmed_exit(self):
        class Finished:
            returncode = 0
            stdout = io.BytesIO(b"x" * 2049)
            def wait(self, timeout):
                return 0
            def poll(self):
                return 0
        value = observer.finish(Finished(), 2)
        self.assertEqual(value["reason"], "observer-output-invalid")
        process = self.start("print('{}');print('{}')")
        value = observer.finish(process, 2)
        self.assertEqual(value["status"], "unmeasured")
        self.assertEqual(value["reason"], "observer-output-invalid")
        self.assertIsNotNone(process.returncode)

    def test_timeout_terminates_only_the_observer_before_any_pipe_read(self):
        process = self.start("import time;time.sleep(30)")
        value = observer.finish(process, 0.05)
        self.assertEqual(value["status"], "unmeasured")
        self.assertEqual(value["reason"], "observer-timeout")
        self.assertIsNotNone(process.returncode)

    def test_unconfirmed_stop_never_reads_the_pipe_or_claims_media_cleanup(self):
        class Pipe:
            closed = False
            def read(self, _limit):
                raise AssertionError("must not read from an unconfirmed writer")
            def close(self):
                self.closed = True
        class Blocked:
            stdout = Pipe()
            def wait(self, timeout):
                raise subprocess.TimeoutExpired("observer", timeout)
            def terminate(self):
                pass
        process = Blocked()
        value = observer.finish(process, 0.05)
        self.assertEqual(value["reason"], "observer-cleanup-unconfirmed")
        self.assertEqual(value["observer_cleanup"], "unconfirmed")
        self.assertTrue(process.stdout.closed)


@unittest.skipUnless(os.name == "nt", "Native Windows process-query and pipe contracts")
class WindowsNativeTests(unittest.TestCase):
    def start(self, body):
        return CompletionTests.start(self, body)

    def test_real_held_parent_and_child_working_sets_and_normal_summary_pipe(self):
        api = observer.WindowsApi()
        root = api.open(os.getpid())
        creation = root.times()[0]
        root.close()
        directory = tempfile.TemporaryDirectory()
        ready = Path(directory.name) / "ready"
        child = subprocess.Popen([sys.executable, "-c",
            "import pathlib,sys;data=bytearray(64*1024*1024);data[::4096]=b'x'*len(data[::4096]);"
            "pathlib.Path(sys.argv[1]).write_bytes(b'ready');sys.stdin.buffer.read(1)", str(ready)], stdin=subprocess.PIPE,
            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, close_fds=True)
        sampler = observer.Sampler(api, os.getpid(), creation, 0.1)
        held = []
        try:
            deadline = time.monotonic() + 5
            while not ready.is_file():
                self.assertIsNone(child.poll())
                self.assertLess(time.monotonic(), deadline, "child readiness was not confirmed")
                time.sleep(0.005)
            sampler.sample()
            held = list(sampler.handles.values())
            result = sampler.summary()
            self.assertGreater(result["samples"], 0)
            self.assertGreater(result["parent_sampled_peak_bytes"], 0)
            self.assertGreater(result["descendant_sampled_peak_bytes"], 0)
            self.assertIn(child.pid, sampler.handles)
            raw = json.dumps(result, separators=(",", ":")) + "\n"
            self.assertLessEqual(len(raw.encode("ascii")), 2048)
            writer = self.start("import os;os.write(1," + repr(raw.encode("ascii")) + ")")
            parsed = observer.finish(writer, 2)
            self.assertEqual(parsed["observer_cleanup"], "confirmed-exit")
            self.assertGreater(parsed["samples"], 0)
            self.assertGreater(parsed["parent_sampled_peak_bytes"], 0)
            child.stdin.write(b"x")
            child.stdin.flush()
            self.assertEqual(child.wait(timeout=5), 0)
        finally:
            closed = sampler.close()
            if child.poll() is None:
                child.kill()
                child.wait(timeout=5)
            child.stdin.close()
            directory.cleanup()
        self.assertTrue(closed)
        self.assertTrue(held)
        self.assertTrue(all(handle.handle is None for handle in held))

    def test_actual_oversized_pipe_is_rejected_after_native_writer_exit(self):
        process = self.start("import os;os.write(1,b'x'*2049)")
        result = observer.finish(process, 2)
        self.assertEqual(result["reason"], "observer-output-invalid")
        self.assertEqual(result["observer_cleanup"], "confirmed-exit")


if __name__ == "__main__":
    unittest.main()
