"""Exercise bounded native scheduling with real subprocesses and failures."""
import importlib.util
from pathlib import Path
import os
import sys
import tempfile
import unittest
from unittest import mock


PATH = Path(__file__).resolve().parents[2] / ".github/scripts/run-native-tests.py"
SPEC = importlib.util.spec_from_file_location("run_native_tests", PATH)
RUNNER = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(RUNNER)


@unittest.skipIf(os.name == "nt", "The POSIX scheduler uses process groups; Windows has native controls")
class NativeScheduler(unittest.TestCase):
    def test_priority_failure_never_starts_expensive_work(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            marker = root / "expensive"
            groups = [
                {"executable": sys.executable, "arguments": ["-c", "raise SystemExit(7)"], "priority": True},
                {"executable": sys.executable, "arguments": ["-c", f"from pathlib import Path;Path({str(marker)!r}).touch()"], "priority": False},
            ]
            results = RUNNER.execute(groups, root / "groups", workers=2, threads=1)
            self.assertEqual([result["exit_code"] for result in results], [7])
            self.assertFalse(marker.exists())

    def test_workers_overlap_and_logs_and_temp_stay_separate(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            script = """import os,pathlib,sys,time
root=pathlib.Path(sys.argv[1]); index=sys.argv[2]
(root / (index+'.started')).touch()
deadline=time.monotonic()+5
while len(list(root.glob('*.started'))) < 2:
    if time.monotonic()>deadline: raise SystemExit(9)
    time.sleep(.01)
print(index);print(os.environ['TMPDIR']);print(index,file=sys.stderr)
"""
            groups = [{"executable": sys.executable, "arguments": ["-c", script, str(root), str(i)], "priority": False} for i in range(2)]
            results = RUNNER.execute(groups, root / "groups", workers=2, threads=1)
            self.assertEqual(len(results), 2)
            self.assertTrue(all(result["exit_code"] == 0 for result in results))
            for result in results:
                directory = Path(result["directory"])
                self.assertEqual((directory / "stderr.log").read_text().strip(), str(result["index"]))
                self.assertIn(str(directory / "temp"), (directory / "stdout.log").read_text())

    def test_cargo_messages_reject_duplicates_or_missing_test_binaries(self):
        import json
        row = {"reason": "compiler-artifact", "profile": {"test": True}, "executable": "/tmp/drmr_core-01234567"}
        self.assertEqual(RUNNER.executables(json.dumps(row)), [row["executable"]])
        for text in ("", json.dumps(row) + "\n" + json.dumps(row), "not json"):
            with self.subTest(text=text), self.assertRaises(ValueError):
                RUNNER.executables(text)

    def test_budget_never_exceeds_cpu_and_memory_bounds(self):
        for processors in (1, 2, 3, 4, 16):
            for memory in (0, 2 * 1024 ** 3, 4 * 1024 ** 3, 32 * 1024 ** 3):
                workers, threads = RUNNER.budget(processors, memory)
                self.assertLessEqual(workers * threads, processors)
                self.assertLessEqual(workers, 2)
                if memory < 4 * 1024 ** 3:
                    self.assertEqual(workers, 1)

    def test_zero_test_success_cannot_bypass_the_priority_barrier(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            marker = root / "expensive"
            groups = [
                {"executable": sys.executable, "arguments": ["-c", "print('test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 1 filtered out')"], "priority": True, "tests": ["required"]},
                {"executable": sys.executable, "arguments": ["-c", f"from pathlib import Path;Path({str(marker)!r}).touch()"], "priority": False},
            ]
            results = RUNNER.execute(groups, root / "groups", workers=2, threads=1)
            self.assertEqual([result["exit_code"] for result in results], [1])
            self.assertFalse(marker.exists())

    def test_interrupt_terminates_all_owned_subprocesses(self):
        processes = []
        original = RUNNER.subprocess.Popen
        original_sleep = RUNNER.time.sleep
        interrupted = False
        def spawn(*args, **kwargs):
            process = original(*args, **kwargs)
            processes.append(process)
            return process
        def interrupt_once(delay):
            nonlocal interrupted
            if not interrupted:
                interrupted = True
                raise KeyboardInterrupt
            original_sleep(delay)
        with tempfile.TemporaryDirectory() as temp:
            groups = [{"executable": sys.executable, "arguments": ["-c", "import time;time.sleep(60)"], "priority": False} for _ in range(2)]
            with mock.patch.object(RUNNER.subprocess, "Popen", side_effect=spawn), mock.patch.object(RUNNER.time, "sleep", side_effect=interrupt_once):
                with self.assertRaises(KeyboardInterrupt):
                    RUNNER.execute(groups, Path(temp) / "groups", workers=2, threads=1)
            self.assertEqual(len(processes), 2)
            self.assertTrue(all(process.poll() is not None for process in processes))


if __name__ == "__main__":
    unittest.main()
