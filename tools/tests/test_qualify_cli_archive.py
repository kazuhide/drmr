"""Qualification controls are tested independently of real packaged-media proof."""

import copy
import importlib.util
import io
import json
import os
from pathlib import Path
import subprocess
import sys
import tarfile
import tempfile
import unittest
from unittest.mock import patch


SPEC = importlib.util.spec_from_file_location(
    "qualify_cli_archive", Path(__file__).resolve().parents[1] / "qualify_cli_archive.py")
module = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(module)


class EvidenceTests(unittest.TestCase):
    def proof(self):
        return {"schema_version": 1, "passed": True, "media_verified": True,
                "license_playback_verified": False,
                "checks": {"stream_metadata": "pass", "packet_payloads_equal": True,
                           "packet_timing_equal": True, "decoded_content_equal": True},
                "native_manifest": {"schema_version": 1, "scope": "manifest-only",
                                    "profile": "dash-static-manifest", "errors": 0,
                                    "warnings": 0, "truncated": False, "incomplete": False,
                                    "document_count": 1}}

    def test_full_media_and_native_manifest_evidence_are_required(self):
        result = module.verification(self.proof())
        self.assertTrue(result["media_verified"])
        changes = [lambda p: p.update(passed=False), lambda p: p.update(media_verified=1),
                   lambda p: p.update(license_playback_verified=True),
                   lambda p: p["checks"].update(packet_payloads_equal=False),
                   lambda p: p["checks"].update(packet_timing_equal=None),
                   lambda p: p["checks"].update(decoded_content_equal="true"),
                   lambda p: p["checks"].update(stream_metadata=True),
                   lambda p: p.pop("native_manifest"),
                   lambda p: p["native_manifest"].update(errors=1),
                   lambda p: p["native_manifest"].update(incomplete=True),
                   lambda p: p["native_manifest"].update(truncated=True),
                   lambda p: p["native_manifest"].update(document_count=0)]
        for change in changes:
            altered = self.proof()
            change(altered)
            with self.subTest(change=change), self.assertRaises(module.Refusal):
                module.verification(altered)

    def test_reports_never_retain_unrecognized_fields_or_raw_diagnostics(self):
        raw = self.proof()
        raw["private_path"] = "private-marker"
        raw["checks"]["private_key"] = "private-marker"
        raw["native_manifest"]["diagnostics"] = ["private-marker"]
        self.assertNotIn("private-marker", json.dumps(module.verification(raw)))

    def test_conversion_uses_actual_output_inspection_and_the_same_saved_plan(self):
        report = {"plan_id": "a" * 64, "input": {"format": "dash", "protection": "clear"},
                  "output": {"format": "dash", "protection": "cbcs"}, "verification": self.proof()}
        module.conversion_proof(report, "a" * 64)
        for change in (lambda r: r.update(plan_id="b" * 64),
                       lambda r: r["output"].update(protection="clear"),
                       lambda r: r["output"].update(format="mp4"),
                       lambda r: r["input"].update(protection="cenc")):
            altered = copy.deepcopy(report)
            change(altered)
            with self.assertRaisesRegex(module.Refusal, "^conversion-proof-invalid$"):
                module.conversion_proof(altered, "a" * 64)

    def test_doctor_requires_every_actual_dependency_and_confirmed_cleanup(self):
        report = {"schema_version": 1, "mode": "probe", "profile": "all", "ready": True,
                  "cleanup_status": "confirmed", "cancelled": False, "media_compatibility_checked": False,
                  "tools": [{"name": name, "available": True, "required": True,
                             "version_status": "build-unverified" if name == "mp4dump" else "supported",
                             "feature_status": "available", "failure_code": None,
                             "path": "private-marker", "version": "private-marker"}
                            for name in ("ffmpeg", "ffprobe", "mp4dump", "packager")]}
        self.assertNotIn("private-marker", json.dumps(module.doctor(report)))
        for change in (lambda r: r.update(cleanup_status="unconfirmed"),
                       lambda r: r["tools"].pop(),
                       lambda r: r["tools"][0].update(failure_code="tool-failed"),
                       lambda r: r["tools"][0].update(feature_status="missing")):
            altered = copy.deepcopy(report)
            change(altered)
            with self.assertRaisesRegex(module.Refusal, "^doctor-proof-invalid$"):
                module.doctor(altered)

    def test_duplicate_members_and_nonfinite_json_are_rejected(self):
        for raw in (b'{"passed":false,"passed":true}', b'{"nested":{"a":1,"a":2}}',
                    b'{"number":NaN}', b'{} {}', b'\xff'):
            with self.subTest(raw=raw), self.assertRaisesRegex(module.Refusal, "^json-invalid$"):
                module.load_json(raw)

    def test_native_target_gate_refuses_emulation_and_unknown_platforms(self):
        module.require_native("aarch64-apple-darwin", "Darwin", "arm64", False)
        module.require_native("x86_64-pc-windows-msvc", "Windows", "AMD64", False)
        module.require_native("x86_64-unknown-linux-gnu", "Linux", "x86_64", False)
        for target, system, machine in (("x86_64-apple-darwin", "Darwin", "arm64"),
                                        ("aarch64-unknown-linux-gnu", "Linux", "x86_64"),
                                        ("x86_64-pc-windows-msvc", "Linux", "x86_64"),
                                        ("x86_64-unknown-linux-gnu", "Other", "x86_64")):
            with self.subTest(target=target), self.assertRaisesRegex(module.Refusal, "^native-target-required$"):
                module.require_native(target, system, machine, False)

    def test_elevated_media_execution_is_refused(self):
        with self.assertRaisesRegex(module.Refusal, "^ordinary-user-required$"):
            module.require_native("x86_64-pc-windows-msvc", "Windows", "AMD64", True)

    def test_exact_source_version_is_checked_before_any_binary_execution(self):
        class Source:
            def read(self, _):
                return b'[workspace.package]\nversion = "1.0.0"\n[workspace.dependencies]\n'
        module.require_version(Source(), "1.0.0")
        for version in ("0.1.0", "v1.0.0", "01.0.0", "1.0.0\nprivate-marker"):
            with self.subTest(version=version), self.assertRaises(module.Refusal):
                module.require_version(Source(), version)

    def test_full_packet_oracle_checks_last_packet_and_all_frame_counts(self):
        raw = {"streams": [{"codec_name": "h264", "time_base": "1/12288", "nb_read_frames": "240"}],
               "packets": [{"pts": i * 512, "dts": (i - 2) * 512, "duration": 512,
                            "data_hash": "SHA256:" + format(i, "064x")} for i in range(240)]}
        module.video_evidence(raw)
        for change in (lambda p: p["packets"].pop(),
                       lambda p: p["packets"][-1].update(duration=0),
                       lambda p: p["streams"][0].update(nb_read_frames="239")):
            altered = copy.deepcopy(raw)
            change(altered)
            with self.assertRaisesRegex(module.Refusal, "^video-proof-invalid$"):
                module.video_evidence(altered)


class OwnedFilesTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)

    def test_source_archive_is_a_bounded_regular_file_and_symlinks_are_refused(self):
        original = self.root / "archive.tar.gz"
        original.write_bytes(b"archive")
        destination = self.root / "snapshot.tar.gz"
        module.snapshot_archive(original, destination)
        self.assertEqual(destination.read_bytes(), b"archive")
        with patch.object(module.VERIFY, "MAX_TOTAL", 3):
            with self.assertRaisesRegex(module.Refusal, "^archive-input-invalid$"):
                module.snapshot_archive(original, self.root / "oversized.tar.gz")
        with self.assertRaises(module.Refusal):
            module.snapshot_archive(self.root, self.root / "directory.tar.gz")
        if os.name != "nt":
            linked = self.root / "linked.tar.gz"
            linked.symlink_to(original)
            with self.assertRaisesRegex(module.Refusal, "^archive-input-invalid$"):
                module.snapshot_archive(linked, self.root / "linked-snapshot.tar.gz")

    def test_archive_links_and_traversal_fail_before_a_binary_is_written(self):
        for name, kind in (("../escape", tarfile.REGTYPE), ("package/drmr", tarfile.SYMTYPE)):
            archive = self.root / "malformed.tar.gz"
            with tarfile.open(archive, "w:gz") as output:
                item = tarfile.TarInfo(name)
                item.type = kind
                item.linkname = "escape"
                output.addfile(item)
            destination = self.root / "extracted"
            with self.subTest(name=name), self.assertRaises(module.Refusal):
                module.extract_cli(archive, "x86_64-unknown-linux-gnu", "0" * 64, destination)
            self.assertFalse(destination.exists())
            self.assertFalse((self.root / "escape").exists())

    def test_changed_binary_cannot_be_executed_after_archive_verification(self):
        archive = self.root / "changed.tar.gz"
        with tarfile.open(archive, "w:gz") as output:
            item = tarfile.TarInfo("arbitrary-versioned-root/drmr")
            item.size = 64
            item.mode = 0o755
            output.addfile(item, io.BytesIO(b"unverified".ljust(64, b"\0")))
        destination = self.root / "drmr"
        with self.assertRaises(module.Refusal):
            module.extract_cli(archive, "x86_64-unknown-linux-gnu", "0" * 64, destination)
        self.assertFalse(destination.exists())

    def test_verified_binary_is_extracted_from_an_arbitrary_versioned_archive_root(self):
        payload = bytearray(64)
        payload[:7] = b"\x7fELF\x02\x01\x01"
        payload[16:20] = b"\x02\x00\x3e\x00"
        archive = self.root / "versioned.tar.gz"
        with tarfile.open(archive, "w:gz") as output:
            item = tarfile.TarInfo("drmr-1.0.0-linux/drmr")
            item.size = len(payload)
            item.mode = 0o755
            output.addfile(item, io.BytesIO(payload))
        destination = self.root / "drmr"
        actual = module.extract_cli(archive, "x86_64-unknown-linux-gnu", module.VERIFY.sha(payload), destination)
        self.assertEqual(actual, destination)
        self.assertEqual(destination.read_bytes(), payload)
        # The synthetic header is never executed and is not media evidence.

    def test_failed_full_archive_verification_cannot_reach_extraction_or_execution(self):
        archive = self.root / "candidate.tar.gz"
        archive.write_bytes(b"candidate")
        source = type("Source", (), {"head": "a" * 40, "tree": "b" * 40})()
        with patch.object(module, "require_native"), patch.object(module, "require_version"), \
                patch.object(module.VERIFY, "Source", return_value=source), \
                patch.object(module.VERIFY, "metadata", return_value={}), \
                patch.object(module.VERIFY, "build_expectation", return_value={}), \
                patch.object(module.VERIFY, "qualify", side_effect=module.VERIFY.Failure("private-marker")), \
                patch.object(module, "extract_cli") as extraction, patch.object(module, "Runner") as execution:
            with self.assertRaisesRegex(module.Refusal, "^archive-verification-failed$"):
                module.qualify(archive, "x86_64-unknown-linux-gnu", "1.0.0", self.root)
            extraction.assert_not_called()
            execution.assert_not_called()

    def test_corrupt_compression_has_only_a_fixed_diagnostic(self):
        archive = self.root / "malformed-private-marker.tar.gz"
        archive.write_bytes(b"not a compressed archive")
        with self.assertRaisesRegex(module.Refusal, "^archive-extraction-invalid$") as denied:
            module.extract_cli(archive, "x86_64-unknown-linux-gnu", "0" * 64, self.root / "drmr")
        self.assertNotIn("private-marker", str(denied.exception))

    def test_cleanup_never_follows_a_changed_workspace_identity(self):
        workspace = module.Workspace(self.root)
        foreign = self.root / "foreign"
        foreign.mkdir()
        marker = foreign / "keep"
        marker.write_bytes(b"keep")
        owned = workspace.path
        renamed = self.root / "renamed-owned"
        owned.rename(renamed)
        if os.name == "nt":
            owned.mkdir()
        else:
            owned.symlink_to(foreign, target_is_directory=True)
        with self.assertRaisesRegex(module.Refusal, "^owned-cleanup-unconfirmed$"):
            workspace.close()
        self.assertEqual(marker.read_bytes(), b"keep")

    def test_graph_identity_detects_last_segment_changes(self):
        graph = self.root / "source"
        graph.mkdir()
        (graph / "manifest.mpd").write_bytes(b"manifest")
        (graph / "seg_5.m4s").write_bytes(b"last")
        before = module.graph_identity(graph)
        (graph / "seg_5.m4s").write_bytes(b"lost")
        self.assertNotEqual(before, module.graph_identity(graph))

    def test_plan_destination_uses_canonical_location_and_refuses_another_output(self):
        output = self.root / "converted"
        intent = {"format": "dash", "target": "cbcs", "output": str(output)}
        module.planned_intent(intent, output)
        if os.name == "nt":
            module.planned_intent({**intent, "output": "\\\\?\\" + str(output.resolve())}, output)
        with self.assertRaisesRegex(module.Refusal, "^plan-proof-invalid$"):
            module.planned_intent({**intent, "output": str(self.root / "other")}, output)

    def test_graph_symlinks_and_resource_limits_are_refused(self):
        graph = self.root / "source"
        graph.mkdir()
        (graph / "manifest.mpd").write_bytes(b"manifest")
        with patch.object(module, "MAX_GRAPH_BYTES", 3):
            with self.assertRaisesRegex(module.Refusal, "^media-graph-limit$"):
                module.graph_identity(graph)
        if os.name != "nt":
            (graph / "outside").symlink_to(self.root / "not-owned")
            with self.assertRaisesRegex(module.Refusal, "^media-graph-invalid$"):
                module.graph_identity(graph)


class ProcessTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.runner = module.Runner(self.root, secrets=(b"private-marker",))

    def test_child_stderr_and_nonzero_failures_never_enter_public_diagnostics(self):
        script = "import sys; sys.stderr.write('secret operator path'); sys.exit(7)"
        with self.assertRaisesRegex(module.Refusal, "^command-failed$") as denied:
            self.runner.run([sys.executable, "-c", script], "version", timeout=3)
        self.assertNotIn("secret", str(denied.exception))
        self.assertEqual(denied.exception.stage, "version")
        self.assertEqual(denied.exception.evidence["exit_code"], 7)
        self.assertNotIn("secret", json.dumps(denied.exception.evidence))

    def test_secret_output_cannot_be_accepted_even_with_a_zero_exit_code(self):
        with self.assertRaisesRegex(module.Refusal, "^secret-displayed$"):
            self.runner.run([sys.executable, "-c", "print('private-marker')"], "version", timeout=3)

    def test_output_limit_is_checked_when_child_has_already_exited(self):
        with patch.object(module, "MAX_OUTPUT", 32):
            with self.assertRaisesRegex(module.Refusal, "^command-output-limit$"):
                self.runner.run([sys.executable, "-c", "print('x'*200)"], "version", timeout=3)

    def test_deadline_reaps_the_owned_child_and_never_returns_a_success(self):
        expected = "^command-cleanup-unconfirmed$" if os.name == "nt" else "^command-deadline$"
        with self.assertRaisesRegex(module.Refusal, expected):
            self.runner.run([sys.executable, "-c", "import time; time.sleep(10)"],
                            "version", timeout=0.05)

    def test_main_refuses_to_overwrite_a_report(self):
        report = self.root / "report.json"
        report.write_text("original", encoding="utf-8")
        self.assertEqual(module.main(["--archive", "unused", "--target", "x86_64-unknown-linux-gnu",
                                      "--expected-version", "1.0.0", "--report", str(report)]), 1)
        self.assertEqual(report.read_text(encoding="utf-8"), "original")

    def test_cleanup_failure_cannot_leave_a_successful_receipt(self):
        report = self.root / "report.json"
        with patch.object(module, "qualify", side_effect=module.Refusal("owned-cleanup-unconfirmed")):
            self.assertEqual(module.main(["--archive", "unused", "--target", "x86_64-unknown-linux-gnu",
                                          "--expected-version", "1.0.0", "--report", str(report)]), 1)
        result = json.loads(report.read_text())
        self.assertIs(result["success"], False)
        self.assertEqual(result["failure_code"], "owned-cleanup-unconfirmed")


if __name__ == "__main__":
    unittest.main()
