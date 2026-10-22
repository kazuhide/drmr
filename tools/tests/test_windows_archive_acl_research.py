"""Exercise the diagnostic-only stderr drain without retaining tool output."""

import importlib.util
import json
import os
from pathlib import Path
import subprocess
import sys
import unittest
from unittest import mock


SCRIPT = Path(__file__).resolve().parents[2] / ".github/scripts/windows_archive_acl_research.py"
SPEC = importlib.util.spec_from_file_location("acl_research_test", SCRIPT)
RESEARCH = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(RESEARCH)


class ResearchCaptureTests(unittest.TestCase):
    def test_actual_child_is_drained_with_only_a_bounded_prefix_retained(self):
        rows = []
        code = "import os; os.write(2, b'UnauthorizedAccessException: Access is denied. HResult: 0x80070005 NativeErrorCode: 5\\n'); os.write(2, b'x'*262144); raise SystemExit(7)"
        result = RESEARCH.capture(subprocess.run, rows.append, "report-file",
                                  [sys.executable, "-B", "-c", code], stdin=subprocess.DEVNULL,
                                  stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=20)
        self.assertEqual(result.returncode, 7)
        self.assertIsNone(result.stderr)
        row = rows[0]
        self.assertEqual(row["stderr_state"], "complete")
        self.assertGreater(row["stderr_bytes"], 262144)
        self.assertEqual(row["stderr_retained_bytes"], 8192)
        self.assertTrue(row["stderr_truncated"])
        self.assertEqual(row["hresults"], [2147942405])
        self.assertEqual(row["win32_codes"], [5])
        self.assertEqual(row["categories"], ["AccessDenied", "UnauthorizedAccessException"])

    def test_only_stderr_changes_and_unsigned_result_identity_is_preserved(self):
        rows = []
        command, environment, positional = ["powershell.exe", "-EncodedCommand", "fixed"], {"private": "marker"}, object()
        returned = subprocess.CompletedProcess(command, 3221225794)

        def run(actual, *args, **kwargs):
            self.assertIs(actual, command)
            self.assertEqual(args, (positional,))
            self.assertIs(kwargs["env"], environment)
            self.assertEqual(kwargs["timeout"], 20)
            self.assertEqual(kwargs["stdin"], subprocess.DEVNULL)
            self.assertEqual(kwargs["stdout"], subprocess.DEVNULL)
            self.assertIs(type(kwargs["stderr"]), int)
            os.write(kwargs["stderr"], b"synthetic-private-value private/path S-1-5-private Password=secret")
            return returned

        actual = RESEARCH.capture(run, rows.append, "temp-directory", command, positional,
                                  env=environment, timeout=20, stdin=subprocess.DEVNULL,
                                  stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        self.assertIs(actual, returned)
        self.assertEqual(rows[0]["exit_code"], 3221225794)
        self.assertEqual(rows[0]["categories"], ["UnclassifiedStderr"])
        serialized = json.dumps(rows)
        for private in ("synthetic-private-value", "private/path", "S-1-5-private", "Password", "secret"):
            self.assertNotIn(private, serialized)

    def test_timeout_and_oserror_instances_are_preserved(self):
        for error, expected in ((subprocess.TimeoutExpired(["private"], 20, stderr=b"private"), "timeout"),
                                (OSError("private error"), "os-error")):
            with self.subTest(expected=expected):
                rows = []

                def run(*args, **kwargs):
                    os.write(kwargs["stderr"], b"SetAccessControl: Access is denied")
                    raise error

                with self.assertRaises(type(error)) as raised:
                    RESEARCH.capture(run, rows.append, "report-file", ["powershell.exe"],
                                     stderr=subprocess.DEVNULL, timeout=20)
                self.assertIs(raised.exception, error)
                self.assertEqual(rows[0]["status"], expected)
                self.assertEqual(rows[0]["stderr_state"], "complete")
                self.assertNotIn("private", json.dumps(rows))

    def test_pipe_unavailable_delegates_unchanged(self):
        rows, called = [], []
        kwargs = {"stderr": subprocess.DEVNULL, "timeout": 20}
        returned = subprocess.CompletedProcess(["powershell.exe"], 1)

        def run(command, **actual):
            called.append(actual)
            return returned

        with mock.patch.object(RESEARCH.os, "pipe", side_effect=OSError("private")):
            self.assertIs(RESEARCH.capture(run, rows.append, "report-file", ["powershell.exe"], **kwargs), returned)
        self.assertEqual(called, [kwargs])
        self.assertEqual(rows[0]["stderr_state"], "unavailable")
        self.assertNotIn("private", json.dumps(rows))

    def test_error_codes_require_explicit_labels_and_supported_integer_ranges(self):
        record = RESEARCH.classify(b"private/123456 0x12345678 HResult: -2147024891 NativeErrorCode=5 Win32ErrorCode=4294967296")
        self.assertEqual(record["hresults"], [-2147024891])
        self.assertEqual(record["win32_codes"], [5])
        self.assertEqual(record["categories"], ["UnclassifiedStderr"])
        self.assertEqual(RESEARCH.classify(b"")["categories"], ["NoStderr"])

    def test_publish_failure_does_not_replace_result_or_original_exception(self):
        def publish(row):
            raise OSError("private publish error")

        returned = subprocess.CompletedProcess([], 0)
        self.assertIs(RESEARCH.capture(lambda *a, **kw: returned, publish, "report-file", []), returned)
        error = OSError("private original")

        def run(*args, **kwargs):
            raise error

        with self.assertRaises(OSError) as raised:
            RESEARCH.capture(run, publish, "report-file", [])
        self.assertIs(raised.exception, error)


if __name__ == "__main__":
    unittest.main()
