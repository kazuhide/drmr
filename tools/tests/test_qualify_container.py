"""Container report controls do not substitute for real image qualification."""
import copy
import importlib.util
import http.server
import json
from pathlib import Path
import sys
import tempfile
import threading
import unittest
from unittest.mock import patch

SPEC = importlib.util.spec_from_file_location("qualify_container", Path(__file__).resolve().parents[1] / "qualify_container.py")
module = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(module)


class EvidenceTests(unittest.TestCase):
    def valid(self):
        return {"passed": True, "media_verified": True, "license_playback_verified": False,
                "checks": {"stream_metadata": "pass", "packet_payloads_equal": True,
                           "packet_timing_equal": True, "decoded_content_equal": True}}

    def test_every_proof_and_the_explicit_license_boundary_are_required(self):
        module.require_verification(self.valid())
        for field in ("passed", "media_verified", "license_playback_verified"):
            altered = self.valid()
            altered[field] = not altered[field]
            with self.subTest(field=field), self.assertRaisesRegex(module.Refusal, "^media-proof-invalid$"):
                module.require_verification(altered)
        for field in self.valid()["checks"]:
            for value in (False, None, "pass" if field != "stream_metadata" else True):
                altered = self.valid()
                altered["checks"][field] = value
                with self.subTest(field=field, value=value), self.assertRaises(module.Refusal):
                    module.require_verification(altered)

    def test_no_partial_or_truthy_report_is_accepted(self):
        for altered in ({}, [], True, {"media_verified": True}, {**self.valid(), "passed": 1}):
            with self.subTest(value=altered), self.assertRaises(module.Refusal):
                module.require_verification(altered)

    def test_duplicate_json_members_are_refused_before_evidence_parsing(self):
        for raw in ('{"passed":false,"passed":true}', '{"checks":{"a":1,"a":2}}', '{"value":NaN}'):
            with self.subTest(raw=raw), self.assertRaisesRegex(module.Refusal, "^json-invalid$"):
                module.load_json(raw)

    def test_packet_oracle_checks_every_record_and_never_trims_the_last_one(self):
        evidence = {"streams": [{"codec_name": "h264", "time_base": "1/12288", "nb_read_frames": "240"}],
                    "packets": [{"pts": i * 512, "dts": i * 512, "duration": 512,
                                 "data_hash": "SHA256:" + format(i, "064x")} for i in range(240)]}
        module.require_video(evidence)
        for change in (lambda e: e["packets"].pop(),
                       lambda e: e["packets"][-1].pop("duration"),
                       lambda e: e["streams"][0].update(nb_read_frames="239")):
            altered = copy.deepcopy(evidence)
            change(altered)
            with self.assertRaisesRegex(module.Refusal, "^video-proof-invalid$"):
                module.require_video(altered)

    def test_architecture_qualification_refuses_emulation(self):
        module.require_native("aarch64", "arm64")
        module.require_native("x86_64", "amd64")
        for daemon, image in (("arm64", "amd64"), ("amd64", "arm64"), ("unknown", "amd64")):
            with self.assertRaisesRegex(module.Refusal, "^native-architecture-required$"):
                module.require_native(daemon, image)

    def test_stop_requires_in_namespace_immediate_evidence_not_just_exit130(self):
        report = {"success": True, "driver_still_alive": True, "independent_tool_group": True,
                  "immediate_lock_released": True, "tool_absent": True, "descendant_absent": True,
                  "kind": "sigterm-cli-return", "exit_code": 130, "elapsed_milliseconds": 20}
        module.require_stop(report)
        for field in ("driver_still_alive", "independent_tool_group", "immediate_lock_released",
                      "tool_absent", "descendant_absent"):
            altered = dict(report)
            altered[field] = False
            with self.subTest(field=field), self.assertRaisesRegex(module.Refusal, "^stop-proof-invalid$"):
                module.require_stop(altered)
        for elapsed in (5000, None, True):
            with self.subTest(elapsed=elapsed), self.assertRaises(module.Refusal):
                module.require_stop({**report, "elapsed_milliseconds": elapsed})

    def test_refusals_keep_the_exact_domain_code_and_failure_schema(self):
        report = {"schema_version": 1, "command": "convert", "status": "failed",
                  "error": {"code": "key-file-invalid", "hint": "Check protected configuration.", "last_activity": "configuration"}}
        module.require_failure(2, json.dumps(report), "key-file-invalid", 2)
        for status, altered in ((0, report), (2, {**report, "status": "succeeded"}),
                                (2, {**report, "error": {**report["error"], "code": "invalid-input-media"}})):
            with self.subTest(status=status, report=altered), self.assertRaises(module.Refusal):
                module.require_failure(status, json.dumps(altered), "key-file-invalid", 2)

    def test_a_cleanup_failure_cannot_leave_a_successful_receipt(self):
        class Owner:
            identifier = "sha256:" + "0" * 64
            architecture = "arm64"
            last_failure = None

            def __init__(self, *_):
                pass

            def checked(self, *_):
                return b'{"ready":true,"cleanup_status":"confirmed"}'

            def close(self):
                raise module.Refusal("owned-cleanup-unconfirmed")

        with tempfile.TemporaryDirectory() as temporary:
            report = Path(temporary) / "report.json"
            argv = ["qualify_container", "--image", "test-only-owner", "--qualifiers", temporary,
                    "--report", str(report), "--expected-revision", "0" * 40, "--expected-tree", "1" * 40]
            with patch.object(sys, "argv", argv), patch.object(module, "Image", Owner), \
                    patch.object(module, "provenance", return_value={}), \
                    patch.object(module, "media", side_effect=lambda _, root: {"source_hashes": module.hashes(root / "input")}), \
                    patch.object(module, "permissions", return_value={}), \
                    patch.object(module, "lifecycle", return_value={}), \
                    patch.object(module, "resources", return_value={}):
                self.assertEqual(module.main(), 1)
            actual = json.loads(report.read_text())
            self.assertIs(actual["success"], False)
            self.assertEqual(actual["failure_code"], "owned-cleanup-unconfirmed")

    def test_failed_container_removal_cannot_be_reported_as_confirmed_cleanup(self):
        image = object.__new__(module.Image)
        image.names = ["drmr-qualify-owned"]
        with patch.object(module, "run", return_value=(1, b"", b"")), \
                patch.object(module, "successful", return_value=b"drmr-qualify-owned\n"):
            with self.assertRaisesRegex(module.Refusal, "^owned-container-cleanup-unconfirmed$"):
                image.close()

    def host_guard_control(self, response_overrides=None):
        requests = []
        replies = {"other.invalid": (403, "untrusted-host"),
                   "user@localhost": (400, "invalid-host")}
        replies.update(response_overrides or {})

        class Handler(http.server.BaseHTTPRequestHandler):
            def do_GET(self):
                hosts = self.headers.get_all("Host")
                requests.append((self.path, hosts))
                status, error = replies.get(hosts[0], (500, "fixture-invalid-host"))
                body = json.dumps({"error": error}).encode("utf-8")
                self.send_response(status)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(body)))
                self.end_headers()
                self.wfile.write(body)

            def log_message(self, *_):
                pass

        server = http.server.HTTPServer(("127.0.0.1", 0), Handler)
        worker = threading.Thread(target=lambda: server.serve_forever(poll_interval=0.01), daemon=True)
        worker.start()
        def cleanup():
            server.shutdown()
            server.server_close()
            worker.join(timeout=2)
            self.assertFalse(worker.is_alive(), "The owned HTTP fixture must finish")
        self.addCleanup(cleanup)
        return server.server_port, requests

    def test_real_http_headers_distinguish_untrusted_and_malformed_authorities(self):
        port, requests = self.host_guard_control()
        self.assertEqual(module.verify_web_host_guards(port), [
            {"case": "untrusted-authority", "status": 403, "error": "untrusted-host"},
            {"case": "malformed-authority", "status": 400, "error": "invalid-host"}])
        self.assertEqual(requests, [
            ("/api/catalog", ["other.invalid"]),
            ("/api/catalog", ["user@localhost"])])

    def test_real_http_guard_controls_refuse_success_and_wrong_failure_classes(self):
        for host, reply in (("other.invalid", (200, "untrusted-host")),
                            ("other.invalid", (400, "invalid-host")),
                            ("user@localhost", (403, "untrusted-host")),
                            ("user@localhost", (400, "untrusted-host"))):
            with self.subTest(host=host, reply=reply):
                port, requests = self.host_guard_control({host: reply})
                with self.assertRaisesRegex(module.Refusal, "^web-host-guard-failed$"):
                    module.verify_web_host_guards(port)
                self.assertEqual(requests[-1], ("/api/catalog", [host]))

    def test_output_limit_terminates_a_live_writer_before_its_deadline(self):
        command = [sys.executable, "-c", "import os,time; os.write(1,b'x'*4096); time.sleep(10)"]
        with patch.object(module, "MAX_OUTPUT", 64), self.assertRaisesRegex(
                module.Refusal, "^command-output-limit$"):
            module.run(command, timeout=0.5)

    def test_nonzero_exit_keeps_bounded_stdout_for_the_refusal_oracle(self):
        status, stdout, stderr = module.run([
            sys.executable, "-c", "import os; os.write(1,b'refused'); os.write(2,b'fixed'); raise SystemExit(2)"])
        self.assertEqual((status, stdout, stderr), (2, b"refused", b"fixed"))

if __name__ == "__main__":
    unittest.main()
