"""Assemble only coherent source-bound release packets from read-only CI data."""
import contextlib
import copy
import hashlib
import importlib.util
import io
import json
from pathlib import Path
import stat
import subprocess
import tempfile
import unittest
import zipfile
from unittest.mock import Mock, patch
import sys

sys.path.insert(0, str(Path(__file__).resolve().parent))
import roundtrip_reports

SPEC = importlib.util.spec_from_file_location("prepare_release", Path(__file__).resolve().parents[1] / "prepare_release.py")
PREPARE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(PREPARE)
QUALIFY = PREPARE.module("qualify_cli_archive")


class FakeGitHub:
    def __init__(self, head, tree):
        self.head, self.tree = head, tree
        self.calls, self.downloads = [], []
        self.workflows, self.runs, self.artifacts = {}, {}, {}
        self.entries, self.outer = {}, {}
        for index, name in enumerate(PREPARE.WORKFLOWS, 1):
            workflow = {"id": index, "path": ".github/workflows/" + name, "state": "active"}
            self.workflows[name] = workflow
            self.runs[name] = [{"id": 100 + index, "run_number": 10, "run_attempt": 1, "workflow_id": index,
                                "path": workflow["path"], "event": "push", "head_branch": "main", "head_sha": head,
                                "repository": {"full_name": "example/drmr"}, "head_repository": {"full_name": "example/drmr"},
                                "status": "completed", "conclusion": "success", "run_started_at": "2026-10-22T00:00:00Z",
                                "updated_at": "2026-10-22T00:30:00Z"}]
        for index, (workflow, name, target, suffix) in enumerate(PREPARE.TARGETS, 1):
            archive = ("native archive fixture " + target).encode()
            archive_hash = hashlib.sha256(archive).hexdigest()
            binary_hash = hashlib.sha256(target.encode()).hexdigest()
            archive_report = {"success": True, "source_head": head, "source_tree": tree, "target": target, "kind": "cli",
                              "archive_sha256": archive_hash, "files": 100, "packages": 20,
                              "binaries": {"drmr": {"sha256": binary_hash, "bytes": 1000, "mode": "0o755", "header_target": target}},
                              "pair_check": "not-applicable", "raw_notice_files": 20, "limits": PREPARE.ARCHIVE_LIMITS}
            proof = {"media_verified": True, "license_playback_verified": False,
                     "checks": {"stream_metadata": "pass", "packet_payloads_equal": True, "packet_timing_equal": True, "decoded_content_equal": True},
                     "native_manifest": {"schema_version": 1, "scope": "manifest-only", "profile": "dash-static-manifest", "errors": 0,
                                         "warnings": 0, "truncated": False, "incomplete": False, "document_count": 1}}
            graph = {"sha256": "d" * 64, "files": 7, "bytes": 10000}
            media = {"schema_version": 1, "success": True, "kind": "native-cli-archive-media", "source_head": head, "source_tree": tree,
                     "target": target, "version": "1.0.0", "binary_reported_version": "1.0.0", "archive_sha256": archive_hash,
                     "binary_sha256": binary_hash, "archive_files": 100, "dependency_packages": 20, "native_ordinary_user": True,
                     "owned_workspace_cleanup": "confirmed", "limits": PREPARE.MEDIA_LIMITS,
                     "metadata_analysis": {"schema_version": 1, "format": "dash", "protection": "clear",
                                           "evidence": "manifest", "media_verified": False, "source_unchanged": True},
                     "doctor": QUALIFY.doctor({"schema_version": 1, "mode": "probe", "profile": "all", "ready": True,
                                               "cleanup_status": "confirmed", "cancelled": False, "media_compatibility_checked": False,
                                               "tools": [{"name": tool, "available": True, "required": True, "version_status": "supported",
                                                          "feature_status": "available", "failure_code": None}
                                                         for tool in ("ffmpeg", "ffprobe", "packager")]}),
                     "media": {"fixture_sha256": PREPARE.FIXTURE_SHA256, "fixture_duration_seconds": 10,
                               "scope": "all 240 original H.264 video packets and frames; audio not selected",
                               "input_format": "dash", "input_protection": "clear", "output_format": "dash", "output_protection": "cbcs",
                               "segment_target_seconds": 2, "input_segments": 5, "source_graph": graph,
                               "cli_source_graph_sha256": "e" * 64, "plan_schema_version": 1, "plan_id": "f" * 64,
                               "plan_sha256": "a" * 64, "saved_original_and_plan_unchanged": True,
                               "prepared_source_against_original_video": copy.deepcopy(proof), "conversion": copy.deepcopy(proof),
                               "independent_verification": copy.deepcopy(proof), "output_graph": copy.deepcopy(graph)},
                     "stages": [{"stage": stage, "elapsed_milliseconds": 1, "exit_code": 0} for stage in PREPARE.STAGES]}
            audio = {**copy.deepcopy(media["media"]),
                     "scope": "all generated 44.1 kHz AAC frames; fixture audio resampled from the attributed clip",
                     "input_format": "hls-ts", "input_segments": 6,
                     "cli_source_graph_sha256": "1" * 64, "plan_id": "2" * 64, "plan_sha256": "3" * 64,
                     "source_audio": QUALIFY.audio_evidence({
                         "streams": [{"codec_name": "aac", "profile": "LC", "sample_rate": "44100", "channels": 2,
                                      "time_base": "1/90000", "nb_read_frames": "432"}],
                         "packets": [{"pts": 126000 + packet * 1024 * 90000 // 44100,
                                      "dts": 126000 + packet * 1024 * 90000 // 44100,
                                      "duration": 2089} for packet in range(432)]})}
            audio.pop("prepared_source_against_original_video")
            media["aac_44100"] = audio
            self.entries[name] = [("drmr-1.0.0-" + target + suffix, archive), ("cli-archive-verification.json", json.dumps(archive_report).encode()),
                                  ("cli-media-qualification.json", json.dumps(media).encode())]
            self.artifacts[name] = {"id": 200 + index, "name": name, "expired": False, "created_at": "2026-10-22T00:10:00Z",
                                    "workflow_run": {"id": self.runs[workflow][0]["id"], "head_sha": head, "head_branch": "main"}}
            self.rebuild(name)
        self.extra_artifacts = []
        self.on_json = None

    def rebuild(self, name, compression=zipfile.ZIP_STORED):
        stream = io.BytesIO()
        with zipfile.ZipFile(stream, "w", compression=compression) as archive:
            for filename, data in self.entries[name]:
                archive.writestr(filename, data)
        self.outer[name] = stream.getvalue()
        self.artifacts[name].update(size_in_bytes=len(self.outer[name]), digest="sha256:" + hashlib.sha256(self.outer[name]).hexdigest())

    def report(self, name, filename, change):
        entries = []
        for current, raw in self.entries[name]:
            if current == filename:
                value = json.loads(raw)
                change(value)
                raw = json.dumps(value).encode()
            entries.append((current, raw))
        self.entries[name] = entries
        self.rebuild(name)

    def json(self, endpoint):
        self.calls.append(endpoint)
        if self.on_json:
            self.on_json(endpoint)
        path, _, query = endpoint.partition("?")
        base = "repos/example/drmr"
        if path == base + "/git/ref/heads/main":
            return {"ref": "refs/heads/main", "object": {"type": "commit", "sha": self.head}}
        prefix = base + "/actions/workflows/"
        if path.startswith(prefix):
            tail = path[len(prefix):]
            if tail in self.workflows:
                return copy.deepcopy(self.workflows[tail])
            identity = int(tail.split("/")[0])
            name = next(name for name, workflow in self.workflows.items() if workflow["id"] == identity)
            return {"total_count": len(self.runs[name]), "workflow_runs": copy.deepcopy(self.runs[name])}
        prefix = base + "/actions/runs/"
        if path.startswith(prefix):
            tail = path[len(prefix):]
            identity = int(tail.split("/")[0])
            run = next(run for runs in self.runs.values() for run in runs if run["id"] == identity)
            if "/attempts/" in tail:
                return copy.deepcopy(run)
            artifacts = [artifact for artifact in list(self.artifacts.values()) + self.extra_artifacts if artifact["workflow_run"]["id"] == identity]
            return {"total_count": len(artifacts), "artifacts": copy.deepcopy(artifacts)}
        raise AssertionError("Unexpected read-only request")

    def download(self, endpoint, destination):
        self.downloads.append(endpoint)
        identity = int(endpoint.split("/")[-2])
        name = next(name for name, artifact in self.artifacts.items() if artifact["id"] == identity)
        with destination.open("xb") as stream:
            stream.write(self.outer[name])


class ReleasePacket(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.home = Path(self.temporary.name)
        self.root = self.home / "source"
        self.root.mkdir()
        self.output = self.home / "release"
        self.write("Cargo.toml", '[workspace]\nresolver = "2"\nmembers = ["crates/drmr"]\n[workspace.package]\nversion = "1.0.0"\nedition = "2021"\n')
        self.write("crates/drmr/Cargo.toml", '[package]\nname = "drmr"\nversion.workspace = true\nedition.workspace = true\n')
        self.write("crates/drmr/src/lib.rs", "")
        self.write("Cargo.lock", 'version = 4\n\n[[package]]\nname = "drmr"\nversion = "1.0.0"\n')
        self.write("crates/drmr-desktop/tauri.conf.json", '{"version":"1.0.0"}')
        self.write("CHANGELOG.md", '# Changelog\n\n## [Unreleased]\n\n## [1.0.0] - 2026-10-22\n\nCurated release notes.\n\n### Changes\n\n- Stable CLI.\n')
        self.git("init", "--quiet")
        self.git("add", ".")
        self.git("-c", "user.name=Release Test", "-c", "user.email=release-test@example.invalid", "commit", "--quiet", "-m", "Release fixture")
        self.git("tag", "v1.0.0")
        self.head, self.tree = self.git("rev-parse", "HEAD"), self.git("rev-parse", "HEAD^{tree}")
        self.client = FakeGitHub(self.head, self.tree)

    def write(self, name, content):
        path = self.root / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(content, encoding="utf-8")

    def git(self, *args):
        return subprocess.run(["git", *args], cwd=self.root, check=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE).stdout.decode().strip()

    def prepare(self, qualified=False):
        return PREPARE.prepare(self.root, "v1.0.0", self.output, "example/drmr", self.client,
                               roundtrip_reports=self.home / "roundtrips" if qualified else None,
                               native_inputs_only=not qualified)

    def roundtrip_reports(self):
        directory = self.home / "roundtrips"
        directory.mkdir()
        for _, artifact, target, _ in PREPARE.TARGETS:
            archive = json.loads(dict(self.client.entries[artifact])["cli-archive-verification.json"])
            value = roundtrip_reports.complete(PREPARE.ROUNDTRIP, self.head, self.tree, target, "1.0.0",
                                               archive["archive_sha256"], archive["binaries"]["drmr"]["sha256"])
            (directory / ("cli-release-roundtrip-" + target + ".json")).write_text(json.dumps(value), encoding="utf-8")
        return directory

    def test_unqualified_input_packet_is_explicitly_not_publication_ready(self):
        result = self.prepare()
        manifest = json.loads((self.output / "release-manifest.json").read_text())
        self.assertFalse(result["publication_ready"])
        self.assertFalse(manifest["publication_ready"])
        self.assertEqual(manifest["kind"], "native-cli-roundtrip-inputs")

    def test_assembly_requires_a_release_report_or_explicit_input_staging(self):
        with self.assertRaisesRegex(PREPARE.Failure, "^roundtrip-reports-required$"):
            PREPARE.prepare(self.root, "v1.0.0", self.output, "example/drmr", self.client)
        self.assertFalse(self.output.exists())

    def test_qualified_packet_contains_and_checksums_all_three_matrix_reports(self):
        self.roundtrip_reports()
        result = self.prepare(qualified=True)
        manifest = json.loads((self.output / "release-manifest.json").read_text())
        self.assertTrue(result["publication_ready"])
        self.assertEqual(manifest["kind"], "verified-cli-release")
        self.assertTrue(manifest["publication_ready"])
        self.assertEqual(len(list(self.output.iterdir())), 15)
        checksums = (self.output / "SHA256SUMS").read_text()
        for asset in manifest["assets"]:
            report = asset["roundtrip_report"]
            self.assertEqual(PREPARE.digest(self.output / report["filename"]), report["sha256"])
            self.assertIn(report["sha256"] + "  " + report["filename"], checksums)

    def test_missing_stale_or_partial_roundtrip_reports_block_assembly(self):
        directory = self.roundtrip_reports()
        target = PREPARE.TARGETS[0][2]
        path = directory / ("cli-release-roundtrip-" + target + ".json")
        original = path.read_bytes()
        mutations = [lambda value: value.update(source_head="f" * 40),
                     lambda value: value["records"].pop(),
                     lambda value: value["counts"].update(complete=737),
                     lambda value: value["records"][0].update(status="not-checked")]
        for change in mutations:
            value = json.loads(original)
            change(value)
            path.write_text(json.dumps(value), encoding="utf-8")
            with self.assertRaises(PREPARE.Failure):
                self.prepare(qualified=True)
            self.assertFalse(self.output.exists())
        path.unlink()
        with self.assertRaisesRegex(PREPARE.Failure, "roundtrip-report-missing-or-invalid"):
            self.prepare(qualified=True)

    def refuse(self, token=None):
        with self.assertRaises(PREPARE.CHECK.Failure) as context:
            self.prepare()
        if token:
            self.assertEqual(str(context.exception), token)
        self.assertFalse(self.output.exists())
        self.assertEqual(list(self.home.glob(".drmr-release-*")), [])

    def test_coherent_packet_has_three_archives_and_bound_reports(self):
        result = self.prepare()
        self.assertTrue(result["success"])
        self.assertFalse(result["publication_performed"])
        self.assertEqual(len(self.client.downloads), 3)
        files = {path.name for path in self.output.iterdir()}
        self.assertEqual(len(files), 12)
        manifest = json.loads((self.output / "release-manifest.json").read_text())
        self.assertEqual(manifest["source_head"], self.head)
        self.assertEqual(manifest["source_tree"], self.tree)
        self.assertEqual(len(manifest["workflow_runs"]), 5)
        for asset in manifest["assets"]:
            self.assertEqual(hashlib.sha256((self.output / asset["filename"]).read_bytes()).hexdigest(), asset["sha256"])
            for name in ("archive_report", "media_report"):
                self.assertEqual(hashlib.sha256((self.output / asset[name]["filename"]).read_bytes()).hexdigest(), asset[name]["sha256"])
        sums = (self.output / "SHA256SUMS").read_text().splitlines()
        self.assertEqual(len(sums), 10)
        for line in sums:
            expected, name = line.split("  ", 1)
            self.assertEqual(hashlib.sha256((self.output / name).read_bytes()).hexdigest(), expected)
        self.assertNotIn("release-notes.md", "\n".join(sums))
        self.assertEqual((self.output / "release-notes.md").read_text(), "Curated release notes.\n\n### Changes\n\n- Stable CLI.\n")
        self.assertEqual(self.git("status", "--porcelain"), "")
        self.assertEqual(list(self.home.glob(".drmr-release-*")), [])

    def test_deflated_artifacts_preserve_archive_and_report_bindings(self):
        for _, name, _, _ in PREPARE.TARGETS:
            self.client.rebuild(name, zipfile.ZIP_DEFLATED)
        result = self.prepare()
        self.assertTrue(result["success"])
        self.assertEqual(result["archives"], 3)
        self.assertEqual(list(self.home.glob(".drmr-release-*")), [])

    def test_valid_lzma_artifact_is_refused_before_publication(self):
        self.client.rebuild("drmr-Linux-X64", zipfile.ZIP_LZMA)
        self.refuse("artifact-archive-invalid")

    def test_corrupt_lzma_artifact_has_only_a_fixed_diagnostic(self):
        name = "drmr-Linux-X64"
        self.client.rebuild(name, zipfile.ZIP_LZMA)
        damaged = bytearray(self.client.outer[name])
        payload_at = 30 + int.from_bytes(damaged[26:28], "little") + int.from_bytes(damaged[28:30], "little")
        self.assertEqual(int.from_bytes(damaged[8:10], "little"), zipfile.ZIP_LZMA)
        self.assertEqual(damaged[payload_at + 2:payload_at + 4], b"\x05\x00")
        damaged[payload_at + 4] = 0xff
        self.client.outer[name] = bytes(damaged)
        self.client.artifacts[name].update(size_in_bytes=len(damaged), digest="sha256:" + hashlib.sha256(damaged).hexdigest())
        self.refuse("artifact-archive-invalid")

    def test_media_producer_projection_and_versioned_ci_names_are_accepted(self):
        # Invoke the real producer result assembly with command/native execution
        # replaced at their boundaries. This is a schema contract, not media proof.
        for _, artifact, target, suffix in PREPARE.TARGETS:
            entries = dict(self.client.entries[artifact])
            archive_proof = json.loads(entries["cli-archive-verification.json"])
            previous = json.loads(entries["cli-media-qualification.json"])
            candidate = self.home / (artifact + ".candidate")
            candidate.write_bytes(entries["drmr-1.0.0-" + target + suffix])
            source = Mock(head=self.head, tree=self.tree)
            source.read.side_effect = lambda name: (self.root / name).read_bytes()
            runner = Mock(stages=previous["stages"])
            runner.run.return_value = b"drmr 1.0.0\n"
            runner.json.return_value = {"schema_version": 1, "mode": "probe", "profile": "all", "ready": True,
                                        "cleanup_status": "confirmed", "cancelled": False, "media_compatibility_checked": False,
                                        "tools": [{**tool, "failure_code": None} for tool in previous["doctor"]["tools"]]}
            def extract(path, selected_target, expected_hash, destination):
                runner.workspace = destination.parent
                destination.write_bytes(selected_target.encode())
                return destination
            with patch.object(QUALIFY, "require_native"), patch.object(QUALIFY.VERIFY, "Source", return_value=source), \
                    patch.object(QUALIFY.VERIFY, "metadata", return_value={}), patch.object(QUALIFY.VERIFY, "build_expectation", return_value={}), \
                    patch.object(QUALIFY.VERIFY, "qualify", return_value=archive_proof), patch.object(QUALIFY, "extract_cli", side_effect=extract), \
                    patch.object(QUALIFY, "Runner", return_value=runner), patch.object(QUALIFY, "media", return_value=previous["media"]), \
                    patch.object(QUALIFY, "audio_media", return_value=previous["aac_44100"]), \
                    patch.object(QUALIFY, "metadata_analysis", return_value=previous["metadata_analysis"]) as metadata:
                result = QUALIFY.qualify(candidate, target, "1.0.0", self.root)
                metadata.assert_called_once_with(runner.workspace / ("drmr.exe" if target.endswith("windows-msvc") else "drmr"),
                                                 runner, runner.workspace / "dash/manifest.mpd")
            self.client.report(artifact, "cli-media-qualification.json", lambda report: (report.clear(), report.update(result)))
        self.assertTrue(self.prepare()["success"])
        for _, artifact, target, suffix in PREPARE.TARGETS:
            self.assertEqual(self.client.entries[artifact][0][0], "drmr-1.0.0-" + target + suffix)
            media = json.loads((self.output / ("cli-media-qualification-" + target + ".json")).read_text())
            self.assertEqual({tool["name"] for tool in media["doctor"]["tools"]}, {"ffmpeg", "ffprobe", "packager"})
            self.assertEqual(media["aac_44100"]["source_audio"]["sample_rate"], 44100)
            self.assertEqual(len(media["stages"]), 18)
            self.assertEqual(media["metadata_analysis"], previous["metadata_analysis"])

    def test_current_single_plan_version_is_accepted_for_both_media_proofs(self):
        for _, artifact, _, _ in PREPARE.TARGETS:
            def current_plan(report):
                report["media"]["plan_schema_version"] = 5
                report["aac_44100"]["plan_schema_version"] = 5
            self.client.report(artifact, "cli-media-qualification.json", current_plan)
        self.assertTrue(self.prepare()["success"])

    def test_receipts_accept_only_supported_single_plan_versions_for_both_routes(self):
        entries = dict(self.client.entries["drmr-Linux-X64"])
        archive = json.loads(entries["cli-archive-verification.json"])
        media = json.loads(entries["cli-media-qualification.json"])
        for field in ("media", "aac_44100"):
            for version in (1, 3, 5, 7, 9):
                altered = copy.deepcopy(media)
                altered[field]["plan_schema_version"] = version
                with self.subTest(route=field, version=version):
                    PREPARE.reports(archive, altered, self.head, self.tree,
                                    "x86_64-unknown-linux-gnu", "1.0.0", archive["archive_sha256"])
            for version in (0, 2, 4, 6, 8, 10, 11, True, 9.0):
                altered = copy.deepcopy(media)
                altered[field]["plan_schema_version"] = version
                code = "media-proof-invalid" if field == "media" else "aac44100-proof-invalid"
                with self.subTest(route=field, version=version), self.assertRaisesRegex(
                        PREPARE.CHECK.Failure, "^" + code + "$"):
                    PREPARE.reports(archive, altered, self.head, self.tree,
                                    "x86_64-unknown-linux-gnu", "1.0.0", archive["archive_sha256"])

    def test_metadata_analysis_is_mandatory_exact_and_does_not_claim_media_admission(self):
        entries = dict(self.client.entries["drmr-Linux-X64"])
        archive = json.loads(entries["cli-archive-verification.json"])
        media = json.loads(entries["cli-media-qualification.json"])
        changes = [lambda r: r.pop("metadata_analysis"),
                   lambda r: r["metadata_analysis"].update(schema_version=True),
                   lambda r: r["metadata_analysis"].update(schema_version=2),
                   lambda r: r["metadata_analysis"].update(format="hls"),
                   lambda r: r["metadata_analysis"].update(protection="cbcs"),
                   lambda r: r["metadata_analysis"].update(evidence="container-metadata"),
                   lambda r: r["metadata_analysis"].update(media_verified=True),
                   lambda r: r["metadata_analysis"].update(media_verified=0),
                   lambda r: r["metadata_analysis"].update(source_unchanged=False),
                   lambda r: r["metadata_analysis"].update(source_unchanged=1),
                   lambda r: r["metadata_analysis"].update(download={"retained": True}),
                   lambda r: r["metadata_analysis"].update(private_path="private-marker"),
                   lambda r: r["metadata_analysis"].pop("source_unchanged"),
                   lambda r: r["stages"].pop()]
        for change in changes:
            altered = copy.deepcopy(media)
            change(altered)
            with self.subTest(change=change), self.assertRaises(PREPARE.CHECK.Failure):
                PREPARE.reports(archive, altered, self.head, self.tree, "x86_64-unknown-linux-gnu",
                                "1.0.0", archive["archive_sha256"])

    def test_aac44100_route_is_mandatory_and_every_source_frame_is_bound(self):
        entries = dict(self.client.entries["drmr-Linux-X64"])
        archive = json.loads(entries["cli-archive-verification.json"])
        media = json.loads(entries["cli-media-qualification.json"])
        def validate(report):
            PREPARE.reports(archive, report, self.head, self.tree, "x86_64-unknown-linux-gnu",
                            "1.0.0", archive["archive_sha256"])
        validate(media)
        changes = [lambda r: r.pop("aac_44100"),
                   lambda r: r["aac_44100"].update(input_format="hls"),
                   lambda r: r["aac_44100"].update(input_protection="hls-aes128"),
                   lambda r: r["aac_44100"].update(output_protection="clear"),
                   lambda r: r["aac_44100"].update(saved_original_and_plan_unchanged=False),
                   lambda r: r["aac_44100"].update(plan_id="invalid"),
                   lambda r: r["aac_44100"].update(cli_source_graph_sha256="invalid"),
                   lambda r: r["aac_44100"]["source_audio"].update(sample_rate=48000),
                   lambda r: r["aac_44100"]["source_audio"].update(packet_count=431),
                   lambda r: r["aac_44100"]["source_audio"].update(frame_count=True),
                   lambda r: r["aac_44100"]["source_audio"].update(expected_decoded_samples_per_channel=431 * 1024),
                   lambda r: r["aac_44100"]["source_audio"].update(packet_clock_sha256="invalid"),
                   lambda r: r["aac_44100"]["source_audio"].update(last_pts=False),
                   lambda r: r["aac_44100"]["source_audio"].update(last_duration=2090),
                   lambda r: r["aac_44100"]["conversion"]["checks"].update(packet_payloads_equal=1),
                   lambda r: r["aac_44100"]["independent_verification"]["checks"].update(packet_timing_equal=False),
                   lambda r: r["aac_44100"]["independent_verification"]["native_manifest"].update(incomplete=True),
                   lambda r: r["aac_44100"].update(private_key="private-marker"),
                   lambda r: r["aac_44100"]["source_audio"].update(private_path="private-marker")]
        for change in changes:
            altered = copy.deepcopy(media)
            change(altered)
            with self.subTest(change=change), self.assertRaises(PREPARE.CHECK.Failure):
                validate(altered)

    def test_aac44100_all_six_stages_are_required_in_order(self):
        entries = dict(self.client.entries["drmr-Linux-X64"])
        archive = json.loads(entries["cli-archive-verification.json"])
        media = json.loads(entries["cli-media-qualification.json"])
        self.assertEqual(len(PREPARE.STAGES), 18)
        self.assertEqual(PREPARE.STAGES[-1], "metadata-analysis")
        self.assertEqual(PREPARE.MEDIA_LIMITS, QUALIFY.MEDIA_LIMITS)
        for index in range(11, 17):
            altered = copy.deepcopy(media)
            altered["stages"].pop(index)
            with self.subTest(stage=index), self.assertRaisesRegex(PREPARE.CHECK.Failure, "^media-stages-invalid$"):
                PREPARE.reports(archive, altered, self.head, self.tree, "x86_64-unknown-linux-gnu",
                                "1.0.0", archive["archive_sha256"])
        media["stages"][15], media["stages"][16] = media["stages"][16], media["stages"][15]
        with self.assertRaisesRegex(PREPARE.CHECK.Failure, "^media-stages-invalid$"):
            PREPARE.reports(archive, media, self.head, self.tree, "x86_64-unknown-linux-gnu",
                            "1.0.0", archive["archive_sha256"])

    def test_doctor_inventory_and_each_tool_proof_are_required(self):
        entries = dict(self.client.entries["drmr-Linux-X64"])
        archive = json.loads(entries["cli-archive-verification.json"])
        media = json.loads(entries["cli-media-qualification.json"])
        def validate(report):
            PREPARE.reports(archive, report, self.head, self.tree, "x86_64-unknown-linux-gnu",
                            "1.0.0", archive["archive_sha256"])
        validate(media)
        for label, change in (
                ("cleanup", lambda d: d.update(cleanup_status="unconfirmed")),
                ("missing", lambda d: d["tools"].pop()),
                ("extra", lambda d: d["tools"].append({**d["tools"][0], "name": "mp4dump"})),
                ("duplicate", lambda d: d["tools"][1].update(name="ffmpeg"))):
            altered = copy.deepcopy(media)
            change(altered["doctor"])
            with self.subTest(case=label), self.assertRaisesRegex(PREPARE.CHECK.Failure, "^doctor-proof-invalid$"):
                validate(altered)
        for index, tool in enumerate(media["doctor"]["tools"]):
            for field, value in (("available", False), ("required", False),
                                 ("version_status", "unsupported"), ("version_status", "build-unverified"),
                                 ("feature_status", "missing")):
                altered = copy.deepcopy(media)
                altered["doctor"]["tools"][index][field] = value
                with self.subTest(tool=tool["name"], field=field, value=value), self.assertRaisesRegex(
                        PREPARE.CHECK.Failure, "^doctor-proof-invalid$"):
                    validate(altered)

    def test_remote_main_must_be_the_exact_local_tag_commit(self):
        self.client.head = "b" * 40
        self.refuse("remote-main-mismatch")

    def test_workflow_identity_cannot_be_substituted(self):
        self.client.workflows["ci.yml"]["path"] = ".github/workflows/other.yml"
        self.refuse("workflow-identity-invalid")

    def test_wrong_workflow_source_or_event_is_refused(self):
        for field, value in (("head_sha", "b" * 40), ("head_branch", "topic"), ("event", "pull_request")):
            with self.subTest(field=field):
                old = self.client.runs["ci.yml"][0][field]
                self.client.runs["ci.yml"][0][field] = value
                self.refuse("workflow-run-binding-invalid")
                self.client.runs["ci.yml"][0][field] = old

    def test_failed_skipped_or_running_workflow_cannot_qualify(self):
        for status, conclusion in (("completed", "failure"), ("completed", "skipped"), ("in_progress", None)):
            with self.subTest(status=status, conclusion=conclusion):
                self.client.runs["pages.yml"][0].update(status=status, conclusion=conclusion)
                self.refuse("workflow-not-successful")

    def test_newer_in_progress_push_run_blocks_old_success(self):
        run = dict(self.client.runs["ci.yml"][0], id=999, run_number=11, status="in_progress", conclusion=None)
        self.client.runs["ci.yml"].append(run)
        self.refuse("workflow-not-successful")

    def test_missing_expected_artifact_is_refused(self):
        del self.client.artifacts["drmr-Linux-X64"]
        self.refuse("artifact-missing-or-duplicate")

    def test_duplicate_current_attempt_artifact_is_refused(self):
        self.client.extra_artifacts.append(dict(self.client.artifacts["drmr-Linux-X64"], id=999))
        self.refuse("artifact-missing-or-duplicate")

    def test_previous_attempt_artifact_does_not_poison_fresh_attempt(self):
        self.client.extra_artifacts.append(dict(self.client.artifacts["drmr-Linux-X64"], id=999, created_at="2026-10-21T23:00:00Z"))
        self.assertTrue(self.prepare()["success"])

    def test_expired_duplicate_does_not_poison_available_current_artifact(self):
        self.client.extra_artifacts.append(dict(self.client.artifacts["drmr-Linux-X64"], id=999, expired=True))
        self.assertTrue(self.prepare()["success"])

    def test_only_previous_attempt_artifact_is_refused(self):
        self.client.artifacts["drmr-Linux-X64"]["created_at"] = "2026-10-21T23:00:00Z"
        self.refuse("artifact-missing-or-duplicate")

    def test_expired_artifact_is_refused(self):
        self.client.artifacts["drmr-Linux-X64"]["expired"] = True
        self.refuse()

    def test_artifact_metadata_wrong_source_or_run_is_refused(self):
        self.client.artifacts["drmr-Linux-X64"]["workflow_run"]["head_sha"] = "b" * 40
        self.refuse("artifact-metadata-invalid")

    def test_outer_artifact_digest_mismatch_is_refused(self):
        self.client.artifacts["drmr-Linux-X64"]["digest"] = "sha256:" + "0" * 64
        self.refuse("artifact-download-digest-mismatch")

    def test_unknown_or_traversing_outer_paths_are_refused(self):
        for name in ("../escape", "unknown.txt", "folder/cli-archive-verification.json"):
            with self.subTest(name=name):
                original = list(self.client.entries["drmr-Linux-X64"])
                self.client.entries["drmr-Linux-X64"].append((name, b"fixture"))
                self.client.rebuild("drmr-Linux-X64")
                self.refuse()
                self.assertFalse((self.home / "escape").exists())
                self.client.entries["drmr-Linux-X64"] = original

    def test_duplicate_outer_file_is_refused(self):
        name = "drmr-Linux-X64"
        self.client.entries[name].append(self.client.entries[name][0])
        with self.assertWarns(UserWarning):
            self.client.rebuild(name)
        self.refuse("artifact-archive-invalid")

    def test_matching_digest_malformed_or_truncated_compression_is_refused(self):
        name = "drmr-Linux-X64"
        stream = io.BytesIO()
        with zipfile.ZipFile(stream, "w", compression=zipfile.ZIP_DEFLATED) as archive:
            for filename, data in self.client.entries[name]:
                archive.writestr(filename, data)
        damaged = bytearray(stream.getvalue())
        name_bytes = int.from_bytes(damaged[26:28], "little")
        extra_bytes = int.from_bytes(damaged[28:30], "little")
        damaged[30 + name_bytes + extra_bytes] = 0xff
        cases = (b"private fixture invalid archive", self.client.outer[name][:-22], bytes(damaged))
        for raw in cases:
            with self.subTest(size=len(raw)):
                self.client.outer[name] = raw
                self.client.artifacts[name].update(size_in_bytes=len(raw), digest="sha256:" + hashlib.sha256(raw).hexdigest())
                self.refuse("artifact-archive-invalid")

    def test_outer_symlink_is_refused(self):
        name = "drmr-Linux-X64"
        stream = io.BytesIO()
        with zipfile.ZipFile(stream, "w") as archive:
            for filename, data in self.client.entries[name]:
                member = zipfile.ZipInfo(filename)
                member.create_system = 3
                member.external_attr = (stat.S_IFLNK | 0o777) << 16
                archive.writestr(member, data)
        self.client.outer[name] = stream.getvalue()
        self.client.artifacts[name]["digest"] = "sha256:" + hashlib.sha256(stream.getvalue()).hexdigest()
        self.refuse("artifact-archive-invalid")

    def test_report_archive_hash_source_tree_and_target_are_bound(self):
        for field, value in (("archive_sha256", "0" * 64), ("source_head", "b" * 40), ("source_tree", "b" * 40), ("target", "other-target")):
            with self.subTest(field=field):
                original = list(self.client.entries["drmr-Linux-X64"])
                self.client.report("drmr-Linux-X64", "cli-archive-verification.json", lambda report: report.update({field: value}))
                self.refuse("report-binding-invalid")
                self.client.entries["drmr-Linux-X64"] = original
                self.client.rebuild("drmr-Linux-X64")

    def test_failed_or_wrong_version_media_proof_is_refused(self):
        for field, value in (("success", False), ("version", "0.9.0"), ("binary_reported_version", "0.9.0"), ("binary_sha256", "0" * 64)):
            with self.subTest(field=field):
                original = list(self.client.entries["drmr-Linux-X64"])
                self.client.report("drmr-Linux-X64", "cli-media-qualification.json", lambda report: report.update({field: value}))
                self.refuse()
                self.client.entries["drmr-Linux-X64"] = original
                self.client.rebuild("drmr-Linux-X64")

    def test_skipped_stage_or_failed_full_media_comparison_is_refused(self):
        self.client.report("drmr-Linux-X64", "cli-media-qualification.json", lambda report: report["stages"].pop())
        self.refuse("media-stages-invalid")

    def test_independent_verification_must_be_successful(self):
        self.client.report("drmr-Linux-X64", "cli-media-qualification.json", lambda report: report["media"]["independent_verification"]["checks"].update(packet_timing_equal=False))
        self.refuse("media-proof-invalid")

    def test_integer_cannot_substitute_for_boolean_media_proof(self):
        self.client.report("drmr-Linux-X64", "cli-media-qualification.json", lambda report: report["media"]["conversion"]["checks"].update(packet_payloads_equal=1))
        self.refuse("media-proof-invalid")

    def test_oversized_report_is_refused_before_parsing(self):
        name = "drmr-Linux-X64"
        self.client.entries[name] = [(filename, b"x" * (PREPARE.MAX_REPORT + 1) if filename == "cli-media-qualification.json" else data)
                                     for filename, data in self.client.entries[name]]
        self.client.rebuild(name)
        self.refuse("artifact-size-invalid")

    def test_unknown_report_fields_cannot_leak_to_release_assets(self):
        self.client.report("drmr-Linux-X64", "cli-media-qualification.json", lambda report: report.update(private_path="private fixture"))
        self.refuse("report-schema-invalid")

    def test_new_attempt_during_download_invalidates_the_packet(self):
        count = [0]
        def drift(endpoint):
            if "/git/ref/heads/main" in endpoint:
                count[0] += 1
                if count[0] == 2:
                    self.client.runs["ci.yml"][0]["run_attempt"] = 2
        self.client.on_json = drift
        self.refuse("qualification-runs-changed")

    def test_new_failed_push_during_download_invalidates_the_packet(self):
        count = [0]
        def drift(endpoint):
            if "/git/ref/heads/main" in endpoint:
                count[0] += 1
                if count[0] == 2:
                    self.client.runs["ci.yml"].append(dict(self.client.runs["ci.yml"][0], id=999, run_number=11, conclusion="failure"))
        self.client.on_json = drift
        self.refuse("workflow-not-successful")

    def test_existing_output_is_preserved(self):
        self.output.mkdir()
        (self.output / "keep.txt").write_text("keep")
        with self.assertRaisesRegex(PREPARE.Failure, "output-already-exists"):
            self.prepare()
        self.assertEqual((self.output / "keep.txt").read_text(), "keep")

    def test_output_inside_source_is_refused(self):
        with self.assertRaisesRegex(PREPARE.Failure, "output-must-be-outside-source"):
            PREPARE.prepare(self.root, "v1.0.0", self.root / "release", "example/drmr", self.client, native_inputs_only=True)

    def test_output_appearing_during_assembly_is_preserved(self):
        count = [0]
        def other_owner(endpoint):
            if "/git/ref/heads/main" in endpoint:
                count[0] += 1
                if count[0] == 2:
                    self.output.mkdir()
                    (self.output / "keep.txt").write_text("keep")
        self.client.on_json = other_owner
        with self.assertRaises(FileExistsError):
            self.prepare()
        self.assertEqual((self.output / "keep.txt").read_text(), "keep")
        self.assertEqual(list(self.home.glob(".drmr-release-*")), [])

    def test_tag_is_required_to_be_clean_and_exact(self):
        self.write("untracked.txt", "fixture")
        self.refuse("working-tree-dirty")

    def test_missing_workflow_is_refused(self):
        with self.assertRaisesRegex(PREPARE.Failure, "workflow-runs-missing"):
            PREPARE.latest_run([], {"id": 1, "path": ".github/workflows/ci.yml"}, "a" * 40, "example/drmr")

    def test_newer_failure_cannot_be_hidden_by_an_older_success(self):
        workflow = {"id": 1, "path": ".github/workflows/ci.yml"}
        run = {"id": 10, "run_number": 2, "run_attempt": 1, "workflow_id": 1,
               "path": workflow["path"], "event": "push", "head_branch": "main", "head_sha": "a" * 40,
               "repository": {"full_name": "example/drmr"}, "head_repository": {"full_name": "example/drmr"},
               "status": "completed", "conclusion": "success"}
        failed = dict(run, id=11, run_number=3, conclusion="failure")
        with self.assertRaisesRegex(PREPARE.Failure, "workflow-not-successful"):
            PREPARE.latest_run([run, failed], workflow, "a" * 40, "example/drmr")


class ReadOnlyGitHub(unittest.TestCase):
    def test_client_uses_only_get_on_fixed_github_host(self):
        calls = []
        def start(arguments, **kwargs):
            calls.append(arguments)
            kwargs["stdout"].write(b'{"fixture":true}')
            kwargs["stdout"].flush()
            return Mock(returncode=0, poll=Mock(return_value=0))
        with patch.dict(PREPARE.os.environ, {"GH_TOKEN": "fixture-token"}), patch.object(PREPARE.subprocess, "Popen", side_effect=start):
            self.assertEqual(PREPARE.GitHub().json("repos/example/drmr/git/ref/heads/main"), {"fixture": True})
        self.assertEqual(calls[0][:7], ["gh", "api", "--hostname", "github.com", "--method", "GET", "-H"])
        self.assertIn("Cache-Control: no-cache", calls[0])
        self.assertNotIn("fixture-token", " ".join(calls[0]))

    def test_client_bounds_the_response_before_json_parsing(self):
        def start(arguments, **kwargs):
            kwargs["stdout"].write(b"12345")
            kwargs["stdout"].flush()
            return Mock(returncode=0, poll=Mock(return_value=0))
        with tempfile.TemporaryDirectory() as directory, patch.dict(PREPARE.os.environ, {"GH_TOKEN": "fixture-token"}), patch.object(PREPARE.subprocess, "Popen", side_effect=start):
            with self.assertRaisesRegex(PREPARE.Failure, "github-response-too-large"):
                PREPARE.GitHub().request("repos/example/drmr", Path(directory) / "response", 4)

    def test_timed_out_api_process_is_killed_and_reaped(self):
        process = Mock(returncode=None, poll=Mock(return_value=None))
        with tempfile.TemporaryDirectory() as directory, patch.dict(PREPARE.os.environ, {"GH_TOKEN": "fixture-token"}), patch.object(PREPARE.subprocess, "Popen", return_value=process), patch.object(PREPARE.time, "monotonic", side_effect=[0, 181]):
            with self.assertRaisesRegex(PREPARE.Failure, "github-request-timeout"):
                PREPARE.GitHub().request("repos/example/drmr", Path(directory) / "response", 4)
        process.kill.assert_called_once_with()
        process.wait.assert_called_once_with(timeout=10)

    def test_main_sanitizes_archive_errors_without_echoing_operator_paths(self):
        for kind in (PREPARE.tarfile.ReadError, zipfile.BadZipFile, PREPARE.zlib.error, EOFError, NotImplementedError):
            with self.subTest(kind=kind.__name__):
                output = io.StringIO()
                with patch.object(PREPARE, "GitHub"), patch.object(PREPARE, "prepare", side_effect=kind("private fixture path")), contextlib.redirect_stdout(output):
                    status = PREPARE.main(["--tag", "v1.0.0", "--output", "private-fixture", "--native-inputs-only"])
                self.assertEqual(status, 1)
                self.assertEqual(json.loads(output.getvalue()), {"schema_version": 1, "success": False, "error": "artifact-archive-invalid", "publication_performed": False})

    def test_json_duplicate_and_nonfinite_values_are_refused(self):
        for value in (b'{"success":true,"success":false}', b'{"value":NaN}'):
            with self.subTest(value=value), self.assertRaises(PREPARE.CHECK.Failure):
                PREPARE.load_json(value)

    def test_incomplete_or_unbounded_lists_are_refused(self):
        client = Mock()
        for response in ({"total_count": 1001, "artifacts": []}, {"total_count": 2, "artifacts": [{}]}):
            with self.subTest(response=response), self.assertRaises(PREPARE.Failure):
                client.json.return_value = response
                PREPARE.rows(client, "repos/example/drmr/actions/runs/1/artifacts", "artifacts")


if __name__ == "__main__":
    unittest.main()
