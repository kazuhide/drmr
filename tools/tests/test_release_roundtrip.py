"""Release-only matrix coverage, native evidence, privacy and cancellation gates."""
import copy
import contextlib
import io
import json
from pathlib import Path
import sys
import tempfile
import threading
import time
import unittest
from unittest.mock import Mock, patch

TOOLS = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(TOOLS))
sys.path.insert(0, str(TOOLS / "tests"))
import release_roundtrip as ROUNDTRIP
import roundtrip_reports

CONTRACT = ROUNDTRIP.CONTRACT


class ReleaseMatrix(unittest.TestCase):
    def test_all_sixty_five_documented_cells_are_represented(self):
        records = CONTRACT.cases()
        self.assertEqual(len(records), 845)
        self.assertEqual(len({record["id"] for record in records}), 845)
        self.assertEqual({record["recipe"] for record in records},
                         {CONTRACT.EXAMPLES.identifier(*route) for route in CONTRACT.EXAMPLES.routes()})
        self.assertEqual(CONTRACT.counts(records), {"roundtrip": 761, "forward-only": 42, "layout-condition": 42})

    def validate(self, value):
        return CONTRACT.validate(value, "a" * 40, "b" * 40, "aarch64-apple-darwin", "1.0.3", "c" * 64, "d" * 64)

    def test_complete_native_matrix_passes_without_claiming_decode_or_playback(self):
        value = roundtrip_reports.complete(CONTRACT)
        self.assertIs(self.validate(value), value)
        self.assertEqual(value["decode_evidence"], "not-checked")
        self.assertFalse(value["license_playback_verified"])
        self.assertLess(len(json.dumps(value, indent=2)), ROUNDTRIP.PREPARE.MAX_ROUNDTRIP_REPORT)

    def test_tuple_writer_and_list_case_profiles_keep_the_same_actual_providers(self):
        for profile in CONTRACT.OUTPUTS:
            expected = roundtrip_reports.providers(profile)
            with self.subTest(profile=profile):
                self.assertEqual(CONTRACT.drms(profile), expected)
                self.assertEqual(CONTRACT.drms(list(profile)), expected)
        value = roundtrip_reports.complete(CONTRACT)
        for record in value["records"]:
            self.assertIsInstance(record["target"], list)
            if record["kind"] != "layout-condition":
                self.assertEqual(record["forward"]["drms"], roundtrip_reports.providers(record["target"]))
            if record["kind"] == "roundtrip":
                self.assertEqual(record["backward"]["drms"], roundtrip_reports.providers(record["source"]))
        self.assertIs(self.validate(value), value)

    def test_missing_duplicate_or_reordered_records_fail_closed(self):
        original = roundtrip_reports.complete(CONTRACT)
        for change in (lambda value: value["records"].pop(),
                       lambda value: value["records"].append(value["records"][0]),
                       lambda value: value["records"].reverse(),
                       lambda value: value["records"].__setitem__(1, value["records"][0]),
                       lambda value: value["fixture_profiles"].pop()):
            value = copy.deepcopy(original)
            change(value)
            with self.assertRaises(CONTRACT.Failure):
                self.validate(value)

    def test_archive_source_fixture_header_and_matrix_bindings_are_required(self):
        original = roundtrip_reports.complete(CONTRACT)
        fields = ("source_head", "source_tree", "target", "version", "archive_sha256", "binary_sha256",
                  "binary_header_target", "binary_reported_version", "fixture_sha256", "matrix_sha256")
        for field in fields:
            value = copy.deepcopy(original)
            value[field] = "mismatch"
            with self.subTest(field=field), self.assertRaises(CONTRACT.Failure):
                self.validate(value)

    def test_partial_proof_unknown_field_or_weakened_status_is_rejected(self):
        original = roundtrip_reports.complete(CONTRACT)
        changes = (lambda value: value["records"][0].update(status="not-checked"),
                   lambda value: value["records"][0].pop("final_against_original"),
                   lambda value: value["records"][0]["forward"].update(drms=["fairplay"]),
                   lambda value: value["records"][0]["forward"]["conversion"]["checks"].update(packet_timing_equal=False),
                   lambda value: value["records"][0]["final_against_original"].update(license_playback_verified=True),
                   lambda value: value["records"][0].update(private_key="synthetic-private-value"),
                   lambda value: value["counts"].update(not_checked=1),
                   lambda value: value["counts"].update(failed=False),
                   lambda value: value["fixture_profiles"][13].update(sample_rate=48000),
                   lambda value: value["fixture_profiles"][13].update(final_presented_samples=1024))
        for change in changes:
            value = copy.deepcopy(original)
            change(value)
            with self.assertRaises(CONTRACT.Failure):
                self.validate(value)

    def test_unshared_selection_metadata_cannot_qualify(self):
        for field, wrong in (("group", "audio"), ("name", "Main"), ("autoselect", False)):
            value = roundtrip_reports.complete(CONTRACT)
            value["fixture_metadata"]["layouts"]["video-aac44100"]["audio_rendition"][field] = wrong
            with self.subTest(field=field), self.assertRaises(CONTRACT.Failure):
                self.validate(value)

    def test_encryption_keys_keep_mp4_and_hls_signaling_separate(self):
        values = ("01" * 16, "02" * 16, "03" * 16, "skd://fixture")
        self.assertNotIn("signaling", ROUNDTRIP.keys(("mp4", "cbcs"), *values))
        self.assertTrue(ROUNDTRIP.keys(("hls-fmp4", "cbcs"), *values)["signaling"]["fairplay"])
        self.assertIn("widevine_pssh", ROUNDTRIP.keys(("hls-fmp4", "cbcs"), *values)["signaling"])
        self.assertNotIn("hls_key_uri", ROUNDTRIP.keys(("hls-fmp4", "cenc"), *values))
        self.assertNotIn("kid", ROUNDTRIP.keys(("hls-ts", "hls-sample-aes"), *values))
        self.assertEqual(ROUNDTRIP.keys(("dash", "cenc"), *values)["signaling"]["system"], "multi-drm")
        self.assertEqual(ROUNDTRIP.inspection_protection(("hls-ts", "hls-sample-aes")), "unknown")
        self.assertEqual(ROUNDTRIP.inspection_protection(("hls-fmp4", "hls-aes128")), "hls-aes128")

    def test_fixture_generation_is_the_only_ffmpeg_path(self):
        implementation = (TOOLS / "release_roundtrip.py").read_text(encoding="utf-8")
        self.assertNotIn('"--decode"', implementation)
        self.assertNotIn('"cargo"', implementation)

    def test_conversion_runner_cannot_execute_external_decoders(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            with patch.object(ROUNDTRIP.shutil, "which", return_value=str(root / "unused-packager")):
                executor = ROUNDTRIP.Executor(root / "unused-cli", root)
            operation = root / "operation"
            operation.mkdir()
            check = operation / "check"
            check.mkdir()
            executor.check_decoder_guards(check)

    def test_partial_audio_duration_preserves_sample_derived_f64(self):
        for rate in (44100, 48000):
            text = ROUNDTRIP.FIXTURES.partial_audio_duration(rate)
            self.assertEqual(float(text).hex(), (240 / rate).hex())
            self.assertGreater(len(text.split(".")[1]), 9)
        self.assertNotEqual(float(format(240 / 44100, ".17f")).hex(), (240 / 44100).hex())

    def test_hls_three_drm_and_ctr_declarations_are_complete(self):
        identifiers = {"fairplay": "com.apple.streamingkeydelivery", "playready": "com.microsoft.playready",
                       "widevine": "urn:uuid:edef8ba9-79d6-4ace-a3c8-27dcd51d21ed"}
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "media.m3u8"
            for protection in ("cenc", "cbcs"):
                expected = CONTRACT.drms(("hls-fmp4", protection))
                method = "SAMPLE-AES-CTR" if protection == "cenc" else "SAMPLE-AES"
                lines = ["#EXTM3U", "#EXT-X-VERSION:7", "#EXT-X-TARGETDURATION:2"]
                keys = [f'#EXT-X-KEY:METHOD={method},URI="data:text/plain;base64,AA==",KEYFORMAT="{identifiers[drm]}",KEYFORMATVERSIONS="1"'
                        for drm in expected]
                tail = ['#EXT-X-MAP:URI="init.mp4"', "#EXTINF:2,", "segment.m4s", "#EXT-X-ENDLIST"]
                path.write_text("\n".join(lines + keys + tail), encoding="utf-8")
                self.assertEqual(ROUNDTRIP.manifest_profile(path, ("hls-fmp4", protection)), expected)
                for invalid in (keys[:-1], keys + [keys[0]], [key + ",IV=0x1" for key in keys]):
                    path.write_text("\n".join(lines + invalid + tail), encoding="utf-8")
                    with self.assertRaises(CONTRACT.Failure):
                        ROUNDTRIP.manifest_profile(path, ("hls-fmp4", protection))

    def test_each_cycle_compares_final_output_directly_with_original(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            original = root / "original"
            original.mkdir()
            source = original / "asset.mp4"
            source.write_bytes(b"source")
            case = CONTRACT.cases()[0]
            executor = Mock()
            executor.cancel = threading.Event()

            def convert(input_path, profile, layout, output_root):
                output_root.mkdir()
                path = output_root / "asset.mp4"
                path.write_bytes(b"converted")
                return path, {"proof": "unit-test"}

            executor.convert.side_effect = convert
            executor.verify.return_value = roundtrip_reports.native("mp4")
            result = ROUNDTRIP.execute_case(case, executor, {("video", ("mp4", "clear")): source}, root)
            self.assertEqual(result["status"], "complete")
            self.assertEqual(executor.convert.call_count, 2)
            self.assertEqual(executor.verify.call_args.args[1], source)
            self.assertEqual(executor.verify.call_args.args[-1], "final-against-original")
            self.assertEqual(list(root.glob("drmr-cli-archive-*")), [])

    def test_failed_operation_cannot_become_a_completed_case(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            original = root / "original"
            original.mkdir()
            source = original / "asset.mp4"
            source.write_bytes(b"source")
            executor = Mock()
            executor.cancel = threading.Event()
            executor.convert.side_effect = ROUNDTRIP.SUPPORT.Refusal("command-failed")
            result = ROUNDTRIP.execute_case(CONTRACT.cases()[0], executor,
                                            {("video", ("mp4", "clear")): source}, root)
            self.assertEqual(result["status"], "failed")
            self.assertNotIn("forward", result)
            self.assertEqual(list(root.glob("drmr-cli-archive-*")), [])

    def test_missing_verified_fixture_is_reported_without_executing_its_case(self):
        executor = Mock()
        result = ROUNDTRIP.execute_case(CONTRACT.cases()[0], executor, {}, Path("unused"))
        self.assertEqual(result["status"], "not-checked")
        self.assertEqual(result["failure_code"], "source-fixture-unavailable")
        executor.convert.assert_not_called()

    def test_existing_report_is_preserved_and_private_argument_is_not_logged(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            report = root / "private-marker.json"
            report.write_text("preserve", encoding="utf-8")
            output = io.StringIO()
            with patch.object(ROUNDTRIP, "qualify") as qualify, contextlib.redirect_stdout(output):
                status = ROUNDTRIP.main(["--inputs", str(root / "private-input-marker"), "--target", "aarch64-apple-darwin",
                                         "--report", str(report)])
            self.assertEqual(status, 1)
            self.assertEqual(report.read_text(), "preserve")
            self.assertNotIn("private-", output.getvalue())
            qualify.assert_not_called()

    def test_runner_cancellation_stops_owned_process_and_does_not_complete_stage(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            cancel = threading.Event()
            runner = ROUNDTRIP.SUPPORT.Runner(root, cancel_event=cancel)
            ready, reaped = root / "ready", root / "reaped"
            child = root / "child.py"
            child.write_text("import pathlib,signal,subprocess,sys,time\n"
                             "child=subprocess.Popen([sys.executable,'-c','import time;time.sleep(60)'])\n"
                             "def stop(*_):\n child.terminate();child.wait(timeout=2);pathlib.Path(sys.argv[2]).write_text('reaped');sys.exit(0)\n"
                             "signal.signal(signal.SIGTERM,stop)\npathlib.Path(sys.argv[1]).write_text('ready')\ntime.sleep(60)\n",
                             encoding="utf-8")
            errors = []

            def run():
                try:
                    runner.run([sys.executable, child, ready, reaped], "cancel-test", 15)
                except ROUNDTRIP.SUPPORT.Refusal as error:
                    errors.append(error)

            thread = threading.Thread(target=run)
            thread.start()
            deadline = time.monotonic() + 5
            while not ready.exists() and time.monotonic() < deadline:
                time.sleep(0.01)
            self.assertTrue(ready.exists())
            cancel.set()
            thread.join(8)
            self.assertFalse(thread.is_alive())
            self.assertEqual(runner.stages, [])
            self.assertEqual(len(errors), 1)
            if sys.platform != "win32":
                self.assertTrue(reaped.exists())
                self.assertEqual(str(errors[0]), "command-cancelled")


if __name__ == "__main__":
    unittest.main()
