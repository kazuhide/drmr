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
    def test_all_sixty_documented_cells_are_represented(self):
        records = CONTRACT.cases()
        self.assertEqual(len(records), 780)
        self.assertEqual(len({record["id"] for record in records}), 780)
        self.assertEqual({record["recipe"] for record in records},
                         {CONTRACT.EXAMPLES.identifier(*route) for route in CONTRACT.EXAMPLES.routes()})
        self.assertEqual(CONTRACT.counts(records), {"roundtrip": 648, "input-only": 54,
                                                   "forward-only": 36, "layout-condition": 42})
        self.assertTrue(all(record["source"] == ["hls-fmp4", "cenc"]
                            for record in records if record["kind"] == "input-only"))

    def validate(self, value):
        return CONTRACT.validate(value, "a" * 40, "b" * 40, "aarch64-apple-darwin", "1.0.3", "c" * 64, "d" * 64)

    def test_complete_native_matrix_passes_without_claiming_decode_or_playback(self):
        value = roundtrip_reports.complete(CONTRACT)
        self.assertIs(self.validate(value), value)
        self.assertEqual(value["decode_evidence"], "not-checked")
        self.assertFalse(value["license_playback_verified"])
        self.assertLess(len(json.dumps(value, indent=2)), ROUNDTRIP.PREPARE.MAX_ROUNDTRIP_REPORT)

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

    def test_nanosecond_format_ceil_keeps_nonintegral_aac_endpoint(self):
        self.assertEqual(ROUNDTRIP.FIXTURES.seconds(240, 44100), "0.005442177")
        self.assertEqual(ROUNDTRIP.FIXTURES.seconds(1024, 44100), "0.023219955")
        self.assertEqual(ROUNDTRIP.FIXTURES.seconds(96000, 48000), "2")

    def test_encryption_keys_keep_mp4_and_hls_signaling_separate(self):
        values = ("01" * 16, "02" * 16, "03" * 16, "skd://fixture")
        self.assertNotIn("signaling", ROUNDTRIP.keys(("mp4", "cbcs"), *values))
        self.assertEqual(ROUNDTRIP.keys(("hls-fmp4", "cbcs"), *values)["signaling"],
                         {"system": "multi-drm", "fairplay": True})
        self.assertNotIn("kid", ROUNDTRIP.keys(("hls-ts", "hls-sample-aes"), *values))
        self.assertEqual(ROUNDTRIP.keys(("dash", "cenc"), *values)["signaling"]["system"], "multi-drm")

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
            runner = executor.runner(operation)
            for tool in ("ffmpeg", "ffprobe"):
                with self.subTest(tool=tool), self.assertRaises(ROUNDTRIP.SUPPORT.Refusal):
                    runner.run([tool, "-version"], "decoder-must-not-run", 5)
            self.assertEqual(runner.stages, [])

    def test_input_only_hls_fixture_preserves_cenc_bytes_and_rational_durations(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / "dash"
            media = source / "audio"
            media.mkdir(parents=True)
            for name, data in (("init.mp4", b"canonical-init"), ("seg_1.m4s", b"coded-full"),
                               ("seg_2.m4s", b"coded-partial-presentation")):
                (media / name).write_bytes(data)
            mpd = source / "manifest.mpd"
            mpd.write_text('<MPD xmlns="urn:mpeg:dash:schema:mpd:2011"><Period><AdaptationSet contentType="audio">'
                           '<Representation id="1"><SegmentTemplate timescale="44100" startNumber="1" '
                           'initialization="audio/init.mp4" media="audio/seg_$Number$.m4s"><SegmentTimeline>'
                           '<S t="0" d="44100"/><S d="240"/></SegmentTimeline></SegmentTemplate>'
                           '</Representation></AdaptationSet></Period></MPD>', encoding="utf-8")
            destination = root / "hls"
            path = ROUNDTRIP.FIXTURES.cenc_hls(mpd, destination, "skd://test-fixture", ROUNDTRIP.SUPPORT)
            text = (destination / "audio.m3u8").read_text()
            self.assertIn("#EXTINF:0.005442177,", text)
            self.assertIn("#EXT-X-TARGETDURATION:1", text)
            self.assertEqual((destination / "audio/seg_2.m4s").read_bytes(), (media / "seg_2.m4s").read_bytes())
            self.assertEqual((destination / "audio/init.mp4").read_bytes(), (media / "init.mp4").read_bytes())
            self.assertTrue(path.is_file())
            self.assertFalse((destination / "manifest.mpd").exists())

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
