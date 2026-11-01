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

    def test_h264_aac_matrix_alone_cannot_qualify_a_codec_aware_release(self):
        value = roundtrip_reports.complete(CONTRACT)
        value.pop("codecs", None)
        with self.assertRaises(CONTRACT.Failure):
            self.validate(value)

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
                   lambda value: value["records"][0].update(failure_detail={"code": "command-failed"}),
                   lambda value: value["fixture_profiles"][0].update(failed_operation="fixture"),
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
            # Launch-control setup must fail outside the accepted guard refusal.
            with patch.object(ROUNDTRIP.SUPPORT, "headless_windows_errors",
                              side_effect=ROUNDTRIP.SUPPORT.Refusal("command-unavailable")):
                with self.assertRaises(ROUNDTRIP.SUPPORT.Refusal):
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
            self.assertEqual(set(root.iterdir()), {original})

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
            self.assertEqual(set(root.iterdir()), {original})

    def test_failed_conversion_retains_only_allowlisted_command_evidence(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            original = root / "original"
            original.mkdir()
            source = original / "asset.mp4"
            source.write_bytes(b"source")
            executor = Mock()
            executor.cancel = threading.Event()
            executor.convert.side_effect = ROUNDTRIP.SUPPORT.Refusal(
                "command-failed", "convert", {"exit_code": 3221225794,
                "stdout_sha256": "a" * 64, "stderr_sha256": "b" * 64,
                "fixed_cli_code": "temporary-file-unavailable", "raw_stderr": "private-marker"})
            result = ROUNDTRIP.execute_case(CONTRACT.cases()[0], executor,
                                            {("video", ("mp4", "clear")): source}, root)
            self.assertEqual(result["status"], "failed")
            self.assertEqual(result["failed_operation"], "forward")
            self.assertEqual(result["failure_detail"], {"code": "command-failed", "stage": "convert",
                             "exit_code": 3221225794, "stdout_sha256": "a" * 64,
                             "stderr_sha256": "b" * 64, "fixed_cli_code": "temporary-file-unavailable"})
            self.assertNotIn("private-marker", json.dumps(result))

    def test_failure_projection_drops_private_and_malformed_values(self):
        errors = [OSError("private-marker"), CONTRACT.Failure("private-marker"),
                  ROUNDTRIP.SUPPORT.Refusal("private-marker", "private-marker", {
                      "exit_code": True, "stdout_sha256": "private-marker", "stderr_sha256": "A" * 64,
                      "fixed_cli_code": "private-marker", "path": "private-marker"}),
                  ROUNDTRIP.SUPPORT.Refusal("private-marker", ["private-marker"], {
                      "exit_code": [], "stdout_sha256": {}, "fixed_cli_code": ["private-marker"]})]
        for error in errors:
            with self.subTest(error_type=type(error).__name__):
                result = ROUNDTRIP.failure_fields(error, "roundtrip-operation-failed", "forward")
                self.assertEqual(result, {"failure_code": "roundtrip-operation-failed",
                                          "failed_operation": "forward"})
                self.assertNotIn("private-marker", json.dumps(result))
        for code in (-2147483649, 4294967296, 1.5, "private-marker"):
            error = ROUNDTRIP.SUPPORT.Refusal("command-failed", "convert", {"exit_code": code})
            result = ROUNDTRIP.failure_fields(error, "roundtrip-operation-failed", "forward")
            self.assertNotIn("exit_code", result["failure_detail"])

    def test_failed_fixture_and_qualification_preserve_fixed_stage_information(self):
        error = ROUNDTRIP.SUPPORT.Refusal("command-deadline", "fixture-aac44100")
        executor = Mock()
        executor.convert.side_effect = error
        with patch.object(CONTRACT, "LAYOUTS", ("video",)), patch.object(CONTRACT, "OUTPUTS", (("mp4", "clear"),)):
            profiles, records = ROUNDTRIP.fixture_profiles(executor, {"video": Path("source")}, Path("unused"))
        self.assertEqual(profiles, {})
        self.assertEqual(records[0]["failed_operation"], "fixture")
        self.assertEqual(records[0]["failure_detail"], {"code": "command-deadline", "stage": "fixture-aac44100"})
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            report = root / "report.json"
            with patch.object(ROUNDTRIP, "qualify", side_effect=error), contextlib.redirect_stdout(io.StringIO()):
                status = ROUNDTRIP.main(["--inputs", str(root), "--target", "aarch64-apple-darwin",
                                         "--source", str(root / "source"), "--report", str(report)])
            self.assertEqual(status, 1)
            value = json.loads(report.read_bytes())
            self.assertEqual(value["failed_operation"], "qualification")
            self.assertEqual(value["failure_detail"], {"code": "command-deadline", "stage": "fixture-aac44100"})

    def test_missing_verified_fixture_is_reported_without_executing_its_case(self):
        executor = Mock()
        result = ROUNDTRIP.execute_case(CONTRACT.cases()[0], executor, {}, Path("unused"))
        self.assertEqual(result["status"], "not-checked")
        self.assertEqual(result["failure_code"], "source-fixture-unavailable")
        executor.convert.assert_not_called()

    def test_cancelled_first_cohort_cannot_start_codec_qualification(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            archive = root / "candidate.archive"
            archive.write_bytes(b"archive")
            archive_hash = CONTRACT.sha(b"archive")
            binary_hash = CONTRACT.sha(b"binary")
            source = Mock(head="a" * 40, tree="b" * 40)
            report = {"archive_sha256": archive_hash, "binaries": {"drmr": {"sha256": binary_hash}}}
            executor = Mock(cancel=threading.Event())
            codecs = Mock(ERRORS=(RuntimeError,))
            codecs.qualify.return_value = {"success": False}
            workspace_factory = ROUNDTRIP.SUPPORT.Workspace

            def extract(_archive, _target, _digest, output):
                output.write_bytes(b"binary")
                return output

            def work(case, _executor, _profiles, _root):
                executor.cancel.wait(1)
                return {**case, "status": "not-checked"}

            error = None
            with patch.object(ROUNDTRIP.SUPPORT, "require_native"), \
                    patch.object(ROUNDTRIP.SUPPORT.VERIFY, "command", return_value=b""), \
                    patch.object(ROUNDTRIP.SUPPORT.VERIFY, "Source", return_value=source), \
                    patch.object(ROUNDTRIP, "packet", return_value=("1.0.3", archive, report)), \
                    patch.object(ROUNDTRIP.SUPPORT, "require_version"), \
                    patch.object(ROUNDTRIP.SUPPORT, "Workspace", side_effect=lambda: workspace_factory(parent=root)), \
                    patch.object(ROUNDTRIP.SUPPORT, "extract_cli", side_effect=extract), \
                    patch.object(ROUNDTRIP.SUPPORT, "Runner", return_value=Mock(run=Mock(return_value=b"drmr 1.0.3\n"))), \
                    patch.object(ROUNDTRIP.FIXTURES, "source_seeds", return_value={}), \
                    patch.object(ROUNDTRIP, "Executor", return_value=executor), \
                    patch.object(ROUNDTRIP, "fixture_profiles", return_value=({}, [])), \
                    patch.object(CONTRACT, "cases", return_value=CONTRACT.cases()[:1]), \
                    patch.object(ROUNDTRIP, "execute_case", side_effect=work), \
                    patch.object(ROUNDTRIP.concurrent.futures, "as_completed", side_effect=KeyboardInterrupt), \
                    patch.object(CONTRACT, "module", return_value=codecs) as load_codec:
                try:
                    ROUNDTRIP.qualify(root, "aarch64-apple-darwin", root, 1)
                except CONTRACT.Failure as caught:
                    error = caught
                self.assertTrue(executor.cancel.is_set())
                load_codec.assert_not_called()
                codecs.qualify.assert_not_called()
            self.assertIsInstance(error, CONTRACT.Failure)
            self.assertEqual(str(error), "roundtrip-cancelled")
            self.assertEqual(set(root.iterdir()), {archive})

    def test_partial_submission_cancels_owned_futures_before_wait_and_cleans_up(self):
        for interruption in (KeyboardInterrupt, RuntimeError):
            with self.subTest(interruption=interruption.__name__), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                archive = root / "candidate.archive"
                archive.write_bytes(b"archive")
                source = Mock(head="a" * 40, tree="b" * 40)
                report = {"archive_sha256": CONTRACT.sha(b"archive"),
                          "binaries": {"drmr": {"sha256": CONTRACT.sha(b"binary")}}}
                executor = Mock(cancel=threading.Event())
                case = CONTRACT.cases()[0]
                cases = [{**case, "id": str(index)} for index in range(3)]
                started, observed, before_wait, submitted = [], [], [], []
                ready, release = threading.Event(), threading.Event()
                pool_type = ROUNDTRIP.concurrent.futures.ThreadPoolExecutor
                real_submit, real_exit = pool_type.submit, pool_type.__exit__
                workspace_factory = ROUNDTRIP.SUPPORT.Workspace

                def work(item, *_):
                    if executor.cancel.is_set():
                        return {**item, "status": "not-checked"}
                    started.append(item["id"])
                    ready.set()
                    release.wait(2)
                    observed.append(executor.cancel.is_set())
                    return {**item, "status": "not-checked"}

                def submit(pool, *args, **kwargs):
                    future = real_submit(pool, *args, **kwargs)
                    submitted.append(future)
                    if len(submitted) == 3:
                        self.assertTrue(ready.wait(2))
                        raise interruption()
                    return future

                def finish(pool, *args):
                    before_wait.append(executor.cancel.is_set())
                    release.set()
                    return real_exit(pool, *args)

                def extract(_archive, _target, _digest, output):
                    output.write_bytes(b"binary")
                    return output

                controls = (patch.object(ROUNDTRIP.SUPPORT, "require_native"),
                            patch.object(ROUNDTRIP.SUPPORT.VERIFY, "command", return_value=b""),
                            patch.object(ROUNDTRIP.SUPPORT.VERIFY, "Source", return_value=source),
                            patch.object(ROUNDTRIP, "packet", return_value=("1.0.3", archive, report)),
                            patch.object(ROUNDTRIP.SUPPORT, "require_version"),
                            patch.object(ROUNDTRIP.SUPPORT, "Workspace", side_effect=lambda: workspace_factory(parent=root)),
                            patch.object(ROUNDTRIP.SUPPORT, "extract_cli", side_effect=extract),
                            patch.object(ROUNDTRIP.SUPPORT, "Runner", return_value=Mock(run=Mock(return_value=b"drmr 1.0.3\n"))),
                            patch.object(ROUNDTRIP.FIXTURES, "source_seeds", return_value={}),
                            patch.object(ROUNDTRIP, "Executor", return_value=executor),
                            patch.object(ROUNDTRIP, "fixture_profiles", return_value=({}, [])),
                            patch.object(CONTRACT, "cases", return_value=cases),
                            patch.object(ROUNDTRIP, "execute_case", side_effect=work),
                            patch.object(pool_type, "submit", submit), patch.object(pool_type, "__exit__", finish))
                with contextlib.ExitStack() as stack:
                    for control in controls:
                        stack.enter_context(control)
                    load_codec = stack.enter_context(patch.object(CONTRACT, "module"))
                    error = None
                    try:
                        ROUNDTRIP.qualify(root, "aarch64-apple-darwin", root, 1)
                    except BaseException as caught:
                        error = caught
                    load_codec.assert_not_called()
                self.assertEqual(before_wait, [True])
                self.assertEqual(started, ["0"])
                self.assertEqual(observed, [True])
                self.assertTrue(submitted[1].cancelled())
                self.assertTrue(all(future.done() for future in submitted))
                self.assertEqual(list(root.iterdir()), [archive])
                self.assertIsInstance(error, CONTRACT.Failure)
                self.assertEqual(str(error), "roundtrip-cancelled")

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
