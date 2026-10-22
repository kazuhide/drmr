"""Require complete Rust test logs without retaining obsolete skip exemptions."""
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


def summary(ignored=0, duration=True):
    text = (f"test result: ok. 3 passed; 0 failed; {ignored} ignored; "
            "0 measured; 0 filtered out")
    return text + ("; finished in 0.08s" if duration else "") + "\n"


class SkipCoverage(unittest.TestCase):
    def test_core_and_workspace_require_no_ignored_tests(self):
        for scope in ("core", "workspace"):
            self.assertEqual(guard.check_log(summary() + summary(), scope), 2)

    def test_existing_harness_without_duration_is_complete(self):
        self.assertEqual(guard.check_log(summary(duration=False), "workspace"), 1)

    def test_ansi_color_and_captured_output_prefix_are_accepted(self):
        text = "captured output \x1b[32m" + summary().rstrip("\n") + "\x1b[0m\n"
        for scope in ("core", "workspace"):
            self.assertEqual(guard.check_log(text, scope), 1)

    def test_child_failure_summaries_do_not_replace_cargo_exit_status(self):
        failed = summary().replace("ok. 3 passed; 0 failed", "FAILED. 0 passed; 3 failed")
        self.assertEqual(guard.check_log(failed + summary(), "core"), 2)

    def test_only_the_current_packet_timing_note_is_allowed(self):
        notes = []
        for version in range(7):
            notes.extend((
                f"skipped: ffmpeg {version} omits packet timing on this fixture",
                f"skipped: ffmpeg {version} omits packet timing on this fixture (host detail)",
            ))
        for scope in ("core", "workspace"):
            self.assertEqual(guard.check_log("\n".join(notes) + "\n" + summary(), scope), 1)

    def test_retired_ttml_and_unknown_skip_exemptions_are_rejected(self):
        for note in ("skipped: host FFmpeg lacks TTML-in-MP4 muxing",
                     "skipped: ffmpeg 7 omits packet timing on this fixture",
                     "skipped: new optional tool is missing"):
            for scope in ("core", "workspace"):
                with self.subTest(scope=scope, note=note), self.assertRaises(guard.SkipCoverageError):
                    guard.check_log(note + "\n" + summary(), scope)

    def test_all_named_ignored_tests_are_refused_in_both_scopes(self):
        for name in ("child_reads_the_process_environment", "child_transcrypts_with_relative_paths", "unrelated"):
            for suffix in ("", ", a reason", "; malformed"):
                for scope in ("core", "workspace"):
                    with self.subTest(name=name, scope=scope, suffix=suffix), self.assertRaises(guard.SkipCoverageError):
                        guard.check_log(f"test {name} ... ignored{suffix}\n" + summary(1), scope)

    def test_ignored_count_without_names_also_fails(self):
        for scope in ("core", "workspace"):
            with self.assertRaises(guard.SkipCoverageError):
                guard.check_log(summary(1), scope)

    def test_empty_missing_and_malformed_summaries_are_rejected(self):
        for text in ("", "test something ... ok\n", "test result: ok. incomplete\n",
                     summary().rstrip() + " test result: ok. repeated\n"):
            for scope in ("core", "workspace"):
                with self.subTest(scope=scope, text=text), self.assertRaises(guard.SkipCoverageError):
                    guard.check_log(text, scope)

    def test_unknown_scope_is_refused(self):
        with self.assertRaises(guard.SkipCoverageError):
            guard.check_log(summary(), "unknown")

    def test_cli_refuses_an_actual_ignored_test_log(self):
        with tempfile.TemporaryDirectory() as directory:
            log = Path(directory) / "tests.log"
            log.write_text("test retired ... ignored, reason\n" + summary(1), encoding="utf-8")
            result = subprocess.run([sys.executable, str(SCRIPT), "--scope", "core", "--log", str(log)],
                                    capture_output=True, text=True)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("ignored", result.stderr)


if __name__ == "__main__":
    unittest.main()
