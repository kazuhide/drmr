"""Codec archive qualification is finite, native-only and bound to real fixtures."""
import copy
import contextlib
import io
from pathlib import Path
import sys
import subprocess
import tempfile
import threading
import unittest
from unittest.mock import Mock, patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import codec_contract as CONTRACT
import codec_reports
import codec_qualification as RUN


class CodecMatrix(unittest.TestCase):
    def validate(self, report, scope="smoke"):
        return CONTRACT.validate(report, scope, "a" * 40, "b" * 40, "aarch64-apple-darwin", "1.0.3", "c" * 64, "d" * 64)

    def test_committed_profile_depth_fixtures_have_exact_digests(self):
        self.assertEqual(len(CONTRACT.FIXTURES), 8)
        self.assertEqual({item["codec"] for item in CONTRACT.FIXTURES}, {"hevc", "vp9"})
        for item in CONTRACT.FIXTURES:
            self.assertEqual(CONTRACT.sha((CONTRACT.ROOT / item["path"]).read_bytes()), item["sha256"])

    def test_release_matrix_covers_every_valid_pair_and_explicit_segment_modes(self):
        cases = CONTRACT.cases("full")
        self.assertEqual(len(cases), 534)
        self.assertEqual(len({case["id"] for case in cases}), len(cases))
        for fixture in CONTRACT.FIXTURES:
            selected = [case for case in cases if case["fixture"] == fixture["id"]]
            profiles = CONTRACT.profiles(fixture)
            automatic = [case for case in selected if case["processing"] == "auto"]
            self.assertEqual({(tuple(case["source"]), tuple(case["target"])) for case in automatic},
                             {(source, target) for source in profiles for target in profiles})
            self.assertEqual({case["processing"] for case in selected}, {"auto", "segments", "stitch"})
            self.assertFalse(any("hls-ts" in case["source"] + case["target"] for case in selected))
            if fixture["codec"] == "vp9":
                self.assertFalse(any("cbcs" in case["source"] + case["target"] for case in selected))

    def test_normal_archive_smoke_covers_every_profile_without_running_full_matrix(self):
        smoke = CONTRACT.cases("smoke")
        self.assertEqual(len(smoke), 8)
        self.assertEqual({case["fixture"] for case in smoke}, {fixture["id"] for fixture in CONTRACT.FIXTURES})
        self.assertTrue(all(case["source"] == ["mp4", "clear"] for case in smoke))
        self.assertTrue(all(case["processing"] == "auto" for case in smoke))
        with self.assertRaises(CONTRACT.Failure):
            CONTRACT.cases("unknown")

    def test_complete_smoke_and_full_evidence_are_accepted_separately(self):
        for scope in ("smoke", "full"):
            report = codec_reports.complete(CONTRACT, scope)
            self.assertIs(self.validate(report, scope), report)
        with self.assertRaises(CONTRACT.Failure):
            self.validate(codec_reports.complete(CONTRACT, "smoke"), "full")

    def test_missing_reordered_duplicate_and_unrun_records_are_refused(self):
        original = codec_reports.complete(CONTRACT)
        for change in (lambda r: r["records"].pop(), lambda r: r["records"].reverse(),
                       lambda r: r["records"].__setitem__(1, r["records"][0]),
                       lambda r: r["records"][0].update(status="not-checked"),
                       lambda r: r["refusals"].pop(), lambda r: r["refusals"][0].update(exit_code=2),
                       lambda r: r["refusals"][0].update(code="command-unavailable"),
                       lambda r: r["refusals"][0].update(destination_absent=False)):
            value = copy.deepcopy(original)
            change(value)
            with self.assertRaises(CONTRACT.Failure):
                self.validate(value)

    def test_source_archive_fixture_binary_and_scope_bindings_are_required(self):
        original = codec_reports.complete(CONTRACT)
        for field in ("source_head", "source_tree", "target", "version", "archive_sha256", "binary_sha256", "matrix_sha256", "scope"):
            value = copy.deepcopy(original)
            value[field] = "mismatch"
            with self.subTest(field=field), self.assertRaises(CONTRACT.Failure):
                self.validate(value)
        for change in (lambda r: r["fixtures"][0].update(bit_depth=10),
                       lambda r: r["fixtures"][0].update(sha256="0" * 64),
                       lambda r: r["counts"].update(failed=False),
                       lambda r: r["decoder_guard"].update(ffmpeg="available"),
                       lambda r: r.update(native_ordinary_user=False),
                       lambda r: r.update(owned_workspace_cleanup="unconfirmed"),
                       lambda r: r.update(license_playback_verified=True),
                       lambda r: r.update(private_path="operator-value")):
            value = copy.deepcopy(original)
            change(value)
            with self.assertRaises(CONTRACT.Failure):
                self.validate(value)

    def test_all_four_steps_and_native_media_manifest_proofs_are_required(self):
        original = codec_reports.complete(CONTRACT)
        for change in (lambda r: r["records"][0]["forward"].pop("analysis"),
                       lambda r: r["records"][0]["forward"]["analysis"].update(scope="comparison"),
                       lambda r: r["records"][0]["forward"]["plan"].update(sha256=""),
                       lambda r: r["records"][0]["forward"]["plan"]["processing"].update(requested="stitch"),
                       lambda r: r["records"][0]["forward"]["conversion"]["checks"].update(packet_payloads_equal=False),
                       lambda r: r["records"][0]["forward"]["independent_verification"]["native_manifest"].update(errors=1),
                       lambda r: r["records"][0]["forward"].update(source_and_plan_unchanged=False),
                       lambda r: r["records"][0].pop("final_against_original")):
            value = copy.deepcopy(original)
            change(value)
            with self.assertRaises(CONTRACT.Failure):
                self.validate(value)

    def test_case_finishes_with_original_seed_comparison_and_two_different_key_slots(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            seed_root = root / "source"
            seed_root.mkdir()
            seed = seed_root / "seed.mp4"
            seed.write_bytes(b"retained-original")
            case = CONTRACT.cases("smoke")[0]
            legs = codec_reports.complete(CONTRACT)["records"][0]
            executor = Mock(cancel=threading.Event())
            executor.convert.side_effect = [(root / "forward.mp4", legs["forward"]),
                                            (root / "final.mp4", legs["backward"])]
            executor.verify.return_value = legs["final_against_original"]
            result = RUN.execute_case(case, executor, {(case["fixture"], ("mp4", "clear")): seed},
                                      {case["fixture"]: seed}, {item["id"]: item for item in CONTRACT.FIXTURES}, root)
            self.assertEqual(result["status"], "complete")
            self.assertEqual(executor.convert.call_args_list[0].args[-1], 1)
            self.assertEqual(executor.convert.call_args_list[1].args[-1], 0)
            self.assertEqual(executor.verify.call_args.args[1], seed)
            self.assertEqual(executor.verify.call_args.args[2], root / "final.mp4")
            self.assertEqual(seed.read_bytes(), b"retained-original")
            self.assertEqual(list(root.iterdir()), [seed_root])

    def test_cli_keeps_existing_report_and_does_not_echo_private_arguments(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            report = root / "report.json"
            report.write_bytes(b"preserved")
            output = io.StringIO()
            with contextlib.redirect_stdout(output), patch.object(RUN, "qualify_archive") as qualify:
                status = RUN.main(["--archive", "private-marker", "--target", "private-marker", "--expected-version", "1.0.3",
                                   "--source", str(root), "--report", str(report)])
            self.assertEqual(status, 1)
            self.assertFalse(qualify.called)
            self.assertEqual(report.read_bytes(), b"preserved")
            self.assertNotIn("private-marker", output.getvalue())

    def test_cli_does_not_publish_a_partial_or_failed_native_run_as_success(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / "source"
            source.mkdir()
            report = root / "report.json"
            output = io.StringIO()
            with contextlib.redirect_stdout(output), patch.object(RUN, "qualify_archive", side_effect=OSError("private-marker")):
                status = RUN.main(["--archive", "private-marker", "--target", "aarch64-apple-darwin", "--expected-version", "1.0.3",
                                   "--source", str(source), "--report", str(report), "--scope", "full"])
            self.assertEqual(status, 1)
            self.assertNotIn("private-marker", output.getvalue() + report.read_text())
            self.assertIn('"success": false', report.read_text())

    def test_metadata_timeouts_and_malformed_child_reports_have_only_fixed_diagnostics(self):
        for error in (subprocess.TimeoutExpired(["private-marker"], 1), AttributeError("private-marker")):
            with tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                source = root / "source"
                source.mkdir()
                report = root / "report.json"
                output = io.StringIO()
                with contextlib.redirect_stdout(output), patch.object(RUN, "qualify_archive", side_effect=error):
                    status = RUN.main(["--archive", "private-marker", "--target", "aarch64-apple-darwin", "--expected-version", "1.0.3",
                                       "--source", str(source), "--report", str(report)])
                self.assertEqual(status, 1)
                self.assertNotIn("private-marker", output.getvalue() + report.read_text())

    def test_failure_stage_is_fixed_and_unknown_child_fields_remain_private(self):
        case = CONTRACT.cases("smoke")[0]
        for stage, expected in (("codec-convert", "codec-convert"), ("private-marker", None)):
            error = RUN.SUPPORT.Refusal("command-failed", stage=stage, evidence={"exit_code": 1,
                "fixed_cli_code": "dash-nominal-timescale-not-representable", "raw": "private-marker"})
            result = RUN.failure(case, error)
            self.assertEqual(result.get("failed_stage"), expected)
            self.assertEqual(result["failure_detail"]["fixed_cli_code"], "dash-nominal-timescale-not-representable")
            self.assertNotIn("private-marker", str(result))

    def test_archive_requalification_refuses_dirty_source_before_execution(self):
        with patch.object(RUN.SUPPORT, "require_native"), patch.object(RUN.SUPPORT.VERIFY, "command", return_value=b" M source\n"), \
                patch.object(RUN.SUPPORT.VERIFY, "Source") as source, patch.object(RUN.SUPPORT, "Runner") as runner:
            with self.assertRaisesRegex(RUN.CONTRACT.Failure, "^codec-clean-source-required$"):
                RUN.qualify_archive(Path("candidate"), "aarch64-apple-darwin", "1.0.3", Path("source"))
            self.assertFalse(source.called)
            self.assertFalse(runner.called)

    def test_source_change_after_native_execution_cannot_qualify_the_archive(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            archive_path = root / "candidate.archive"
            archive_path.write_bytes(b"archive")
            archive_hash, binary_hash = CONTRACT.sha(b"archive"), CONTRACT.sha(b"binary")
            source = Mock(head="a" * 40, tree="b" * 40)
            changed = Mock(head="e" * 40, tree="b" * 40)
            archive = {"success": True, "archive_sha256": archive_hash, "binaries": {"drmr": {"sha256": binary_hash}}}
            real_workspace = RUN.SUPPORT.Workspace

            def extract(_archive, _target, _digest, output):
                output.write_bytes(b"binary")
                return output

            with patch.object(RUN.SUPPORT, "require_native"), patch.object(RUN.SUPPORT, "require_version"), \
                    patch.object(RUN.SUPPORT.VERIFY, "command", return_value=b""), \
                    patch.object(RUN.SUPPORT.VERIFY, "Source", side_effect=[source, changed]), \
                    patch.object(RUN.SUPPORT.VERIFY, "metadata", return_value={}), \
                    patch.object(RUN.SUPPORT.VERIFY, "build_expectation", return_value={}), \
                    patch.object(RUN.SUPPORT.VERIFY, "qualify", return_value=archive), \
                    patch.object(RUN.SUPPORT, "Workspace", side_effect=lambda: real_workspace(parent=root)), \
                    patch.object(RUN.SUPPORT, "extract_cli", side_effect=extract), \
                    patch.object(RUN.SUPPORT, "Runner", return_value=Mock(run=Mock(return_value=b"drmr 1.0.3\n"))), \
                    patch.object(RUN, "qualify", return_value={"success": True}) as native:
                with self.assertRaisesRegex(RUN.CONTRACT.Failure, "^codec-source-or-artifact-changed$"):
                    RUN.qualify_archive(archive_path, "aarch64-apple-darwin", "1.0.3", root)
                self.assertEqual(native.call_args.args[2], {"source_head": source.head, "source_tree": source.tree,
                    "target": "aarch64-apple-darwin", "version": "1.0.3", "archive_sha256": archive_hash, "binary_sha256": binary_hash})
            self.assertEqual(list(root.iterdir()), [archive_path])


if __name__ == "__main__":
    unittest.main()
