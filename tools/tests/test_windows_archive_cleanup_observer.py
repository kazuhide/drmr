"""Exercise the embedded diagnostic observer without claiming native ACL proof."""

import ast
import json
from pathlib import Path
import re
import subprocess
import tempfile
import time
import unittest
import uuid


SCRIPT = Path(__file__).resolve().parents[2] / ".github/scripts/test-windows-cli-archive-cleanup.ps1"
CLEANUP = SCRIPT.with_name("windows-cli-archive-cleanup.ps1")
SENTINEL = "synthetic-private-value"


class ArchiveRootNameContracts(unittest.TestCase):
    def accepts(self, name):
        source = CLEANUP.read_text(encoding="utf-8")
        body = source.split("function Assert-DrmrArchiveDirectory {", 1)[1].split("\n}\n", 1)[0]
        pattern = re.search(r"-cnotmatch '([^']+)'", body)
        if pattern:
            # These anchors have the same strict meaning in .NET and Python.
            return re.fullmatch(pattern[1].replace(r"\z", r"\Z"), name) is not None
        legacy = re.search(r"\.StartsWith\('([^']+)', \[StringComparison\]::Ordinal\)", body)
        self.assertIsNotNone(legacy, "The cleanup name contract is unrecognized")
        return name.startswith(legacy[1])

    def test_every_actual_producer_name_is_accepted_by_cleanup(self):
        for script, variable in ((SCRIPT.with_name("windows-cli-archive.ps1"), "directory"),
                                 (SCRIPT, "directory"), (SCRIPT, "observerDirectory")):
            source = script.read_text(encoding="utf-8")
            expression = re.search(r"\$" + variable + r" = Join-Path [^\n]+ \('([^']+)' \+ \[guid\]::NewGuid\(\)\.ToString\('N'\)\)", source)
            self.assertIsNotNone(expression, "The producer must retain a complete GUID nonce")
            with self.subTest(script=script.name, variable=variable):
                self.assertTrue(self.accepts(expression[1] + uuid.uuid4().hex),
                                "The actual producer root is refused by cleanup")

    def test_cleanup_refuses_partial_or_ambiguous_root_names(self):
        full = "q-" + "a" * 32
        for name in ("q-", "q-" + "a" * 31, full + "a", full + "-retained", full + "\n",
                     "Q-" + "a" * 32, "q-" + "A" * 32, "q-" + "g" * 32,
                     "drmr-archive-" + "a" * 32):
            with self.subTest(shape=len(name)):
                self.assertFalse(self.accepts(name))


class ArchiveAclObserverTests(unittest.TestCase):
    def setUp(self):
        blocks = re.findall(r'(?ms)^\$code = @"\n(.*?)^"@', SCRIPT.read_text(encoding="utf-8"))
        self.assertEqual(len(blocks), 1)
        tree = ast.parse(blocks[0])
        names = {"publish_diagnostic", "observe_acl_run"}
        functions = [node for node in tree.body if isinstance(node, ast.FunctionDef) and node.name in names]
        self.assertEqual({node.name for node in functions}, names)
        self.code = compile(ast.Module(body=functions, type_ignores=[]),
                            ".github/scripts/test-windows-cli-archive-cleanup.ps1", "exec")
        temporary = tempfile.TemporaryDirectory(prefix="drmr-acl-observer-test-")
        self.addCleanup(temporary.cleanup)
        self.path = Path(temporary.name) / "diagnostic.json"

    def observer(self, original, destination=None):
        state = {"schema_version": 1, "mode": "success", "stage": "report-file",
                 "python_exit_code": None, "acl_calls": []}
        namespace = {"json": json, "time": time, "subprocess": subprocess,
                     "diagnostic": state, "diagnostic_path": self.path if destination is None else destination,
                     "original_run": original}
        exec(self.code, namespace)
        return namespace["observe_acl_run"], namespace["publish_diagnostic"], state

    def fixed_record(self, expected_statuses):
        raw = self.path.read_bytes()
        self.assertLessEqual(len(raw), 8192)
        self.assertNotIn(SENTINEL.encode(), raw)
        record = json.loads(raw)
        self.assertEqual(set(record), {"schema_version", "mode", "stage", "python_exit_code", "acl_calls"})
        self.assertEqual(record["schema_version"], 1)
        self.assertEqual(record["mode"], "success")
        self.assertIn(record["stage"], {"report-file", "temp-directory", "private-payload"})
        self.assertIsNone(record["python_exit_code"])
        self.assertEqual([call["status"] for call in record["acl_calls"]], expected_statuses)
        for call in record["acl_calls"]:
            self.assertEqual(set(call), {"stage", "status", "elapsed_ms", "exit_code"})
            self.assertIn(call["stage"], {"report-file", "temp-directory", "private-payload"})
            self.assertIs(type(call["elapsed_ms"]), int)
            self.assertGreaterEqual(call["elapsed_ms"], 0)
            self.assertLessEqual(call["elapsed_ms"], 120000)
            if call["exit_code"] is not None:
                self.assertIs(type(call["exit_code"]), int)
                self.assertGreaterEqual(call["exit_code"], -(2**31))
                self.assertLess(call["exit_code"], 2**32)
        return record

    def test_success_preserves_arguments_environment_deadline_and_result(self):
        captured = []
        result = subprocess.CompletedProcess(["powershell.exe"], 0)

        def original(*args, **kwargs):
            captured.append((args, kwargs))
            return result

        observe, _, _ = self.observer(original)
        command = ["powershell.exe", "-EncodedCommand", SENTINEL]
        environment = {"PRIVATE_TEST_VALUE": SENTINEL}
        options = {"env": environment, "timeout": 20, "stdin": subprocess.DEVNULL,
                   "stdout": subprocess.DEVNULL, "stderr": subprocess.DEVNULL}
        self.assertIs(observe(command, **options), result)
        self.assertEqual(len(captured), 1)
        self.assertIs(captured[0][0][0], command)
        self.assertEqual(captured[0][1], options)
        self.assertIs(captured[0][1]["env"], environment)
        self.assertEqual(self.fixed_record(["ok"])["acl_calls"][0]["exit_code"], 0)

    def test_nonzero_result_is_returned_without_replacement(self):
        for code in (-1073741502, 3221225794):
            with self.subTest(returncode=code):
                result = subprocess.CompletedProcess(["powershell.exe"], code,
                                                     stdout=SENTINEL, stderr=SENTINEL)
                observe, _, _ = self.observer(lambda *args, **kwargs: result)
                self.assertIs(observe(["powershell.exe"], timeout=20), result)
                self.assertEqual(self.fixed_record(["nonzero"])["acl_calls"][0]["exit_code"], code)

    def test_timeout_and_os_error_rethrow_the_same_exception_without_details(self):
        for error, status in ((subprocess.TimeoutExpired([SENTINEL], 20, output=SENTINEL,
                                                         stderr=SENTINEL), "timeout"),
                              (OSError(SENTINEL), "os-error")):
            with self.subTest(status=status):
                def original(*args, **kwargs):
                    raise error

                observe, _, _ = self.observer(original)
                with self.assertRaises(type(error)) as caught:
                    observe(["powershell.exe"], timeout=20)
                self.assertIs(caught.exception, error)
                self.assertIsNone(self.fixed_record([status])["acl_calls"][0]["exit_code"])

    def test_unrelated_subprocess_is_delegated_without_diagnostics(self):
        captured = []
        result = object()

        def original(*args, **kwargs):
            captured.append((args, kwargs))
            return result

        observe, _, state = self.observer(original)
        command = ["another-program", SENTINEL]
        self.assertIs(observe(command, timeout=20), result)
        self.assertIs(captured[0][0][0], command)
        self.assertEqual(captured[0][1], {"timeout": 20})
        self.assertEqual(state["acl_calls"], [])
        self.assertFalse(self.path.exists())

    def test_diagnostic_write_failure_cannot_replace_result_or_exception(self):
        class Unwritable:
            def write_text(self, *args, **kwargs):
                raise OSError(SENTINEL)

        result = subprocess.CompletedProcess(["powershell.exe"], 0)
        observe, _, _ = self.observer(lambda *args, **kwargs: result, Unwritable())
        self.assertIs(observe(["powershell.exe"], timeout=20), result)
        error = subprocess.TimeoutExpired([SENTINEL], 20)

        def original(*args, **kwargs):
            raise error

        observe, _, _ = self.observer(original, Unwritable())
        with self.assertRaises(subprocess.TimeoutExpired) as caught:
            observe(["powershell.exe"], timeout=20)
        self.assertIs(caught.exception, error)

    def test_all_three_acl_stages_produce_bounded_fixed_records(self):
        result = subprocess.CompletedProcess(["powershell.exe"], 0)
        observe, _, state = self.observer(lambda *args, **kwargs: result)
        stages = ["report-file", "temp-directory", "private-payload"]
        for stage in stages:
            state["stage"] = stage
            self.assertIs(observe(("powershell.exe", SENTINEL), timeout=20), result)
        record = self.fixed_record(["ok", "ok", "ok"])
        self.assertEqual([call["stage"] for call in record["acl_calls"]], stages)

    def test_publisher_refuses_oversized_records_without_partial_output(self):
        _, publish, state = self.observer(lambda *args, **kwargs: None)
        state["stage"] = SENTINEL * 8192
        publish()
        self.assertFalse(self.path.exists())


if __name__ == "__main__":
    unittest.main()
