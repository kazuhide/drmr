"""Keep the split Rust jobs accountable for their complete skip coverage."""

import importlib.util
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

SCRIPT = Path(__file__).resolve().parents[2] / ".github/scripts/check-rust-test-skips.py"
SPEC = importlib.util.spec_from_file_location("check_rust_test_skips", SCRIPT)
guard = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(guard)

HELPERS = (
    ("child_reads_the_process_environment",
     "run by load_key_material_reads_the_process_environment in a child process"),
    ("child_transcrypts_with_relative_paths",
     "run by transcrypt_relative_paths_are_spelled_as_pathlib_spells_them in a child"),
)


def summary(ignored=0, duration=True):
    text = (f"test result: ok. 3 passed; 0 failed; {ignored} ignored; "
            "0 measured; 0 filtered out")
    return text + ("; finished in 0.08s" if duration else "") + "\n"


def helper(index):
    name, reason = HELPERS[index]
    return f"test {name} ... ignored, {reason}\n"


def core_log():
    return summary() + helper(0) + summary(1) + helper(1) + summary(1)


class SkipCoverage(unittest.TestCase):
    def test_complete_core_accounts_for_each_child_helper_once(self):
        self.assertEqual(guard.check_log(core_log(), "core"), 3)

    def test_helpers_can_share_a_test_binary(self):
        self.assertEqual(guard.check_log(helper(0) + helper(1) + summary(2), "core"), 1)

    def test_workspace_requires_no_ignored_tests(self):
        self.assertEqual(guard.check_log(summary(), "workspace"), 1)

    def test_existing_harness_without_duration_is_complete(self):
        self.assertEqual(guard.check_log(summary(duration=False), "workspace"), 1)

    def test_ansi_color_and_captured_output_prefix_are_accepted(self):
        text = "captured output \x1b[32m" + summary().rstrip("\n") + "\x1b[0m\n"
        self.assertEqual(guard.check_log(text, "workspace"), 1)

    def test_colored_core_helper_reasons_are_still_checked_exactly(self):
        text = "\n".join(f"\x1b[33m{line}\x1b[0m" for line in core_log().splitlines())
        self.assertEqual(guard.check_log(text, "core"), 3)

    def test_child_failure_summaries_do_not_replace_cargo_exit_status(self):
        child = summary().replace("ok. 3 passed; 0 failed", "FAILED. 0 passed; 3 failed")
        self.assertEqual(guard.check_log(child + core_log(), "core"), 4)

    def test_only_the_existing_skip_notes_are_allowed_in_either_scope(self):
        notes = ["skipped: host FFmpeg lacks TTML-in-MP4 muxing"]
        for version in range(7):
            notes.extend((
                f"skipped: ffmpeg {version} omits packet timing on this fixture",
                f"skipped: ffmpeg {version} omits packet timing on this fixture (host detail)",
            ))
        for scope, log in (("core", core_log()), ("workspace", summary())):
            with self.subTest(scope=scope):
                self.assertGreater(guard.check_log("\n".join(notes) + "\n" + log, scope), 0)

    def test_unknown_or_extended_skip_notes_are_rejected(self):
        for note in (
            "skipped: ffmpeg 7 omits packet timing on this fixture",
            "skipped: host FFmpeg lacks TTML-in-MP4 muxing unexpectedly",
            "skipped: new optional tool is missing",
        ):
            with self.subTest(note=note), self.assertRaisesRegex(
                    guard.SkipCoverageError, "^Unexpected skipped tests detected\\.$"):
                guard.check_log(note + "\n" + core_log(), "core")

    def test_missing_helper_or_its_summary_is_rejected(self):
        for text in (helper(0) + summary(1), core_log().rsplit("test result:", 1)[0]):
            with self.subTest(text=text), self.assertRaises(guard.SkipCoverageError):
                guard.check_log(text, "core")

    def test_duplicate_helper_is_rejected_even_across_summaries(self):
        with self.assertRaises(guard.SkipCoverageError):
            guard.check_log(core_log() + helper(0) + summary(1), "core")

    def test_unknown_helpers_and_missing_or_changed_reasons_are_rejected(self):
        for line in (
            "test other_child ... ignored, private reason\n",
            f"test {HELPERS[0][0]} ... ignored\n",
            f"test {HELPERS[0][0]} ... ignored, changed reason\n",
        ):
            with self.subTest(line=line), self.assertRaises(guard.SkipCoverageError):
                guard.check_log(line + summary(1) + core_log(), "core")

    def test_workspace_rejects_both_known_and_unknown_ignored_tests(self):
        for line in (helper(0), "test unrelated ... ignored\n"):
            with self.subTest(line=line), self.assertRaises(guard.SkipCoverageError):
                guard.check_log(line + summary(1), "workspace")

    def test_malformed_ignored_lines_cannot_bypass_either_scope(self):
        for line in ("test unrelated ... ignored; malformed\n",
                     "test  unrelated ... ignored, reason\n"):
            for scope, log in (("core", core_log()), ("workspace", summary())):
                with self.subTest(scope=scope, line=line), self.assertRaises(guard.SkipCoverageError):
                    guard.check_log(line + log, scope)

    def test_ignored_summary_must_match_names_in_that_binary(self):
        for text, scope in (
            (helper(0) + summary(0) + helper(1) + summary(2), "core"),
            (summary(1) + core_log(), "core"),
            (summary(1), "workspace"),
        ):
            with self.subTest(scope=scope, text=text), self.assertRaises(guard.SkipCoverageError):
                guard.check_log(text, scope)

    def test_empty_missing_and_malformed_summaries_are_rejected_in_both_scopes(self):
        for text in (
            "",
            "running 3 tests\ntest ordinary ... ok\n",
            "test result: ok. 3 passed; 0 failed; 0 ignored;\n",
            summary().replace("0 ignored", "many ignored"),
            summary().rstrip("\n") + " truncated\n",
        ):
            for scope in ("core", "workspace"):
                with self.subTest(scope=scope, text=text), self.assertRaises(guard.SkipCoverageError):
                    guard.check_log(text, scope)

    def test_later_malformed_summary_is_not_hidden_by_a_complete_one(self):
        with self.assertRaises(guard.SkipCoverageError):
            guard.check_log(summary() + "test result: ok. truncated\n", "workspace")

    def test_unknown_scope_cannot_disable_checks(self):
        with self.assertRaises(guard.SkipCoverageError):
            guard.check_log(summary(), "unknown")


class SkipGuardCli(unittest.TestCase):
    def run_guard(self, scope, payload=None):
        with tempfile.TemporaryDirectory() as directory:
            log = Path(directory) / "private-log.txt"
            if payload is not None:
                log.write_bytes(payload)
            return subprocess.run(
                [sys.executable, "-B", str(SCRIPT), "--scope", scope, "--log", str(log)],
                capture_output=True, text=True, check=False,
            )

    def test_valid_core_and_workspace_logs_succeed(self):
        for scope, payload in (("core", core_log()), ("workspace", summary())):
            with self.subTest(scope=scope):
                result = self.run_guard(scope, payload.encode("utf-8"))
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertIn("No unexpected test skips", result.stdout)

    def test_missing_and_invalid_utf8_logs_fail_without_exposing_paths(self):
        for payload in (None, b"\xff\xfe"):
            with self.subTest(payload=payload):
                result = self.run_guard("workspace", payload)
                self.assertNotEqual(result.returncode, 0)
                self.assertEqual(result.stdout, "")
                self.assertNotIn("private-log", result.stderr)
                self.assertNotIn("Traceback", result.stderr)

    def test_unexpected_notes_fail_without_echoing_untrusted_text(self):
        payload = "skipped: PRIVATE_REASON\n" + summary()
        result = self.run_guard("workspace", payload.encode("utf-8"))
        self.assertNotEqual(result.returncode, 0)
        self.assertNotIn("PRIVATE_REASON", result.stdout + result.stderr)

    def test_empty_workspace_log_fails_in_the_real_entrypoint(self):
        result = self.run_guard("workspace", b"")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("complete test-result summary", result.stderr)


if __name__ == "__main__":
    unittest.main()
