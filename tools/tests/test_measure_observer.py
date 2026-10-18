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
