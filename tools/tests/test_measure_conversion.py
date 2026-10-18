"""Regression checks for cooperative benchmark termination; no media or keys are fetched."""
import importlib.util
import os
from pathlib import Path
import signal
import subprocess
import sys
import tempfile
import time
import unittest

SPEC = importlib.util.spec_from_file_location(
    "measure_conversion", Path(__file__).resolve().parents[1] / "measure_conversion.py"
)
measure = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(measure)


@unittest.skipUnless(os.name == "posix", "POSIX termination-event contract")
class TerminationTests(unittest.TestCase):
    def start(self, directory, body):
        ready = Path(directory) / "ready"
        process = subprocess.Popen(
            [sys.executable, "-c", body, str(ready)], start_new_session=True,
            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
        )
        deadline = time.monotonic() + 5
        while not ready.is_file():
            self.assertIsNone(process.poll())
            self.assertLess(time.monotonic(), deadline)
            time.sleep(0.005)
        return process

    def test_cooperative_signal_is_delivered_before_forcing_exit(self):
        with tempfile.TemporaryDirectory() as directory:
            child = self.start(directory,
                "import pathlib,signal,sys,time; "
                "signal.signal(signal.SIGTERM,lambda *_:sys.exit(130)); "
                "pathlib.Path(sys.argv[1]).write_text('ready'); time.sleep(30)")
            try:
                result = measure.terminate(child, grace=1)
                self.assertEqual(result, {"method": "cooperative-signal", "cleanup": "cooperative-exit"})
                self.assertEqual(child.returncode, 130)
            finally:
                if child.poll() is None:
                    os.killpg(child.pid, signal.SIGKILL)
                    child.wait()

    def test_forced_fallback_is_bounded_and_never_claims_cleanup(self):
        with tempfile.TemporaryDirectory() as directory:
            child = self.start(directory,
                "import pathlib,signal,sys,time; signal.signal(signal.SIGTERM,signal.SIG_IGN); "
                "pathlib.Path(sys.argv[1]).write_text('ready'); time.sleep(30)")
            started = time.monotonic()
            try:
                result = measure.terminate(child, grace=0.1)
                self.assertEqual(result, {"method": "force", "cleanup": "unconfirmed"})
                self.assertIsNotNone(child.returncode)
                self.assertLess(time.monotonic() - started, 5)
            finally:
                if child.poll() is None:
                    os.killpg(child.pid, signal.SIGKILL)
                    child.wait()


if __name__ == "__main__":
    unittest.main()
