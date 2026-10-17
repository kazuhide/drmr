"""Run with: python3 -m unittest discover -s tools/tests -p 'test_measure_capture.py'."""

import importlib.util
import json
from pathlib import Path
import subprocess
import sys
import time
import unittest
from unittest.mock import patch

SPEC = importlib.util.spec_from_file_location(
    "measure_capture", Path(__file__).resolve().parents[1] / "measure_capture.py"
)
capture = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(capture)


class CaptureTests(unittest.TestCase):
    def command(self, body):
        return [sys.executable, "-c", body]

    def test_noisy_stderr_does_not_block_valid_stdout(self):
        raw, memory = capture.run_captured(self.command(
            "import os; os.write(2, b'x' * 524288); print('{\"success\": true}')"
        ), timeout=2)
        self.assertEqual(json.loads(raw), {"success": True})
        self.assertEqual(memory["status"], "unmeasured")

    def test_each_stream_limit_refuses_a_zero_exit_worker(self):
        for stream, limit in ((1, capture.MAX_STDOUT_BYTES), (2, capture.MAX_STDERR_BYTES)):
            with self.subTest(stream=stream), self.assertRaisesRegex(
                RuntimeError, "^capture-output-size-limit$"
            ):
                capture.run_captured(self.command(
                    f"import os; os.write({stream}, b'x' * {limit + 1})"
                ), timeout=2)

    def test_deadline_kills_and_reaps_a_worker_even_after_a_valid_report(self):
        children = []
        original = subprocess.Popen

        def spawn(*args, **kwargs):
            child = original(*args, **kwargs)
            children.append(child)
            return child

        started = time.monotonic()
        with patch.object(capture.subprocess, "Popen", side_effect=spawn):
            with self.assertRaisesRegex(RuntimeError, "^capture-child-timeout$"):
                capture.run_captured(self.command(
                    "import time; print('{\"success\": true}', flush=True); time.sleep(10)"
                ), timeout=0.1)
        self.assertLess(time.monotonic() - started, 5)
        self.assertIsNotNone(children[0].returncode)
        self.assertNotEqual(children[0].returncode, 0)

    def test_child_failure_has_fixed_diagnostics(self):
        with self.assertRaisesRegex(RuntimeError, "^capture-child-failed$"):
            capture.run_captured(self.command(
                "import sys; print('PRIVATE', file=sys.stderr); sys.exit(7)"
            ), timeout=2)

    def test_measure_preserves_negative_capture_evidence(self):
        payload = {"success": False, "error": "output-limit", "elapsed_seconds": 0.1}
        with patch.object(capture, "run_captured", return_value=(
            (json.dumps(payload) + "\n").encode(), {"status": "unmeasured"}
        )):
            self.assertEqual(capture.measure("worker", 65536)["result"], payload)

    def test_measure_refuses_missing_duplicate_or_malformed_reports(self):
        for raw in (b"no result\n", b"{}\n{}\n", b"{broken}\n", b"{}\n"):
            with self.subTest(raw=raw), patch.object(capture, "run_captured", return_value=(raw, {})):
                with self.assertRaisesRegex(RuntimeError, "^capture-worker-report-invalid$"):
                    capture.measure("worker", 65536)


if __name__ == "__main__":
    unittest.main()
