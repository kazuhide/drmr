"""Qualification controls are tested independently of real packaged-media proof."""

import base64
import copy
import importlib.util
import io
import json
import os
from pathlib import Path
import stat
import subprocess
import sys
import tarfile
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch
import zipfile


SPEC = importlib.util.spec_from_file_location(
    "qualify_cli_archive", Path(__file__).resolve().parents[1] / "qualify_cli_archive.py")
module = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(module)


class EvidenceTests(unittest.TestCase):
    def test_current_plan_requires_saved_decoding_and_resolved_stitch_boundaries(self):
        current = {"schema_version": 5, "required_depth": "decoded", "processing": {
            "requested": "auto", "resolved": "stitch", "boundaries": "resegment", "segment_duration_ms": 2000}}
        self.assertTrue(module.plan_depth(current))
        for change in [
                lambda p: p.update(required_depth="native"),
                lambda p: p.pop("processing"),
                lambda p: p["processing"].update(resolved="auto"),
                lambda p: p["processing"].update(boundaries="preserve"),
                lambda p: p["processing"].update(segment_duration_ms=2000.0),
                lambda p: p["processing"].update(segment_duration_ms=None),
                lambda p: p["processing"].update(unrecognized=True)]:
            altered = copy.deepcopy(current)
            change(altered)
            with self.subTest(plan=altered):
                self.assertFalse(module.plan_depth(altered))
        self.assertTrue(module.plan_depth({"schema_version": 1}))
        self.assertTrue(module.plan_depth({"schema_version": 3, "required_depth": "decoded"}))
        self.assertFalse(module.plan_depth({"schema_version": 3, "required_depth": "native"}))

    def schema_seven_plan(self, source_format):
        source, kind, playlist = {
            "dash": ("manifest.mpd", "video", "playlist-0000.m3u8"),
            "hls-ts": ("media.m3u8", "audio", "media.m3u8"),
        }[source_format]
        return module.load_json(json.dumps({
            "schema_version": 7, "required_depth": "decoded", "source_format": source_format,
            "processing": {"requested": "auto", "resolved": "stitch",
                           "boundaries": "resegment", "segment_duration_ms": 2000},
            "output_options": {"layout": {"mode": "source", "tracks": [{
                "output_ordinal": 0, "source": source, "selector": 0, "kind": kind,
                "media_directory": "", "playlist": playlist, "generated_stem": "track-0000-",
            }]}, "dash_addressing": "template"},
        }).encode())

    def test_schema_seven_accepts_both_serialized_fixture_layouts(self):
        for source_format in ("dash", "hls-ts"):
            with self.subTest(source_format=source_format):
                self.assertTrue(module.plan_depth(self.schema_seven_plan(source_format)))

    def test_schema_seven_refuses_changed_depth_processing_or_output_options(self):
        changes = [
            lambda p: p.update(schema_version=9),
            lambda p: p.update(schema_version=True),
            lambda p: p.update(required_depth="native"),
            lambda p: p.update(source_format="mp4"),
            lambda p: p.update(source_format=[]),
            lambda p: p.update(source_format=None),
            lambda p: p.pop("processing"),
            lambda p: p["processing"].update(requested="stitch"),
            lambda p: p["processing"].update(resolved="segments"),
            lambda p: p["processing"].update(boundaries="preserve"),
            lambda p: p["processing"].update(segment_duration_ms=2000.0),
            lambda p: p["processing"].update(segment_duration_ms=6000),
            lambda p: p["processing"].update(unrecognized=True),
            lambda p: p.pop("output_options"),
            lambda p: p.update(output_options=None),
            lambda p: p["output_options"].update(dash_addressing="template-timeline"),
            lambda p: p["output_options"].update(unrecognized=True),
            lambda p: p["output_options"].pop("layout"),
            lambda p: p["output_options"]["layout"].update(mode="normalized"),
            lambda p: p["output_options"]["layout"].update(unrecognized=True),
            lambda p: p["output_options"]["layout"].update(tracks=[]),
            lambda p: p["output_options"]["layout"]["tracks"].append(
                copy.deepcopy(p["output_options"]["layout"]["tracks"][0])),
        ]
        for field, value in (
                ("output_ordinal", False), ("selector", False), ("source", "other.mpd"),
                ("kind", "text"), ("media_directory", "track-0000"),
                ("playlist", "other.m3u8"), ("generated_stem", "other-"),
                ("unrecognized", True)):
            changes.append(lambda p, field=field, value=value:
                           p["output_options"]["layout"]["tracks"][0].update({field: value}))
        for source_format in ("dash", "hls-ts"):
            for index, change in enumerate(changes):
                altered = self.schema_seven_plan(source_format)
                change(altered)
                with self.subTest(source_format=source_format, mutation=index):
                    self.assertFalse(module.plan_depth(altered))

    def audio_probe(self):
        return {"streams": [{"codec_name": "aac", "profile": "LC", "sample_rate": "44100",
                             "channels": 2, "time_base": "1/90000", "nb_read_frames": "432"}],
                "packets": [{"pts": 126000 + index * 1024 * 90000 // 44100,
                             "dts": 126000 + index * 1024 * 90000 // 44100,
                             "duration": 2089} for index in range(432)]}

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
                             "version_status": "supported",
                             "feature_status": "available", "failure_code": None,
                             "path": "private-marker", "version": "private-marker"}
                            for name in ("ffmpeg", "ffprobe", "packager")]}
        result = module.doctor(report)
        self.assertEqual({tool["name"] for tool in result["tools"]}, {"ffmpeg", "ffprobe", "packager"})
        self.assertEqual(len(result["tools"]), 3)
        self.assertNotIn("private-marker", json.dumps(result))
        for label, change in (
                ("cleanup", lambda r: r.update(cleanup_status="unconfirmed")),
                ("missing", lambda r: r["tools"].pop()),
                ("extra", lambda r: r["tools"].append({**r["tools"][0], "name": "mp4dump"})),
                ("duplicate", lambda r: r["tools"][1].update(name="ffmpeg"))):
            altered = copy.deepcopy(report)
            change(altered)
            with self.subTest(case=label), self.assertRaisesRegex(module.Refusal, "^doctor-proof-invalid$"):
                module.doctor(altered)
        for index, tool in enumerate(report["tools"]):
            for field, value in (("available", False), ("required", False),
                                 ("version_status", "unsupported"), ("version_status", "build-unverified"),
                                 ("feature_status", "missing"), ("failure_code", "tool-failed")):
                altered = copy.deepcopy(report)
                altered["tools"][index][field] = value
                with self.subTest(tool=tool["name"], field=field, value=value), self.assertRaisesRegex(
                        module.Refusal, "^doctor-proof-invalid$"):
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

    def test_aac44100_probe_proves_every_frame_and_exact_original_clock(self):
        raw = self.audio_probe()
        result = module.audio_evidence(raw)
        self.assertEqual(result["sample_rate"], 44100)
        self.assertEqual(result["frame_count"], 432)
        self.assertEqual(result["packet_count"], 432)
        self.assertEqual(result["expected_decoded_samples_per_channel"], 432 * 1024)
        self.assertEqual(result["first_pts"], raw["packets"][0]["pts"])
        self.assertEqual(result["last_pts"], raw["packets"][-1]["pts"])
        changed = copy.deepcopy(raw)
        changed["packets"][-1]["pts"] += 1
        changed["packets"][-1]["dts"] += 1
        self.assertNotEqual(result["packet_clock_sha256"], module.audio_evidence(changed)["packet_clock_sha256"])
        raw["streams"][0]["private_path"] = "private-marker"
        raw["packets"][0]["private_key"] = "private-marker"
        self.assertNotIn("private-marker", json.dumps(module.audio_evidence(raw)))

    def test_aac44100_probe_refuses_missing_frames_and_wrong_configuration(self):
        for change in (lambda r: r["streams"][0].update(sample_rate="48000"),
                       lambda r: r["streams"][0].update(profile="HE-AAC"),
                       lambda r: r["streams"][0].update(channels=1),
                       lambda r: r["streams"][0].update(channels=True),
                       lambda r: r["streams"][0].update(time_base="1/44100"),
                       lambda r: r["streams"][0].update(nb_read_frames="431"),
                       lambda r: r["packets"].pop(),
                       lambda r: r["packets"][-1].update(duration=0),
                       lambda r: r["packets"][-1].update(pts=True),
                       lambda r: r["packets"][-1].update(dts=-1)):
            raw = self.audio_probe()
            change(raw)
            with self.subTest(change=change), self.assertRaisesRegex(module.Refusal, "^aac44100-proof-invalid$"):
                module.audio_evidence(raw)

    def test_aac44100_conversion_requires_the_saved_plan_and_actual_ts_input(self):
        report = {"plan_id": "a" * 64, "input": {"format": "hls", "protection": "clear"},
                  "output": {"format": "dash", "protection": "cbcs"}, "verification": self.proof()}
        module.audio_conversion_proof(report, "a" * 64)
        for change in (lambda r: r.update(plan_id="b" * 64),
                       lambda r: r["input"].update(format="dash"),
                       lambda r: r["input"].update(protection="hls-aes128"),
                       lambda r: r["output"].update(format="hls"),
                       lambda r: r["output"].update(protection="clear")):
            altered = copy.deepcopy(report)
            change(altered)
            with self.assertRaisesRegex(module.Refusal, "^aac44100-conversion-proof-invalid$"):
                module.audio_conversion_proof(altered, "a" * 64)
        report["verification"]["checks"]["packet_timing_equal"] = False
        with self.assertRaisesRegex(module.Refusal, "^media-proof-invalid$"):
            module.audio_conversion_proof(report, "a" * 64)


class MetadataAnalysisTests(unittest.TestCase):
    """Metadata orchestration controls do not substitute for packaged execution."""

    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.source = self.root / "manifest.mpd"
        self.source.write_bytes(b"<MPD/>")
        self.cli = self.root / "extracted-cli"
        self.raw = {"schema_version": 1, "format": "dash", "protection": "clear",
                    "evidence": "manifest", "media_verified": False,
                    "findings": [], "tracks": [], "private_path": "private-marker"}

    def run_metadata(self, change=None, changed_source=False):
        raw = copy.deepcopy(self.raw)
        if change:
            change(raw)
        calls = []

        def execute(arguments, stage):
            calls.append((arguments, stage))
            if changed_source:
                self.source.write_bytes(b"<MPD changed='true'/>")
            return raw

        result = module.metadata_analysis(self.cli, SimpleNamespace(json=execute), self.source)
        return result, calls

    def test_metadata_stage_executes_only_the_extracted_cli_and_projects_fixed_evidence(self):
        result, calls = self.run_metadata()
        self.assertEqual(calls, [([self.cli, "analyze", self.source, "--metadata-only", "--json"],
                                 "metadata-analysis")])
        self.assertEqual(result, {"schema_version": 1, "format": "dash", "protection": "clear",
                                  "evidence": "manifest", "media_verified": False, "source_unchanged": True})
        self.assertNotIn("private-marker", json.dumps(result))

    def test_metadata_stage_refuses_admission_claims_wrong_shapes_and_retained_downloads(self):
        for change in (lambda r: r.update(schema_version=True), lambda r: r.update(schema_version=2),
                       lambda r: r.update(format="mp4"), lambda r: r.update(protection="cbcs"),
                       lambda r: r.update(evidence="container-metadata"),
                       lambda r: r.update(media_verified=True), lambda r: r.update(media_verified=0),
                       lambda r: r.pop("media_verified"), lambda r: r.update(download=None),
                       lambda r: r.update(download={"retained": True})):
            with self.subTest(change=change), self.assertRaisesRegex(module.Refusal, "^metadata-proof-invalid$"):
                self.run_metadata(change)

    def test_metadata_stage_refuses_any_changed_source(self):
        with self.assertRaisesRegex(module.Refusal, "^metadata-original-changed$"):
            self.run_metadata(changed_source=True)


class AudioMediaFlowTests(unittest.TestCase):
    """Exercise producer orchestration with tools stubbed, not native-media proof."""

    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.cases = 0

    def run_case(self, mutation=None):
        self.cases += 1
        root = self.root / str(self.cases)
        root.mkdir()
        clip = (module.ROOT / module.CLIP).read_bytes()
        (root / "clip.mp4").write_bytes(clip)
        source = SimpleNamespace(read=lambda _: clip)
        fixture = EvidenceTests()
        source_inventory = {"graph_digest": "a" * 64, "private_path": "private-marker"}
        plan_id = "b" * 64
        cli = root / ("extracted-drmr.exe" if os.name == "nt" else "extracted-drmr")

        class StubRunner:
            def __init__(self):
                self.workspace = root
                self.secrets = (b"previous-secret-marker",)
                self.calls = []

            def run(self, arguments, stage):
                self.calls.append((arguments, stage))
                assert stage == "fixture-aac44100"
                directory = root / "aac44100-input"
                (directory / "media.m3u8").write_text(
                    "#EXTM3U\n#EXTINF:2,\nsegment-000.ts\n#EXTINF:2,\nsegment-001.ts\n#EXT-X-ENDLIST\n",
                    encoding="utf-8")
                (directory / "segment-000.ts").write_bytes(b"first fake segment")
                (directory / "segment-001.ts").write_bytes(b"last fake segment")
                return b""

            def json(self, arguments, stage):
                self.calls.append((arguments, stage))
                if stage == "fixture-aac44100-probe":
                    return fixture.audio_probe()
                if stage == "aac44100-analyze":
                    return {"kind": "input-analysis", "schema_version": 1, "input_admitted": True,
                            "actual_format": "hls" if mutation == "analysis-format" else "hls-ts",
                            "track_protection": ["clear"], "source": source_inventory,
                            "input_checks": {"stream_metadata": "pass", "packet_payloads_equal": True,
                                             "packet_timing_equal": True, "decoded_content_equal": True}}
                if stage == "aac44100-plan":
                    plan = {"kind": "execution-plan", "schema_version": 1, "plan_id": plan_id,
                            "source": {"graph_digest": "c" * 64} if mutation == "plan-source" else source_inventory,
                            "source_format": "hls-ts", "intent": {
                                "format": "dash", "target": "cbcs", "output": str(root / "aac44100-converted")}}
                    (root / "aac44100.plan.json").write_text(json.dumps(plan), encoding="utf-8")
                    return plan
                if stage == "aac44100-convert":
                    output = root / "aac44100-converted"
                    output.mkdir()
                    (output / "manifest.mpd").write_bytes(b"fake manifest")
                    return {"plan_id": "c" * 64 if mutation == "conversion-plan" else plan_id,
                            "input": {"format": "hls", "protection": "clear"},
                            "output": {"format": "dash", "protection": "cbcs"}, "verification": fixture.proof()}
                assert stage == "aac44100-independent-verify"
                if mutation == "changed-source":
                    (root / "aac44100-input/segment-001.ts").write_bytes(b"changed fake segment")
                if mutation == "changed-plan":
                    with (root / "aac44100.plan.json").open("ab") as output:
                        output.write(b"\n")
                return {"plan_id": plan_id, "source_unchanged": mutation != "verification-binding",
                        "output_matches_plan": True, "verification": fixture.proof()}

        runner = StubRunner()
        return module.audio_media(cli, runner, source), runner, cli

    def test_audio_route_uses_the_extracted_cli_for_all_four_saved_plan_steps(self):
        result, runner, cli = self.run_case()
        self.assertEqual([stage for _, stage in runner.calls], [
            "fixture-aac44100", "fixture-aac44100-probe", "aac44100-analyze", "aac44100-plan",
            "aac44100-convert", "aac44100-independent-verify"])
        for arguments, _ in runner.calls[2:]:
            self.assertEqual(arguments[0], cli)
        self.assertEqual(result["input_format"], "hls-ts")
        self.assertEqual(result["source_audio"]["frame_count"], 432)
        self.assertTrue(result["saved_original_and_plan_unchanged"])
        key = json.loads((runner.workspace / "aac44100-keys.json").read_text())["output"]["key"]
        self.assertIn(key.encode("ascii"), runner.secrets)
        self.assertIn(b"previous-secret-marker", runner.secrets)
        shown = json.dumps(result)
        for private in (str(runner.workspace), key, "private-marker", "previous-secret-marker"):
            self.assertNotIn(private, shown)

    def test_audio_route_refuses_mismatched_analysis_plan_or_fresh_verification(self):
        for mutation, code in (
                ("analysis-format", "aac44100-analysis-proof-invalid"),
                ("plan-source", "aac44100-plan-proof-invalid"),
                ("conversion-plan", "aac44100-conversion-proof-invalid"),
                ("verification-binding", "aac44100-saved-plan-proof-invalid"),
                ("changed-source", "aac44100-saved-original-or-plan-changed"),
                ("changed-plan", "aac44100-saved-original-or-plan-changed")):
            with self.subTest(mutation=mutation), self.assertRaisesRegex(module.Refusal, "^" + code + "$"):
                self.run_case(mutation)

    def test_audio_packet_probe_reads_all_original_transport_bytes_in_playlist_order(self):
        _, runner, _ = self.run_case()
        arguments, stage = runner.calls[1]
        self.assertEqual(stage, "fixture-aac44100-probe")
        probed = Path(arguments[-1])
        self.assertEqual(probed.suffix, ".ts")
        self.assertEqual(probed.read_bytes(), b"first fake segmentlast fake segment")
        original = runner.workspace / "aac44100-input"
        self.assertEqual((original / "segment-000.ts").read_bytes(), b"first fake segment")
        self.assertEqual((original / "segment-001.ts").read_bytes(), b"last fake segment")


class OwnedFilesTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)

    def cli_zip(self, compression, corrupt=False):
        payload = bytearray(64)
        payload[:7] = b"\x7fELF\x02\x01\x01"
        payload[16:20] = b"\x02\x00\x3e\x00"
        archive = self.root / ("private-marker-" + str(compression) + "-" + str(corrupt) + ".zip")
        with zipfile.ZipFile(archive, "w") as output:
            member = zipfile.ZipInfo("package/drmr")
            member.create_system = 3
            member.external_attr = (stat.S_IFREG | 0o755) << 16
            output.writestr(member, payload, compress_type=compression)
        if corrupt:
            damaged = bytearray(archive.read_bytes())
            payload_at = 30 + int.from_bytes(damaged[26:28], "little") + int.from_bytes(damaged[28:30], "little")
            self.assertEqual(int.from_bytes(damaged[8:10], "little"), zipfile.ZIP_LZMA)
            self.assertEqual(damaged[payload_at + 2:payload_at + 4], b"\x05\x00")
            damaged[payload_at + 4] = 0xff
            archive.write_bytes(damaged)
        return archive, payload

    def test_stored_and_deflated_zip_binaries_are_extracted_with_the_verified_hash(self):
        for compression in (zipfile.ZIP_STORED, zipfile.ZIP_DEFLATED):
            archive, payload = self.cli_zip(compression)
            destination = self.root / ("drmr-" + str(compression))
            with self.subTest(compression=compression):
                module.extract_cli(archive, "x86_64-unknown-linux-gnu", module.VERIFY.sha(payload), destination)
                self.assertEqual(destination.read_bytes(), payload)

    def test_valid_and_corrupt_lzma_zip_members_have_only_a_fixed_diagnostic(self):
        for corrupt in (False, True):
            archive, payload = self.cli_zip(zipfile.ZIP_LZMA, corrupt)
            destination = self.root / ("drmr-" + str(corrupt))
            with self.subTest(corrupt=corrupt):
                with self.assertRaisesRegex(module.Refusal, "^archive-extraction-invalid$") as denied:
                    module.extract_cli(archive, "x86_64-unknown-linux-gnu", module.VERIFY.sha(payload), destination)
                self.assertNotIn("private-marker", str(denied.exception))
                self.assertFalse(destination.exists())

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

    def test_archive_dependency_probe_requests_complete_decoding(self):
        archive = self.root / "candidate.tar.gz"
        archive.write_bytes(b"candidate")
        cli = self.root / "verified-drmr"
        proof = {"success": True, "binaries": {"drmr": {"sha256": "a" * 64}}}
        source = SimpleNamespace(head="b" * 40, tree="c" * 40)
        with patch.object(module, "require_native"), patch.object(module, "require_version"), \
                patch.object(module.VERIFY, "Source", return_value=source), \
                patch.object(module.VERIFY, "metadata", return_value={}), \
                patch.object(module.VERIFY, "build_expectation", return_value={}), \
                patch.object(module.VERIFY, "qualify", return_value=proof), \
                patch.object(module, "extract_cli", return_value=cli), \
                patch.object(module, "Runner") as execution:
            execution.return_value.run.return_value = b"drmr 1.0.0\n"
            execution.return_value.json.side_effect = module.Refusal("probe-reached")
            with self.assertRaisesRegex(module.Refusal, "^probe-reached$"):
                module.qualify(archive, "x86_64-unknown-linux-gnu", "1.0.0", self.root)
            arguments, stage, timeout = execution.return_value.json.call_args.args
            self.assertEqual(arguments[0], cli)
            self.assertEqual(arguments[1:3], ["doctor", "--probe"])
            self.assertIn("--decode", arguments)
            self.assertEqual(arguments[arguments.index("--route") + 1], "all")
            self.assertIn("--json", arguments)
            self.assertEqual((stage, timeout), ("doctor", 120))

    def test_corrupt_compression_has_only_a_fixed_diagnostic(self):
        archive = self.root / "malformed-private-marker.tar.gz"
        archive.write_bytes(b"not a compressed archive")
        with self.assertRaisesRegex(module.Refusal, "^archive-extraction-invalid$") as denied:
            module.extract_cli(archive, "x86_64-unknown-linux-gnu", "0" * 64, self.root / "drmr")
        self.assertNotIn("private-marker", str(denied.exception))

    def test_unsupported_zip_compression_has_only_a_fixed_diagnostic(self):
        payload = bytearray(64)
        payload[:7] = b"\x7fELF\x02\x01\x01"
        payload[16:20] = b"\x02\x00\x3e\x00"
        archive = self.root / "private-marker.zip"
        with zipfile.ZipFile(archive, "w", compression=zipfile.ZIP_STORED) as output:
            output.writestr("package/drmr", payload)
        damaged = bytearray(archive.read_bytes())
        damaged[8:10] = (99).to_bytes(2, "little")
        central = damaged.index(b"PK\x01\x02")
        damaged[central + 10:central + 12] = (99).to_bytes(2, "little")
        archive.write_bytes(damaged)
        destination = self.root / "drmr"
        with self.assertRaisesRegex(module.Refusal, "^archive-extraction-invalid$") as denied:
            module.extract_cli(archive, "x86_64-unknown-linux-gnu", module.VERIFY.sha(payload), destination)
        self.assertNotIn("private-marker", str(denied.exception))
        self.assertFalse(destination.exists())

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

    def test_private_workspace_and_file_permissions_are_applied_on_the_native_host(self):
        # On Windows these are genuine ordinary-user ACL operations, including
        # the helper's owner, protected-DACL, and exact-access-rule readback.
        workspace = module.Workspace(self.root)
        try:
            private = workspace.path / "private.json"
            module.private_write(private, b'{"private":"test-value"}')
            self.assertEqual(private.read_bytes(), b'{"private":"test-value"}')
            if os.name != "nt":
                self.assertEqual(workspace.path.stat().st_mode & 0o777, 0o700)
                self.assertEqual(private.stat().st_mode & 0o777, 0o600)
        finally:
            workspace.close()
        self.assertFalse(workspace.path.exists())

    def test_acl_failure_removes_only_the_newly_owned_empty_workspace(self):
        with patch.object(module, "secure_windows", side_effect=module.Refusal("private-acl-unavailable")):
            with self.assertRaisesRegex(module.Refusal, "^private-acl-unavailable$"):
                module.Workspace(self.root)
        self.assertEqual(list(self.root.iterdir()), [])

    def test_acl_failure_prevents_any_private_payload_write(self):
        private = self.root / "private.json"

        def unavailable(path, directory):
            self.assertEqual((path, directory), (private, False))
            self.assertEqual(path.read_bytes(), b"")
            raise module.Refusal("private-acl-unavailable")

        with patch.object(module, "secure_windows", side_effect=unavailable):
            with self.assertRaisesRegex(module.Refusal, "^private-acl-unavailable$"):
                module.private_write(private, b"private-marker")
        self.assertEqual(private.read_bytes(), b"")

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
        nested = self.root / "nested"
        nested.mkdir()
        module.planned_intent({**intent, "output": str(nested / ".." / "converted")}, output)
        for other in (self.root / "other", nested / "converted", self.root / "missing" / "converted",
                      Path("converted")):
            with self.subTest(other=other), self.assertRaisesRegex(module.Refusal, "^plan-proof-invalid$"):
                module.planned_intent({**intent, "output": str(other)}, output)

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
        native_popen = module.subprocess.Popen

        def completed_child(*arguments, **options):
            child = native_popen(*arguments, **options)
            try:
                self.assertEqual(child.wait(timeout=3), 0)
            except BaseException:
                child.kill()
                child.wait(timeout=5)
                raise
            return child

        # The real child has finished before Runner observes its spools. A short
        # print command alone races the live-child forced-cleanup path on Windows.
        with patch.object(module, "MAX_OUTPUT", 32), \
                patch.object(module.subprocess, "Popen", side_effect=completed_child):
            with self.assertRaisesRegex(module.Refusal, "^command-output-limit$"):
                self.runner.run([sys.executable, "-c", "print('x'*200)"], "version", timeout=3)
        self.assertEqual(self.runner.stages, [])

    def test_live_output_limit_preserves_unconfirmed_forced_cleanup(self):
        expected = "^command-cleanup-unconfirmed$" if os.name == "nt" else "^command-output-limit$"
        script = "import os,time; os.write(1,b'x'*200); time.sleep(10)"
        with patch.object(module, "MAX_OUTPUT", 32):
            with self.assertRaisesRegex(module.Refusal, expected) as denied:
                self.runner.run([sys.executable, "-c", script], "version", timeout=3)
        self.assertEqual(denied.exception.stage, "version")
        self.assertEqual(self.runner.stages, [])

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

    def test_failed_report_acl_cannot_publish_a_successful_receipt(self):
        report = self.root / "report.json"
        stdout = io.StringIO()
        with patch.object(module, "qualify", return_value={"success": True, "private": "private-marker"}), \
                patch.object(module, "secure_windows", side_effect=module.Refusal("private-acl-unavailable")), \
                patch.object(sys, "stdout", stdout):
            self.assertEqual(module.main(["--archive", "unused", "--target", "x86_64-unknown-linux-gnu",
                                          "--expected-version", "1.0.0", "--report", str(report)]), 1)
        self.assertEqual(report.read_bytes(), b"")
        self.assertEqual(json.loads(stdout.getvalue()),
                         {"schema_version": 1, "success": False, "failure_code": "report-unavailable"})
        self.assertNotIn("private-marker", stdout.getvalue())


class WindowsAclTransportTests(unittest.TestCase):
    def test_private_paths_cross_the_powershell_boundary_only_as_environment_values(self):
        private = Path("private-marker & 'quote' $value; [file]")
        # Replace this module's OS view rather than mutating os.name globally;
        # pathlib must keep using the actual host's path implementation.
        windows = SimpleNamespace(name="nt", environ={"BASE_ENVIRONMENT": "preserved"})
        for directory in (False, True):
            with self.subTest(directory=directory), patch.object(module, "os", windows), \
                    patch.object(module.subprocess, "run", return_value=SimpleNamespace(returncode=0)) as run:
                module.secure_windows(private, directory)
            arguments = run.call_args.args[0]
            self.assertEqual(arguments[:5], ["powershell.exe", "-NoLogo", "-NoProfile", "-NonInteractive",
                                             "-EncodedCommand"])
            self.assertEqual(len(arguments), 6)
            script = base64.b64decode(arguments[-1], validate=True).decode("utf-16le")
            self.assertNotIn("private-marker", script)
            self.assertEqual(run.call_args.kwargs["env"],
                             {"BASE_ENVIRONMENT": "preserved", "DRMR_QUALIFY_PATH": str(private),
                              "DRMR_QUALIFY_DIRECTORY": "1" if directory else "0"})
            self.assertIs(run.call_args.kwargs["stdin"], subprocess.DEVNULL)
            self.assertIs(run.call_args.kwargs["stdout"], subprocess.DEVNULL)
            self.assertIs(run.call_args.kwargs["stderr"], subprocess.DEVNULL)
            self.assertEqual(run.call_args.kwargs["timeout"], 20)

    def test_acl_transport_failures_have_only_fixed_diagnostics(self):
        private = Path("private-marker")
        windows = SimpleNamespace(name="nt", environ={})
        failures = (SimpleNamespace(returncode=1), OSError("private-marker"),
                    subprocess.TimeoutExpired("private-marker", 20, output=b"private-marker"))
        for failure in failures:
            response = {"side_effect": failure} if isinstance(failure, Exception) else {"return_value": failure}
            with self.subTest(failure=type(failure).__name__), patch.object(module, "os", windows), \
                    patch.object(module.subprocess, "run", **response):
                with self.assertRaisesRegex(module.Refusal, "^private-acl-unavailable$"):
                    module.secure_windows(private, True)


if __name__ == "__main__":
    unittest.main()
