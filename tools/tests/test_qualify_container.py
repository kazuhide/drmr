"""Container report controls do not substitute for real image qualification."""
import copy
import hashlib
import importlib.util
import http.server
import json
import subprocess
from pathlib import Path
import sys
import tempfile
import threading
import unittest
from unittest.mock import Mock, patch

SPEC = importlib.util.spec_from_file_location("qualify_container", Path(__file__).resolve().parents[1] / "qualify_container.py")
module = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(module)


class ResourceProgressTests(unittest.TestCase):
    """Exercise the real resource gate with the public progress producer shape."""

    def records(self):
        base = {"schema_version": 1, "entry_point": "cli", "command": "convert", "run_id": "123-456",
                "event": "activity", "elapsed_ms": 0, "activity": "configuration",
                "task": "Checking command settings", "scopes_started": 0, "scopes_closed": 0,
                "active_scopes": 0, "tools_started": 0, "tools_closed": 0,
                "active_tool_count": 0, "active_tools": [], "work_units_completed": 0}
        return [base, {**base, "event": "progress", "elapsed_ms": 1, "workers": 2},
                {**base, "event": "finished", "elapsed_ms": 2, "activity": "publication", "workers": 2}]

    def encode(self, records):
        return b"".join((json.dumps(record) + "\n").encode() for record in records)

    def run_gate(self, stderr=None, status=0, stdout=None, verification=None):
        checked, invoked = [], []
        observed = {"kind": "native-hardware-observation", "memory_source": "process-cgroup",
                    "effective_cpus": 2, "available_memory_bytes": 512 * 1024 ** 2}
        verified = EvidenceTests().valid() if verification is None else verification
        stderr = self.encode(self.records()) if stderr is None else stderr
        stdout = b"Verified stream metadata, packet payloads, decoded content, and packet timing.\n" if stdout is None else stdout

        class Image:
            def checked(self, arguments, **options):
                checked.append((arguments, options))
                return json.dumps(observed if not arguments else verified).encode()

            def invoke(self, arguments, **options):
                invoked.append((arguments, options))
                return status, stdout, stderr

        return module.resources(Image()), checked, invoked

    def test_resource_gate_accepts_numeric_progress_without_legacy_human_worker_text(self):
        raw = self.encode(self.records())
        self.assertNotIn(b"Using up to", raw)
        result, checked, invoked = self.run_gate(raw)
        self.assertEqual(result["actual_cli_workers"], [2, 2])
        self.assertEqual(result["limits"], {"memory_bytes": 1024 ** 3, "cpus": 2})
        self.assertEqual(invoked, [(["convert", "/input/dash/manifest.mpd", "--to", "clear", "--format", "dash",
                                    "--output", "/work/resource-limited", "--progress", "json"], {"limited": True})])
        self.assertEqual(checked, [([], {"entry": "/qualifiers/container-resources", "limited": True}),
                                  (["verify", "/work/resource-limited/manifest.mpd", "--source",
                                    "/input/dash/manifest.mpd", "--json"], {"limited": True})])
        self.assertIs(result["independent_verification"]["passed"], True)
        rows = self.records()
        for row in rows:
            if "workers" in row:
                row["workers"] = 1
        result, _, _ = self.run_gate(self.encode(rows))
        self.assertEqual(result["actual_cli_workers"], [1, 1])

    def test_resource_gate_refuses_missing_malformed_or_unbound_progress(self):
        changes = [lambda rows: rows[1].update(workers=0), lambda rows: rows[1].update(workers=3),
                   lambda rows: rows[1].update(workers=True), lambda rows: rows[1].update(workers="2"),
                   lambda rows: rows[1].update(workers=2.0), lambda rows: rows[1].update(command="analyze"),
                   lambda rows: rows[1].update(schema_version=True), lambda rows: rows[1].update(schema_version=2),
                   lambda rows: rows[1].update(entry_point="web"), lambda rows: rows[1].update(run_id="other-run"),
                   lambda rows: rows[1].update(run_id="123-789"),
                   lambda rows: rows[1].update(event="failed"), lambda rows: rows[1].update(elapsed_ms=-1),
                   lambda rows: rows[1].update(elapsed_ms=True), lambda rows: rows[1].update(active_tools="ffprobe"),
                   lambda rows: rows[1].pop("activity"), lambda rows: rows[1].update(activity=[]),
                   lambda rows: rows[1].update(private_path="private-marker"),
                   lambda rows: rows[-1].update(event="progress"), lambda rows: rows[-1].pop("workers"),
                   lambda rows: rows[1].update(scopes_started=True),
                   lambda rows: rows.append(rows[-1].copy())]
        for change in changes:
            rows = self.records()
            change(rows)
            with self.subTest(change=change):
                with self.assertRaisesRegex(module.Refusal, "^container-resource-progress-invalid$") as refused:
                    self.run_gate(self.encode(rows))
                self.assertEqual(refused.exception.evidence["exit_code"], 0)
                self.assertEqual(set(refused.exception.evidence), {"exit_code", "stdout_sha256", "stderr_sha256"})
        for raw in (b"", b"not JSON\n", b"[]\n", b"\xff\n", b'{"schema_version":1,"schema_version":1}\n',
                    self.encode(self.records()) + b"unexpected diagnostic\n"):
            with self.subTest(raw_digest=hashlib.sha256(raw).hexdigest()), self.assertRaisesRegex(
                    module.Refusal, "^container-resource-progress-invalid$"):
                self.run_gate(raw)
        rows = self.records()
        for row in rows:
            row.pop("workers", None)
        with self.assertRaisesRegex(module.Refusal, "^container-resource-workers-missing$") as refused:
            self.run_gate(self.encode(rows))
        self.assertEqual(refused.exception.evidence["exit_code"], 0)

    def test_progress_output_limit_is_enforced_before_json_parsing(self):
        raw = b"x" * (module.MAX_OUTPUT + 1)
        with patch.object(module, "load_json") as parser, self.assertRaisesRegex(
                module.Refusal, "^container-resource-progress-invalid$"):
            module.resource_workers(raw)
        parser.assert_not_called()

    def test_independent_verification_still_refuses_changed_media(self):
        verification = EvidenceTests().valid()
        verification["checks"]["decoded_content_equal"] = False
        with self.assertRaisesRegex(module.Refusal, "^media-proof-invalid$"):
            self.run_gate(verification=verification)

    def test_native_cpu_and_memory_caps_remain_required_before_conversion(self):
        base = {"kind": "native-hardware-observation", "memory_source": "process-cgroup",
                "effective_cpus": 2, "available_memory_bytes": 512 * 1024 ** 2}
        for field, value in (("effective_cpus", 0), ("effective_cpus", 3), ("effective_cpus", True),
                             ("available_memory_bytes", 0), ("available_memory_bytes", 1024 ** 3 + 1),
                             ("available_memory_bytes", True), ("memory_source", "host")):
            image = Mock()
            image.checked.return_value = json.dumps({**base, field: value}).encode()
            with self.subTest(field=field, value=value), self.assertRaisesRegex(
                    module.Refusal, "^container-resource-observation-invalid$"):
                module.resources(image)
            image.invoke.assert_not_called()

    def test_exit_and_media_success_remain_required_and_diagnostics_are_bounded(self):
        for status, stdout in ((1, b"private-marker"), (0, b"private-marker"), ("private-marker", b"")):
            with self.subTest(status=status), self.assertRaisesRegex(
                    module.Refusal, "^container-resource-cli-failed$") as refused:
                self.run_gate(status=status, stdout=stdout)
            self.assertEqual(refused.exception.evidence["exit_code"], status if type(status) is int else None)
            self.assertEqual(set(refused.exception.evidence), {"exit_code", "stdout_sha256", "stderr_sha256"})
            self.assertNotIn("private-marker", json.dumps(refused.exception.evidence))


class EvidenceTests(unittest.TestCase):
    def test_empty_path_discovery_requires_the_current_runtime_inventory(self):
        tools = [{"name": name, "available": False} for name in ("ffmpeg", "ffprobe", "packager")]
        report = {"ready": False, "mode": "discovery", "tools": tools}
        self.assertEqual(module.require_empty_path_discovery(1, json.dumps(report)),
                         {"exit_code": 1, "ready": False, "tools_missing": 3})
        for changed in (tools + [{"name": "mp4dump", "available": False}], tools[:-1],
                        tools[:-1] + [{"name": "ffmpeg", "available": False}],
                        tools[:-1] + [{"name": "packager", "available": True}]):
            with self.subTest(tools=changed), self.assertRaisesRegex(
                    module.Refusal, "^empty-path-discovery-invalid$"):
                module.require_empty_path_discovery(1, json.dumps({**report, "tools": changed}))

    def test_web_analysis_admission_is_bound_to_both_request_receipts(self):
        server, policy, session, job = [value * 32 for value in "abcd"]
        creation, request = [value * 32 for value in "12"]
        session_reply = {"schema_version": 1, "server_id": server, "session_id": session,
                         "creation_request_id": creation}
        job_reply = {"schema_version": 1, "server_id": server, "session_id": session,
                     "request_id": request, "job_id": job, "operation": "analyze", "state": "running"}
        for invalid in (None, "session-schema", "creation", "job-schema", "job-session", "job-request"):
            with self.subTest(invalid=invalid):
                admitted_session, admitted_job = dict(session_reply), dict(job_reply)
                if invalid == "session-schema":
                    admitted_session["schema_version"] = 2
                elif invalid == "creation":
                    admitted_session["creation_request_id"] = "e" * 32
                elif invalid == "job-schema":
                    admitted_job["schema_version"] = 2
                elif invalid == "job-session":
                    admitted_job["session_id"] = "e" * 32
                elif invalid == "job-request":
                    admitted_job["request_id"] = "e" * 32
                responses = [(201, json.dumps(admitted_session).encode()),
                             (202, json.dumps(admitted_job).encode())]
                with patch.object(module, "http_request", side_effect=responses) as calls, \
                        patch.object(module.uuid, "uuid4", side_effect=[module.uuid.UUID(creation),
                                                                      module.uuid.UUID(request)]):
                    if invalid is None:
                        self.assertEqual(module.submit_web_analysis(8080, server, policy,
                                         "http://127.0.0.1/master.m3u8"), job)
                        self.assertEqual(calls.call_count, 2)
                    else:
                        code = "web-real-session-not-created" if invalid in ("session-schema", "creation") else "web-real-job-not-launched"
                        with self.assertRaisesRegex(module.Refusal, "^" + code + "$"):
                            module.submit_web_analysis(8080, server, policy, "http://127.0.0.1/master.m3u8")

    def test_common_web_context_uses_only_opaque_private_file_references(self):
        server, root, file = "a" * 32, "b" * 32, "c" * 32
        responses = [(200, json.dumps({"schema_version": 1, "server_id": server,
                      "roots": [{"root_ref": root, "roles": ["keys", "fetch-policy"]}]}).encode()),
                     (200, json.dumps({"schema_version": 1, "server_id": server, "next_cursor": None,
                      "entries": [{"kind": "file", "file_ref": file, "label": "Configuration 1"}]}).encode())]
        with patch.object(module, "http_request", side_effect=responses) as request:
            self.assertEqual(module.common_web_context(8080), (server, file))
        self.assertEqual(request.call_args_list[0].args, (8080, "GET", "/api/common/capabilities"))
        self.assertEqual(request.call_args_list[1].args,
                         (8080, "GET", "/api/common/files?root_ref=" + root + "&role=fetch-policy"))

    def test_common_web_context_refuses_changed_server_or_missing_configuration(self):
        for entries, reply_server in (([], "a" * 32),
                                      ([{"kind": "file", "file_ref": "c" * 32}], "d" * 32)):
            responses = [(200, json.dumps({"schema_version": 1, "server_id": "a" * 32,
                          "roots": [{"root_ref": "b" * 32, "roles": ["fetch-policy"]}]}).encode()),
                         (200, json.dumps({"schema_version": 1, "server_id": reply_server,
                          "next_cursor": None, "entries": entries}).encode())]
            with patch.object(module, "http_request", side_effect=responses):
                with self.assertRaisesRegex(module.Refusal, "^web-reference-invalid$"):
                    module.common_web_context(8080)

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

            def checked(self, *_, **__):
                return b'{"ready":true,"cleanup_status":"confirmed"}'

            def invoke(self, *_, **__):
                tools = [{"name": name, "available": False} for name in ("ffmpeg", "ffprobe", "packager")]
                return 1, json.dumps({"ready": False, "mode": "discovery", "tools": tools}), b""

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
            self.assertEqual(actual["active_gate"], "web")

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
                body = json.dumps({"schema_version": 1, "server_id": "a" * 32, "error": {"code": error}}).encode("utf-8")
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
            ("/api/common/capabilities", ["other.invalid"]),
            ("/api/common/capabilities", ["user@localhost"])])

    def test_real_http_guard_controls_refuse_success_and_wrong_failure_classes(self):
        for host, reply in (("other.invalid", (200, "untrusted-host")),
                            ("other.invalid", (400, "invalid-host")),
                            ("user@localhost", (403, "untrusted-host")),
                            ("user@localhost", (400, "untrusted-host"))):
            with self.subTest(host=host, reply=reply):
                port, requests = self.host_guard_control({host: reply})
                with self.assertRaisesRegex(module.Refusal, "^web-host-guard-failed$"):
                    module.verify_web_host_guards(port)
                self.assertEqual(requests[-1], ("/api/common/capabilities", [host]))

    def test_published_barrier_is_observed_when_the_real_child_exits_between_samples(self):
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / "shutdown-ready"
            script = "import sys; from pathlib import Path; sys.stdin.buffer.read(1); Path(sys.argv[1]).write_bytes(b'ready')"
            with subprocess.Popen([sys.executable, "-c", script, str(path)], stdin=subprocess.PIPE,
                                  stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL) as child:
                owner = self

                class Barrier:
                    sampled = False

                    def is_file(self):
                        if self.sampled:
                            return path.is_file()
                        self.sampled = True
                        observed = path.is_file()
                        owner.assertFalse(observed)
                        child.stdin.write(b"!")
                        child.stdin.flush()
                        owner.assertEqual(child.wait(timeout=2), 0)
                        owner.assertTrue(path.is_file())
                        return observed

                module.wait_file(Barrier(), child)
                self.assertEqual(path.read_bytes(), b"ready")

    def test_a_completed_child_without_its_barrier_still_fails_closed(self):
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / "shutdown-ready"
            with subprocess.Popen([sys.executable, "-c", "pass"]) as child:
                self.assertEqual(child.wait(timeout=2), 0)
                with self.assertRaisesRegex(module.Refusal, "^web-lifecycle-barrier-missing$") as denied:
                    module.wait_file(path, child)
            self.assertEqual(denied.exception.evidence, {
                "barrier": "shutdown-ready", "reason": "driver-exited", "driver_returncode": 0})
            self.assertFalse(path.exists())

    def test_an_alive_child_cannot_remove_the_original_barrier_deadline(self):
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / "shutdown-ready"
            with subprocess.Popen([sys.executable, "-c", "import sys; sys.stdin.buffer.read()"],
                                  stdin=subprocess.PIPE) as child:
                with patch.object(module.time, "monotonic", side_effect=[100, 130]):
                    with self.assertRaisesRegex(module.Refusal, "^web-lifecycle-barrier-missing$") as denied:
                        module.wait_file(path, child)
                self.assertEqual(denied.exception.evidence["reason"], "deadline")
                self.assertIsNone(denied.exception.evidence["driver_returncode"])

    def test_web_failure_keeps_fixed_diagnostics_before_workspace_cleanup(self):
        class Image:
            def command(self, *_args, **_kwargs):
                lines = ["private key and URL must not enter evidence",
                         "container-qualification: phase=web-await-stop-job",
                         "container-qualification: check=registry-record",
                         "container-qualification: job-state=unavailable",
                         "container-qualification: owner-exit code=None signal=Some(15)",
                         "container-qualification: lifecycle-unconfirmed kind=PermissionDenied errno=Some(13)"]
                message = "\n".join(lines) + "\n"
                script = f"import sys; sys.stderr.write({message!r}); raise SystemExit(7)"
                return [sys.executable, "-c", script]

        def request(_port, method, path, body=None, headers=None):
            if headers and headers.get("Origin") == "https://other.invalid":
                return 403, b'{"error":{"code":"same-origin-required"}}'
            if method == "GET" and path == "/api/common/capabilities":
                return 200, b"{}"
            self.fail("Only the explicit common job boundary may launch a fixture job")

        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            (root / "input/hls").mkdir(parents=True)
            (root / "work/web-control").mkdir(parents=True)
            with patch.object(module, "http_request", side_effect=request), \
                    patch.object(module, "verify_web_host_guards", return_value=[]), \
                    patch.object(module, "common_web_context", return_value=("a" * 32, "b" * 32)), \
                    patch.object(module, "submit_web_analysis", return_value="0" * 32):
                with self.assertRaisesRegex(module.Refusal, "^web-lifecycle-barrier-missing$") as denied:
                    module.web(Image(), root)
            evidence = denied.exception.evidence
            self.assertEqual(evidence["driver_returncode"], 7)
            self.assertEqual(evidence["driver_phase"], "web-await-stop-job")
            self.assertEqual(evidence["driver_check"], "registry-record")
            self.assertEqual(evidence["driver_job_state"], "unavailable")
            self.assertEqual(evidence["error_kind"], "PermissionDenied")
            self.assertEqual(evidence["errno"], 13)
            self.assertTrue(evidence["server_exit_observed"])
            self.assertIsNone(evidence["server_returncode"])
            self.assertEqual(evidence["server_signal"], 15)
            self.assertTrue(evidence["barriers"]["stop-job"])
            self.assertFalse(evidence["barriers"]["stop-ready"])
            self.assertEqual(evidence["failure_context"]["barrier"], "stop-ready")
            log = root / "work/web-driver.stderr"
            self.assertEqual(evidence["logs"]["driver-stderr"]["sha256"], module.sha256(log))
            self.assertNotIn("private key", json.dumps(evidence))
        self.assertFalse(root.exists())
        self.assertEqual(evidence["driver_returncode"], 7)

    def test_diagnostic_log_reads_are_bounded_and_do_not_follow_links(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            (root / "work/web-control").mkdir(parents=True)
            (root / "work/web-driver.stderr").write_bytes(b"x" * 33)
            with patch.object(module, "MAX_OUTPUT", 32):
                evidence = module.web_failure_evidence(root, None)
            self.assertEqual(evidence["logs"]["driver-stderr"], {"output_limit": True})
            self.assertIsNone(evidence["driver_phase"])
            if sys.platform != "win32":
                (root / "work/web-control/server.stderr").symlink_to(root / "work/web-driver.stderr")
                self.assertNotIn("server-stderr", module.web_failure_evidence(root, None)["logs"])

    def diagnostic_evidence(self, driver, server=b""):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            (root / "work/web-control").mkdir(parents=True)
            (root / "work/web-driver.stderr").write_bytes(driver)
            (root / "work/web-control/server.stderr").write_bytes(server)
            return module.web_failure_evidence(root, None)

    def test_every_stop_and_shutdown_phase_is_retained_exactly(self):
        phases = (
            "web-await-stop-job", "web-check-stop-listener", "web-await-stop-fixture",
            "web-await-stop-terminal", "web-check-stop-return", "web-await-shutdown-job",
            "web-await-shutdown-fixture", "web-await-shutdown-exit",
            "web-check-shutdown-return", "web-shutdown-return-state",
        )
        for phase in phases:
            with self.subTest(phase=phase):
                evidence = self.diagnostic_evidence(
                    f"container-qualification: phase={phase}\n".encode("ascii"))
                self.assertEqual(evidence["driver_phase"], phase)

    def test_only_the_agreed_check_and_job_state_tokens_are_retained(self):
        checks = (
            "registry-bounds", "registry-entry", "registry-record", "fixture-deadline",
            "owner-exit", "tool-parent", "tool-group", "descendant-parent", "descendant-group",
            "owner-group", "held-lock", "tool-absent", "descendant-absent", "group-absent",
            "lock-released", "job-state", "loopback-listener",
        )
        for check in checks:
            with self.subTest(check=check):
                evidence = self.diagnostic_evidence(
                    f"container-qualification: check={check}\n".encode("ascii"))
                self.assertEqual(evidence["driver_check"], check)
        for state in ("running", "interrupted", "failed", "succeeded", "unavailable"):
            with self.subTest(state=state):
                evidence = self.diagnostic_evidence(
                    f"container-qualification: job-state={state}\n".encode("ascii"))
                self.assertEqual(evidence["driver_job_state"], state)

    def test_unknown_malformed_and_unbounded_labels_cannot_replace_last_valid_diagnostics(self):
        lines = [
            "container-qualification: phase=web-await-stop-fixture",
            "container-qualification: check=registry-entry",
            "container-qualification: job-state=running",
            "container-qualification: phase=web-check-shutdown-return",
            "container-qualification: check=lock-released",
            "container-qualification: job-state=interrupted",
        ]
        for field, valid in (("phase", "web-await-stop-job"), ("check", "tool-absent"),
                             ("job-state", "succeeded")):
            lines.extend((
                f"container-qualification: {field}=PRIVATE_DIAGNOSTIC_SENTINEL",
                f"container-qualification: {field}={valid} PRIVATE_DIAGNOSTIC_SENTINEL",
                f"container-qualification: {field}= {valid}",
                f"container-qualification: {field}={valid}\t",
                f"container-qualification: {field}={valid}\x00PRIVATE_DIAGNOSTIC_SENTINEL",
                f"prefix container-qualification: {field}={valid}",
                f"container-qualification: {field}=" + "x" * 4096,
            ))
        evidence = self.diagnostic_evidence(("\n".join(lines) + "\n").encode("ascii"))
        self.assertEqual(evidence["driver_phase"], "web-check-shutdown-return")
        self.assertEqual(evidence["driver_check"], "lock-released")
        self.assertEqual(evidence["driver_job_state"], "interrupted")
        self.assertNotIn("PRIVATE_DIAGNOSTIC_SENTINEL", json.dumps(evidence))

    def test_private_and_server_lines_only_produce_bounded_log_fingerprints(self):
        driver = (b"PRIVATE_DIAGNOSTIC_SENTINEL key=not-a-real-key https://private.invalid/\n"
                  b"container-qualification: phase=PRIVATE_DIAGNOSTIC_SENTINEL\n"
                  b"container-qualification: check=PRIVATE_DIAGNOSTIC_SENTINEL\n"
                  b"container-qualification: job-state=PRIVATE_DIAGNOSTIC_SENTINEL\n"
                  b"container-qualification: check=tool-absent\xff\n")
        server = (b"container-qualification: phase=web-check-stop-return\n"
                  b"container-qualification: check=tool-absent\n"
                  b"container-qualification: job-state=succeeded\n")
        evidence = self.diagnostic_evidence(driver, server)
        for field in ("driver_phase", "driver_check", "driver_job_state"):
            self.assertIsNone(evidence[field])
        self.assertEqual(evidence["logs"], {
            "driver-stderr": {"snapshot_bytes": len(driver), "sha256": hashlib.sha256(driver).hexdigest()},
            "server-stderr": {"snapshot_bytes": len(server), "sha256": hashlib.sha256(server).hexdigest()},
        })
        serialized = json.dumps(evidence)
        for private in ("PRIVATE_DIAGNOSTIC_SENTINEL", "not-a-real-key", "private.invalid"):
            self.assertNotIn(private, serialized)

    def test_oversized_log_never_publishes_even_an_initial_valid_diagnostic(self):
        driver = (b"container-qualification: phase=web-check-stop-return\n"
                  b"container-qualification: check=tool-absent\n"
                  b"container-qualification: job-state=succeeded\n" + b"x" * 512)
        with patch.object(module, "MAX_OUTPUT", 256):
            evidence = self.diagnostic_evidence(driver)
        self.assertEqual(evidence["logs"]["driver-stderr"], {"output_limit": True})
        for field in ("driver_phase", "driver_check", "driver_job_state"):
            self.assertIsNone(evidence[field])

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
