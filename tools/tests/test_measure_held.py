"""Mandatory real helper wiring with a held, owned workload, separate from media proofs."""
import importlib.util
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import threading
import unittest
from unittest import mock


def module(name, path):
    spec = importlib.util.spec_from_file_location(name, path)
    result = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(result)
    return result


ROOT = Path(__file__).resolve().parents[1]
measure = module("measure_held_conversion", ROOT / "measure_conversion.py")
held = module("held_measurement", Path(__file__).with_name("held_measurement.py"))


class HeldHelper(unittest.TestCase):
    def test_abnormal_root_and_failed_release_still_terminate_and_drain_the_retained_job(self):
        for fail_release in (False, True):
            with self.subTest(fail_release=fail_release), tempfile.TemporaryDirectory() as directory:
                owner = held.OwnedWorkload(Path(directory), measure.OBSERVER)
                if owner.job:
                    owner.job.close()
                job = mock.Mock()
                job.drained.return_value = True
                owner.job = job
                root = mock.Mock(returncode=7, _handle=1)
                root.wait.return_value = 7
                owner.root, owner.child = root, mock.Mock(handle=2)
                try:
                    if fail_release:
                        with mock.patch.object(owner, "release", side_effect=OSError("private-test-failure")):
                            with self.assertRaisesRegex(RuntimeError, "^fixture-release-failed$"):
                                owner.stop()
                    else:
                        owner.stop()
                    job.terminate.assert_called_once_with()
                    root.wait.assert_called_once_with(timeout=5)
                    job.drained.assert_called_once_with([1, 2])
                    owner.stop()
                    job.terminate.assert_called_once_with()
                finally:
                    owner.close()

    @unittest.skipUnless(os.name == "posix", "Unix owned-group counterpart to the Windows retained Job case")
    def test_failed_release_stops_a_real_unreaped_owned_group_and_confirms_absence(self):
        with tempfile.TemporaryDirectory() as directory:
            owner = held.OwnedWorkload(Path(directory), measure.OBSERVER)
            root = subprocess.Popen([sys.executable, held.__file__, "--root", directory],
                                    stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL,
                                    stderr=subprocess.DEVNULL, close_fds=True, start_new_session=True)
            owner.start(root)
            try:
                owner.ready()
                with mock.patch.object(owner, "release", side_effect=OSError("private-test-failure")):
                    with self.assertRaisesRegex(RuntimeError, "^fixture-release-failed$"):
                        owner.stop()
                self.assertIsNotNone(root.returncode)
                with self.assertRaises(ProcessLookupError):
                    os.killpg(root.pid, 0)
            finally:
                try:
                    owner.stop()
                finally:
                    owner.close()

    @unittest.skipUnless(os.name == "nt", "Windows held Job counterpart to the Unix owned-group case")
    def test_abnormal_root_exit_terminates_its_real_held_child_before_job_release(self):
        with tempfile.TemporaryDirectory() as directory:
            owner = held.OwnedWorkload(Path(directory), measure.OBSERVER)
            root = subprocess.Popen([sys.executable, held.__file__, "--root-crash", directory],
                                    stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL,
                                    stderr=subprocess.DEVNULL, close_fds=True)
            owner.start(root)
            try:
                owner.ready()
                self.assertEqual(root.wait(timeout=5), 7)
                self.assertTrue(owner.child.is_running())
                owner.stop()
                self.assertFalse(owner.child.is_running())
                self.assertTrue(owner.job.drained([int(root._handle), owner.child.handle]))
            finally:
                try:
                    owner.stop()
                finally:
                    owner.close()

    def test_real_measured_run_retains_positive_held_child_observations_and_confirms_cleanup(self):
        with tempfile.TemporaryDirectory() as directory:
            owner = held.OwnedWorkload(Path(directory), measure.OBSERVER)
            original_popen, original_run = subprocess.Popen, subprocess.run
            original_start, original_sample = measure.OBSERVER.start, measure.sample_processes
            observer_started = threading.Event()
            results, failures, observed, sampled_rows = [], [], [], []
            last_table = {}

            def spawn(argv, *args, **kwargs):
                process = original_popen(argv, *args, **kwargs)
                if len(argv) > 2 and argv[1] == held.__file__ and argv[2] == "--root":
                    owner.root = process
                    if os.name != "nt":
                        owner.start(process)
                return process

            def ps(argv, *args, **kwargs):
                result = original_run(argv, *args, **kwargs)
                if argv == ["ps", "-axo", "pid=,ppid=,rss="]:
                    last_table.clear()
                    last_table.update({pid: (parent, rss * 1024) for pid, parent, rss in
                                       (map(int, line.split()) for line in result.stdout.splitlines())})
                return result

            def sample(pid):
                child = owner.ready()
                value = original_sample(pid)
                if value is not None:
                    sampled_rows.append(value)
                if value is not None and value[1] > 0:
                    self.assertTrue(last_table[child][0] == owner.root.pid, "fixture-parent-link-mismatch")
                    self.assertGreater(last_table[child][1], 0)
                    self.assertEqual(value[1:], (last_table[child][1], 1))
                    observed.append(value)
                    owner.release()
                return value

            def start(process, interval, _timeout, environment):
                owner.start(process)
                owner.ready()
                # Real observer main reaches its normal observation deadline while
                # the fixture remains held. No fake samples or readiness API exist.
                owner.observer = original_start(process, interval, 1, environment)
                observer_started.set()
                return owner.observer

            def run():
                try:
                    results.append(measure.measured_run(
                        [sys.executable, held.__file__, "--root", directory], dict(os.environ), 20, 0.1))
                except BaseException as problem:
                    failures.append(problem)

            worker = threading.Thread(target=run)
            try:
                with mock.patch.object(subprocess, "Popen", side_effect=spawn), mock.patch.object(
                        subprocess, "run", side_effect=ps), mock.patch.object(
                        measure, "sample_processes", side_effect=sample), mock.patch.object(
                        measure.OBSERVER, "start", side_effect=start):
                    worker.start()
                    if os.name == "nt":
                        self.assertTrue(observer_started.wait(8), "fixture-observer-not-started")
                        self.assertIsNotNone(owner.observer)
                        self.assertEqual(owner.observer.wait(timeout=5), 0)
                        owner.release()
                    worker.join(timeout=8)
                    self.assertFalse(worker.is_alive(), "fixture-controller-timeout")
                self.assertEqual(failures, [])
                self.assertEqual(len(results), 1)
                result, payload = results[0]
                self.assertEqual(result["status"], "completed")
                self.assertEqual(result["exit_code"], 0)
                self.assertEqual(owner.root.returncode, 0)
                self.assertEqual(payload, {})  # A control fixture, never a media-proof substitute.
                memory = result["memory"]
                self.assertGreater(memory["samples"], 0)
                self.assertGreater(memory["parent_sampled_peak_bytes"], 0)
                self.assertGreater(memory["descendant_sampled_peak_bytes"], 0)
                self.assertGreater(memory["positive_descendant_memory_samples"], 0)
                self.assertEqual(memory["observed_peak_descendant_count"], 1)
                if os.name == "nt":
                    self.assertEqual(memory["status"], "partial")
                    self.assertEqual(memory["reason"], "observer-deadline")
                    self.assertEqual(memory["observer_cleanup"], "confirmed-exit")
                    for name in ("failed_samples", "root_missing_samples", "parent_zero_samples",
                                 "zero_descendant_memory_samples", "no_descendant_observation_samples",
                                 "incomplete_samples", "identity_rejections"):
                        self.assertEqual(memory[name], 0)
                else:
                    self.assertTrue(observed)
                    self.assertEqual(memory["samples"], len(sampled_rows))
                    self.assertEqual(memory["parent_sampled_peak_bytes"], max(row[0] for row in sampled_rows))
                    self.assertEqual(memory["descendant_sampled_peak_bytes"], max(row[1] for row in sampled_rows))
                    self.assertEqual(memory["positive_descendant_memory_samples"], sum(row[1] > 0 for row in sampled_rows))
                measure.OBSERVER.validate({name: value for name, value in memory.items() if name != "observer_cleanup"})
            finally:
                cleanup_failed = False
                try:
                    owner.stop()
                except BaseException:
                    cleanup_failed = True
                try:
                    if owner.observer and owner.observer.poll() is None:
                        owner.observer.terminate()
                        owner.observer.wait(timeout=2)
                except BaseException:
                    cleanup_failed = True
                try:
                    if worker.ident is not None:
                        worker.join(timeout=5)
                        cleanup_failed = cleanup_failed or worker.is_alive()
                finally:
                    try:
                        owner.close()
                    except BaseException:
                        cleanup_failed = True
                if cleanup_failed:
                    raise RuntimeError("fixture-controller-cleanup-unconfirmed") from None


if __name__ == "__main__":
    unittest.main()
