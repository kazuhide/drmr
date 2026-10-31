#!/usr/bin/env python3
"""Assemble verified CLI release assets using read-only GitHub API requests.

Requires Python 3.9+, Cargo, Git, GitHub CLI, and GH_TOKEN with Actions/Contents
read access. This command never creates tags or releases, changes settings, or
executes downloaded binaries. It accepts only current-main push qualification
for the exact clean local release tag. All five latest runs and attempts must
succeed; the three CLI artifacts must contain matching archive and native-media
proof. Release packets additionally require complete native round-trip reports
for all targets. --native-inputs-only stages an explicitly unqualified packet
for those native jobs. Output is a new directory outside the source checkout.
"""
import argparse
import datetime
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import re
import shutil
import stat
import subprocess
import sys
import tarfile
import tempfile
import time
import zipfile
import zlib

# Importing a maintainer gate must not make its clean source checkout dirty.
sys.dont_write_bytecode = True
ROOT = Path(__file__).resolve().parents[1]


def module(name):
    specification = importlib.util.spec_from_file_location(name, ROOT / ("tools/" + name + ".py"))
    result = importlib.util.module_from_spec(specification)
    specification.loader.exec_module(result)
    return result


CHECK = module("check_release")
VERIFY = module("verify_archive")
EVIDENCE = module("media_evidence")
ROUNDTRIP = module("roundtrip_contract")
CI_CONTRACT = module("ci_contract")
COMPRESSED_ERRORS = (EOFError, tarfile.TarError, zipfile.BadZipFile, zlib.error, NotImplementedError)
ARCHIVE_ERRORS = (VERIFY.Failure, OSError, ValueError, TypeError, KeyError) + COMPRESSED_ERRORS
WORKFLOWS = ("ci.yml", "windows.yml", "container.yml", "advisories.yml", "pages.yml")
TARGETS = (
    ("ci.yml", "drmr-Linux-X64", "x86_64-unknown-linux-gnu", ".tar.gz"),
    ("ci.yml", "drmr-macOS-ARM64", "aarch64-apple-darwin", ".tar.gz"),
    ("windows.yml", "drmr-windows-x86_64", "x86_64-pc-windows-msvc", ".zip"),
)
MAX_JSON = 4 * 1024 * 1024
MAX_REPORT = 2 * 1024 * 1024
MAX_ROUNDTRIP_REPORT = 16 * 1024 * 1024
MAX_ARTIFACT = 512 * 1024 * 1024
MAX_ROWS = 1000
STAGES = ("version", "doctor", "fixture-probe", "fixture-video", "prepared-video-probe", "fixture-segments",
          "prepared-source-verification", "analyze", "plan", "convert", "independent-verify",
          "fixture-aac44100", "fixture-aac44100-probe", "aac44100-analyze", "aac44100-plan",
          "aac44100-convert", "aac44100-independent-verify", "metadata-analysis")
ARCHIVE_LIMITS = "Archive/source and optional native CLI-pair checks only; no GUI, media, signature, build-origin or legal certification."
MEDIA_LIMITS = (
    "One native packaged CLI, one ten-second video-only Clear DASH to CBCS DASH saved-plan route "
    "and one generated ten-second 44.1 kHz stereo AAC Clear HLS/TS to CBCS DASH saved-plan route. "
    "One local Clear DASH metadata-only analysis with the original preserved. "
    "Full original video comparison, complete generated audio frame/clock evidence and CLI media comparisons, "
    "and native manifest linting; no independent external audio PCM comparison, DRM license/device playback, "
    "GUI, installer, signature, or build-origin certification.")
FIXTURE_SHA256 = "a0ef6d20f2319c57af35d9c3165e46b669c67b951268cf882dbfff14b5b26e71"


class Failure(CHECK.Failure):
    """A fixed public diagnostic, without API bodies, paths, or tool output."""


def require(condition, code):
    if not condition:
        raise Failure(code)


def load_json(raw):
    def constant(_):
        raise Failure("json-invalid")
    try:
        return json.loads(raw, object_pairs_hook=CHECK.unique, parse_constant=constant)
    except (ValueError, TypeError, UnicodeError, RecursionError):
        raise Failure("json-invalid") from None


def digest(path):
    result = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(65536), b""):
            result.update(chunk)
    return result.hexdigest()


def positive(value):
    return type(value) is int and value > 0


def hexadecimal(value, length=64):
    return isinstance(value, str) and re.fullmatch("[0-9a-f]{" + str(length) + "}", value) is not None


def instant(value):
    require(isinstance(value, str) and re.fullmatch(r"[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}Z", value) is not None,
            "github-timestamp-invalid")
    try:
        return datetime.datetime.strptime(value, "%Y-%m-%dT%H:%M:%SZ")
    except ValueError:
        raise Failure("github-timestamp-invalid") from None


class GitHub:
    """Only fixed-host GET requests; stdout is bounded and stderr discarded."""
    def __init__(self):
        require(bool(os.environ.get("GH_TOKEN")), "github-token-required")

    def request(self, endpoint, destination, maximum):
        arguments = ["gh", "api", "--hostname", "github.com", "--method", "GET",
                     "-H", "Accept: application/vnd.github+json", "-H", "X-GitHub-Api-Version: 2022-11-28",
                     "-H", "Cache-Control: no-cache", endpoint]
        environment = dict(os.environ, GH_PROMPT_DISABLED="1", GH_NO_UPDATE_NOTIFIER="1")
        process = None
        try:
            with destination.open("xb") as output:
                process = subprocess.Popen(arguments, stdin=subprocess.DEVNULL, stdout=output,
                                           stderr=subprocess.DEVNULL, env=environment)
                deadline = time.monotonic() + 180
                while process.poll() is None:
                    require(os.fstat(output.fileno()).st_size <= maximum, "github-response-too-large")
                    require(time.monotonic() < deadline, "github-request-timeout")
                    try:
                        process.wait(timeout=0.1)
                    except subprocess.TimeoutExpired:
                        pass
                require(process.returncode == 0, "github-request-failed")
                require(0 < os.fstat(output.fileno()).st_size <= maximum, "github-response-too-large")
        except OSError:
            raise Failure("github-request-failed") from None
        finally:
            if process is not None and process.poll() is None:
                process.kill()
                try:
                    process.wait(timeout=10)
                except subprocess.TimeoutExpired:
                    raise Failure("github-process-cleanup-unconfirmed") from None

    def json(self, endpoint):
        with tempfile.TemporaryDirectory(prefix="drmr-release-api-") as directory:
            response = Path(directory) / "response"
            self.request(endpoint, response, MAX_JSON)
            return load_json(response.read_bytes())

    def download(self, endpoint, destination):
        self.request(endpoint, destination, MAX_ARTIFACT)


def rows(client, endpoint, key):
    result = []
    total = None
    for page in range(1, 11):
        response = client.json(endpoint + ("&" if "?" in endpoint else "?") + "per_page=100&page=" + str(page))
        require(isinstance(response, dict) and type(response.get("total_count")) is int
                and 0 <= response["total_count"] <= MAX_ROWS and isinstance(response.get(key), list), "github-list-invalid")
        if total is None:
            total = response["total_count"]
        require(response["total_count"] == total, "github-list-changed")
        result.extend(response[key])
        require(len(result) <= total and len(response[key]) <= 100, "github-list-invalid")
        if len(result) == total:
            return result
        require(len(response[key]) == 100, "github-list-incomplete")
    raise Failure("github-list-incomplete")


def run_binding(run, workflow, head, repository):
    require(isinstance(run, dict) and all(positive(run.get(name)) for name in ("id", "run_number", "run_attempt"))
            and run.get("workflow_id") == workflow["id"] and run.get("path") == workflow["path"]
            and run.get("head_sha") == head and run.get("head_branch") == "main" and run.get("event") == "push"
            and run.get("repository", {}).get("full_name", "").lower() == repository.lower()
            and run.get("head_repository", {}).get("full_name", "").lower() == repository.lower(), "workflow-run-binding-invalid")


def latest_run(runs, workflow, head, repository):
    require(bool(runs), "workflow-runs-missing")
    for run in runs:
        run_binding(run, workflow, head, repository)
    require(len({run["id"] for run in runs}) == len(runs)
            and len({run["run_number"] for run in runs}) == len(runs), "workflow-runs-duplicate")
    selected = max(runs, key=lambda run: (run["run_number"], run["id"]))
    require(selected.get("status") == "completed" and selected.get("conclusion") == "success", "workflow-not-successful")
    return selected


def pin(run):
    return {"workflow_id": run["workflow_id"], "workflow_path": run["path"], "run_id": run["id"],
            "run_number": run["run_number"], "run_attempt": run["run_attempt"],
            "event": "push", "branch": "main", "status": "completed", "conclusion": "success"}


def qualification_runs(client, repository, head):
    base = "repos/" + repository
    main = client.json(base + "/git/ref/heads/main")
    require(main.get("ref") == "refs/heads/main" and main.get("object", {}).get("type") == "commit"
            and main["object"].get("sha") == head, "remote-main-mismatch")
    selected = {}
    for name in WORKFLOWS:
        workflow = client.json(base + "/actions/workflows/" + name)
        require(positive(workflow.get("id")) and workflow.get("path") == ".github/workflows/" + name
                and workflow.get("state") == "active", "workflow-identity-invalid")
        listed = rows(client, base + "/actions/workflows/" + str(workflow["id"]) + "/runs?branch=main&event=push&head_sha=" + head,
                      "workflow_runs")
        run = latest_run(listed, workflow, head, repository)
        attempt = client.json(base + "/actions/runs/" + str(run["id"]) + "/attempts/" + str(run["run_attempt"]))
        run_binding(attempt, workflow, head, repository)
        require(pin(run) == pin(attempt) and attempt.get("status") == "completed"
                and attempt.get("conclusion") == "success", "workflow-attempt-changed")
        require(instant(attempt["run_started_at"]) <= instant(attempt["updated_at"]), "workflow-attempt-window-invalid")
        jobs = rows(client, base + "/actions/runs/" + str(run["id"]) + "/attempts/"
                    + str(run["run_attempt"]) + "/jobs", "jobs")
        try:
            CI_CONTRACT.validate_jobs(name, jobs, head, run["id"], run["run_attempt"], release=True)
        except CI_CONTRACT.Refusal:
            raise Failure("workflow-native-jobs-incomplete") from None
        selected[name] = attempt
    return selected


def select_artifact(artifacts, name, run, head):
    start, end = instant(run["run_started_at"]), instant(run["updated_at"])
    candidates = []
    for artifact in artifacts:
        require(isinstance(artifact, dict), "artifact-metadata-invalid")
        if artifact.get("name") != name:
            continue
        created = instant(artifact.get("created_at"))
        if start <= created <= end and artifact.get("expired") is False:
            candidates.append(artifact)
    require(len(candidates) == 1, "artifact-missing-or-duplicate")
    artifact = candidates[0]
    binding = artifact.get("workflow_run", {})
    require(positive(artifact.get("id")) and artifact.get("expired") is False
            and type(artifact.get("size_in_bytes")) is int and 0 < artifact["size_in_bytes"] <= MAX_ARTIFACT
            and binding.get("id") == run["id"] and binding.get("head_sha") == head and binding.get("head_branch") == "main"
            and isinstance(artifact.get("digest"), str) and re.fullmatch(r"sha256:[0-9a-f]{64}", artifact["digest"]) is not None,
            "artifact-metadata-invalid")
    return artifact


def exact_keys(value, wanted):
    require(isinstance(value, dict) and set(value) == set(wanted), "report-schema-invalid")


def proof(value):
    if isinstance(value, dict) and value.get("schema_version") == 2:
        exact_keys(value, ("schema_version", "required_depth", "scope", "passed", "checks", "decode_evidence", "license_playback_verified", "native_manifest"))
        require(EVIDENCE.v2_proof(value, require_decoded=True) is not None, "media-proof-invalid")
        require(value["native_manifest"]["profile"] == "dash-static-manifest"
                and value["native_manifest"]["document_count"] == 1
                and 0 <= value["native_manifest"]["warnings"] <= 1000000, "media-proof-invalid")
        return
    exact_keys(value, ("media_verified", "license_playback_verified", "checks", "native_manifest"))
    require(value["media_verified"] is True and value["license_playback_verified"] is False, "media-proof-invalid")
    checks = value["checks"]
    exact_keys(checks, ("stream_metadata", "packet_payloads_equal", "packet_timing_equal", "decoded_content_equal"))
    require(checks["stream_metadata"] == "pass" and all(checks[name] is True for name in
            ("packet_payloads_equal", "packet_timing_equal", "decoded_content_equal")), "media-proof-invalid")
    native = value["native_manifest"]
    exact_keys(native, ("schema_version", "scope", "profile", "errors", "warnings", "truncated", "incomplete", "document_count"))
    require(type(native["schema_version"]) is int and native["schema_version"] == 1 and native["scope"] == "manifest-only"
            and native["profile"] == "dash-static-manifest" and type(native["errors"]) is int and native["errors"] == 0
            and type(native["warnings"]) is int and 0 <= native["warnings"] <= 1000000
            and native["truncated"] is False and native["incomplete"] is False
            and type(native["document_count"]) is int and native["document_count"] == 1, "media-proof-invalid")


def graph(value):
    exact_keys(value, ("sha256", "files", "bytes"))
    require(hexadecimal(value["sha256"]) and positive(value["files"]) and value["files"] <= 64
            and positive(value["bytes"]) and value["bytes"] <= 64 * 1024 * 1024, "media-proof-invalid")


def aac44100(value):
    exact_keys(value, ("fixture_sha256", "fixture_duration_seconds", "scope", "input_format", "input_protection",
                       "output_format", "output_protection", "segment_target_seconds", "input_segments", "source_audio",
                       "source_graph", "cli_source_graph_sha256", "plan_schema_version", "plan_id", "plan_sha256",
                       "saved_original_and_plan_unchanged", "conversion", "independent_verification", "output_graph"))
    require(value["fixture_sha256"] == FIXTURE_SHA256 and type(value["fixture_duration_seconds"]) is int
            and value["fixture_duration_seconds"] == 10
            and value["scope"] == "all generated 44.1 kHz AAC frames; fixture audio resampled from the attributed clip"
            and value["input_format"] == "hls-ts" and value["input_protection"] == "clear"
            and value["output_format"] == "dash" and value["output_protection"] == "cbcs"
            and type(value["segment_target_seconds"]) is int and value["segment_target_seconds"] == 2
            and type(value["input_segments"]) is int and 2 <= value["input_segments"] <= 8
            and type(value["plan_schema_version"]) is int and value["plan_schema_version"] in (1, 3, 5, 7, 9)
            and all(hexadecimal(value[name]) for name in ("cli_source_graph_sha256", "plan_id", "plan_sha256"))
            and value["saved_original_and_plan_unchanged"] is True, "aac44100-proof-invalid")
    audio = value["source_audio"]
    exact_keys(audio, ("codec_name", "profile", "sample_rate", "channels", "time_base", "frame_count", "packet_count",
                       "expected_decoded_samples_per_channel", "first_pts", "first_dts", "last_pts", "last_dts",
                       "last_duration", "packet_clock_sha256"))
    require(audio["codec_name"] == "aac" and audio["profile"] == "LC"
            and type(audio["sample_rate"]) is int and audio["sample_rate"] == 44100
            and type(audio["channels"]) is int and audio["channels"] == 2 and audio["time_base"] == "1/90000"
            and all(type(audio[name]) is int and audio[name] == 432 for name in ("frame_count", "packet_count"))
            and type(audio["expected_decoded_samples_per_channel"]) is int
            and audio["expected_decoded_samples_per_channel"] == 432 * 1024
            and all(type(audio[name]) is int and 0 <= audio[name] < 1 << 33
                    for name in ("first_pts", "first_dts", "last_pts", "last_dts"))
            and audio["first_pts"] == audio["first_dts"] < audio["last_pts"] == audio["last_dts"]
            and type(audio["last_duration"]) is int and audio["last_duration"] == 2089
            and hexadecimal(audio["packet_clock_sha256"]), "aac44100-proof-invalid")
    graph(value["source_graph"])
    graph(value["output_graph"])
    proof(value["conversion"])
    proof(value["independent_verification"])


def reports(archive_proof, media, head, tree, target, version, archive_hash):
    exact_keys(archive_proof, ("success", "source_head", "source_tree", "target", "kind", "archive_sha256", "files", "packages",
                               "binaries", "pair_check", "raw_notice_files", "limits"))
    for report in (archive_proof, media):
        require(isinstance(report, dict) and report.get("success") is True and report.get("source_head") == head
                and report.get("source_tree") == tree and report.get("target") == target
                and report.get("archive_sha256") == archive_hash, "report-binding-invalid")
    require(archive_proof["kind"] == "cli" and archive_proof["pair_check"] == "not-applicable"
            and positive(archive_proof["files"]) and archive_proof["files"] <= 10000
            and positive(archive_proof["packages"]) and archive_proof["packages"] <= 10000
            and type(archive_proof["raw_notice_files"]) is int and 0 <= archive_proof["raw_notice_files"] <= archive_proof["files"]
            and archive_proof["limits"] == ARCHIVE_LIMITS, "archive-proof-invalid")
    exact_keys(archive_proof["binaries"], ("drmr",))
    binary = archive_proof["binaries"]["drmr"]
    exact_keys(binary, ("sha256", "bytes", "mode", "header_target"))
    require(hexadecimal(binary["sha256"]) and positive(binary["bytes"]) and binary["bytes"] <= MAX_ARTIFACT
            and binary["header_target"] == target and isinstance(binary["mode"], str)
            and re.fullmatch(r"0o[0-7]{1,3}", binary["mode"]) is not None, "archive-proof-invalid")
    exact_keys(media, ("schema_version", "success", "kind", "source_head", "source_tree", "target", "version", "binary_reported_version",
                       "archive_sha256", "binary_sha256", "archive_files", "dependency_packages", "native_ordinary_user", "doctor", "media", "aac_44100",
                       "metadata_analysis", "stages", "owned_workspace_cleanup", "limits"))
    require(type(media["schema_version"]) is int and media["schema_version"] == 1 and media["kind"] == "native-cli-archive-media"
            and media["version"] == version and media["binary_reported_version"] == version and media["binary_sha256"] == binary["sha256"]
            and media["archive_files"] == archive_proof["files"] and media["dependency_packages"] == archive_proof["packages"]
            and media["native_ordinary_user"] is True and media["owned_workspace_cleanup"] == "confirmed"
            and media["limits"] == MEDIA_LIMITS, "media-proof-invalid")
    doctor = media["doctor"]
    exact_keys(doctor, ("ready", "cleanup_status", "tools"))
    expected_tools = {"ffmpeg", "ffprobe", "packager"}
    require(doctor["ready"] is True and doctor["cleanup_status"] == "confirmed" and isinstance(doctor["tools"], list)
            and len(doctor["tools"]) == len(expected_tools), "doctor-proof-invalid")
    tool_names = set()
    for tool in doctor["tools"]:
        exact_keys(tool, ("name", "available", "required", "version_status", "feature_status"))
        require(tool["name"] in expected_tools
                and tool["available"] is True and tool["required"] is True
                and tool["version_status"] == "supported" and tool["feature_status"] == "available", "doctor-proof-invalid")
        tool_names.add(tool["name"])
    require(tool_names == expected_tools, "doctor-proof-invalid")
    value = media["media"]
    exact_keys(value, ("fixture_sha256", "fixture_duration_seconds", "scope", "input_format", "input_protection", "output_format", "output_protection",
                       "segment_target_seconds", "input_segments", "source_graph", "cli_source_graph_sha256", "plan_schema_version", "plan_id",
                       "plan_sha256", "saved_original_and_plan_unchanged", "prepared_source_against_original_video", "conversion",
                       "independent_verification", "output_graph"))
    require(value["fixture_sha256"] == FIXTURE_SHA256 and type(value["fixture_duration_seconds"]) is int and value["fixture_duration_seconds"] == 10
            and value["scope"] == "all 240 original H.264 video packets and frames; audio not selected"
            and value["input_format"] == "dash" and value["input_protection"] == "clear"
            and value["output_format"] == "dash" and value["output_protection"] == "cbcs"
            and type(value["segment_target_seconds"]) is int and value["segment_target_seconds"] == 2
            and type(value["input_segments"]) is int and value["input_segments"] == 5
            and type(value["plan_schema_version"]) is int and value["plan_schema_version"] in (1, 3, 5, 7, 9)
            and all(hexadecimal(value[name]) for name in ("cli_source_graph_sha256", "plan_id", "plan_sha256"))
            and value["saved_original_and_plan_unchanged"] is True, "media-proof-invalid")
    graph(value["source_graph"])
    graph(value["output_graph"])
    for name in ("prepared_source_against_original_video", "conversion", "independent_verification"):
        proof(value[name])
    aac44100(media["aac_44100"])
    metadata = media["metadata_analysis"]
    exact_keys(metadata, ("schema_version", "format", "protection", "evidence", "media_verified", "source_unchanged"))
    require(type(metadata["schema_version"]) is int and metadata["schema_version"] == 1
            and metadata["format"] == "dash" and metadata["protection"] == "clear"
            and metadata["evidence"] == "manifest" and metadata["media_verified"] is False
            and metadata["source_unchanged"] is True, "metadata-proof-invalid")
    require(isinstance(media["stages"], list) and len(media["stages"]) == len(STAGES), "media-stages-invalid")
    for stage, expected in zip(media["stages"], STAGES):
        exact_keys(stage, ("stage", "elapsed_milliseconds", "exit_code"))
        require(stage["stage"] == expected and type(stage["elapsed_milliseconds"]) is int
                and 0 <= stage["elapsed_milliseconds"] <= 86400000 and type(stage["exit_code"]) is int
                and stage["exit_code"] == 0, "media-stages-invalid")
    return binary["sha256"]


class OwnedDirectory:
    def __init__(self, path=None, parent=None):
        if path is None:
            self.path = Path(tempfile.mkdtemp(prefix=".drmr-release-", dir=parent))
        else:
            self.path = path
            self.path.mkdir(mode=0o700)
        metadata = self.path.lstat()
        self.identity = (metadata.st_dev, metadata.st_ino)

    def close(self):
        metadata = self.path.lstat()
        require(stat.S_ISDIR(metadata.st_mode) and not self.path.is_symlink()
                and not getattr(metadata, "st_file_attributes", 0) & 0x400
                and (metadata.st_dev, metadata.st_ino) == self.identity, "owned-cleanup-unconfirmed")
        shutil.rmtree(self.path)


def write_json(path, value):
    with path.open("x", encoding="utf-8") as stream:
        json.dump(value, stream, indent=2, sort_keys=True)
        stream.write("\n")


def unpack_artifact(outer, original_archive, destination):
    parsed = None
    try:
        parsed = VERIFY.Archive(outer)
        require(parsed.zip and not parsed.directories
                and set(parsed.files) == {original_archive, "cli-archive-verification.json", "cli-media-qualification.json"}, "artifact-file-set-invalid")
        require(sum(entry[1] for entry in parsed.files.values()) <= MAX_ARTIFACT
                and parsed.files[original_archive][1] > 0, "artifact-size-invalid")
        for name in ("cli-archive-verification.json", "cli-media-qualification.json"):
            require(parsed.files[name][1] <= MAX_REPORT, "artifact-size-invalid")
        with parsed.open(original_archive) as source, destination.open("xb") as output:
            count = 0
            while True:
                chunk = source.read(65536)
                if not chunk:
                    break
                count += len(chunk)
                require(count <= parsed.files[original_archive][1], "artifact-size-invalid")
                output.write(chunk)
        require(count == parsed.files[original_archive][1], "artifact-size-invalid")
        return (load_json(parsed.data("cli-archive-verification.json", MAX_REPORT)),
                load_json(parsed.data("cli-media-qualification.json", MAX_REPORT)))
    except ARCHIVE_ERRORS:
        raise Failure("artifact-archive-invalid") from None
    finally:
        if parsed is not None:
            parsed.close()


def release_notes(root, version):
    text = CHECK.read(root / "CHANGELOG.md")
    match = re.search(r"(?m)^## \[" + re.escape(version) + r"\] - [0-9]{4}-[0-9]{2}-[0-9]{2}[ \t]*$", text)
    require(match is not None, "release-notes-missing")
    following = text[match.end():]
    boundary = re.search(r"(?m)^##[ \t]", following)
    notes = (following[:boundary.start()] if boundary else following).strip()
    require(bool(notes), "release-notes-empty")
    return notes + "\n"


def prepare(root, tag, output, repository, client, roundtrip_reports=None, native_inputs_only=False):
    require((roundtrip_reports is not None) != bool(native_inputs_only), "roundtrip-reports-required")
    require(isinstance(repository, str) and re.fullmatch(r"[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+", repository) is not None
            and all(part not in (".", "..") for part in repository.split("/")), "repository-invalid")
    root = Path(root).resolve()
    output = Path(output).absolute()
    require(not output.exists() and not output.is_symlink(), "output-already-exists")
    require(output.parent.is_dir() and not output.parent.is_symlink(), "output-parent-invalid")
    output = output.parent.resolve() / output.name
    require(output != root and root not in output.parents, "output-must-be-outside-source")
    identity = CHECK.check_release(root, tag=tag, require_tag=True)
    head = identity["commit"]
    tree = CHECK.run(["git", "rev-parse", "HEAD^{tree}"], cwd=root, code="source-command-failed").decode("ascii").strip()
    require(hexadecimal(head, 40) and hexadecimal(tree, 40), "source-identity-invalid")
    selected = qualification_runs(client, repository, head)
    temporary = OwnedDirectory(parent=output.parent)
    promoted = None
    success = False
    try:
        payload = temporary.path / "payload"
        payload.mkdir(mode=0o700)
        inventory = []
        artifact_rows = {}
        base = "repos/" + repository
        for workflow_name, artifact_name, target, suffix in TARGETS:
            run = selected[workflow_name]
            if workflow_name not in artifact_rows:
                artifact_rows[workflow_name] = rows(client, base + "/actions/runs/" + str(run["id"]) + "/artifacts", "artifacts")
            artifact = select_artifact(artifact_rows[workflow_name], artifact_name, run, head)
            outer = temporary.path / (artifact_name + ".artifact.zip")
            client.download(base + "/actions/artifacts/" + str(artifact["id"]) + "/zip", outer)
            require(0 < outer.stat().st_size <= MAX_ARTIFACT and digest(outer) == artifact["digest"][7:], "artifact-download-digest-mismatch")
            filename = "drmr-" + identity["version"] + "-" + target + suffix
            archive = payload / filename
            archive_proof, media = unpack_artifact(outer, filename, archive)
            archive_hash = digest(archive)
            binary_hash = reports(archive_proof, media, head, tree, target, identity["version"], archive_hash)
            archive_report = "cli-archive-verification-" + target + ".json"
            media_report = "cli-media-qualification-" + target + ".json"
            write_json(payload / archive_report, archive_proof)
            write_json(payload / media_report, media)
            entry = {"filename": filename, "target": target, "sha256": archive_hash, "bytes": archive.stat().st_size,
                              "binary_sha256": binary_hash, "workflow_run_id": run["id"], "run_attempt": run["run_attempt"],
                              "artifact_id": artifact["id"], "artifact_sha256": artifact["digest"][7:],
                              "archive_report": {"filename": archive_report, "sha256": digest(payload / archive_report)},
                              "media_report": {"filename": media_report, "sha256": digest(payload / media_report)}}
            if not native_inputs_only:
                report_name = "cli-release-roundtrip-" + target + ".json"
                path = Path(roundtrip_reports) / report_name
                require(not path.is_symlink() and path.is_file() and 0 < path.stat().st_size <= MAX_ROUNDTRIP_REPORT,
                        "roundtrip-report-missing-or-invalid")
                raw = path.read_bytes()
                require(len(raw) <= MAX_ROUNDTRIP_REPORT, "roundtrip-report-missing-or-invalid")
                roundtrip = load_json(raw)
                try:
                    ROUNDTRIP.validate(roundtrip, head, tree, target, identity["version"], archive_hash, binary_hash)
                except ROUNDTRIP.Failure as error:
                    raise Failure(str(error)) from None
                write_json(payload / report_name, roundtrip)
                entry["roundtrip_report"] = {"filename": report_name, "sha256": digest(payload / report_name)}
            inventory.append(entry)
        notes = release_notes(root, identity["version"])
        manifest = {"schema_version": 1, "kind": "native-cli-roundtrip-inputs" if native_inputs_only else "verified-cli-release",
                    "publication_ready": not native_inputs_only, "repository": repository, "tag": tag,
                    "version": identity["version"], "source_head": head, "source_tree": tree,
                    "workflow_runs": [pin(selected[name]) for name in WORKFLOWS], "assets": inventory}
        write_json(payload / "release-manifest.json", manifest)
        with (payload / "release-notes.md").open("x", encoding="utf-8") as stream:
            stream.write(notes)
        with (payload / "SHA256SUMS").open("x", encoding="utf-8") as stream:
            for path in sorted(payload.iterdir()):
                if path.name not in ("SHA256SUMS", "release-notes.md"):
                    stream.write(digest(path) + "  " + path.name + "\n")
        # Re-select latest push runs, not just the old successful IDs. A new run
        # or attempt invalidates this packet even when the old run still exists.
        current = qualification_runs(client, repository, head)
        require([pin(current[name]) for name in WORKFLOWS] == manifest["workflow_runs"], "qualification-runs-changed")
        require(CHECK.check_release(root, tag=tag, require_tag=True) == identity, "source-identity-changed")
        promoted = OwnedDirectory(path=output)
        for path in sorted(payload.iterdir(), key=lambda path: (path.name == "release-manifest.json", path.name)):
            path.rename(promoted.path / path.name)
        success = True
        return {"schema_version": 1, "success": True, "tag": tag, "version": identity["version"],
                "source_head": head, "source_tree": tree, "archives": len(inventory),
                "publication_ready": not native_inputs_only, "publication_performed": False}
    finally:
        try:
            if promoted is not None and not success:
                promoted.close()
        finally:
            temporary.close()


class Parser(argparse.ArgumentParser):
    def error(self, _):
        raise Failure("arguments-invalid")


def main(argv=None):
    parser = Parser(description=__doc__)
    parser.add_argument("--tag", required=True, help="Exact final local release tag")
    parser.add_argument("--output", type=Path, required=True, help="Create a new release packet outside the checkout")
    parser.add_argument("--root", type=Path, default=ROOT)
    parser.add_argument("--repository", default="kazuhide/drmr")
    qualification = parser.add_mutually_exclusive_group(required=True)
    qualification.add_argument("--native-inputs-only", action="store_true", help="Stage CI archives for native round-trip jobs; not a release")
    qualification.add_argument("--roundtrip-reports", type=Path, help="Require complete native round-trip reports for all targets")
    try:
        args = parser.parse_args(argv)
        report = prepare(args.root, args.tag, args.output, args.repository, GitHub(),
                         roundtrip_reports=args.roundtrip_reports, native_inputs_only=args.native_inputs_only)
    except CHECK.Failure as error:
        report = {"schema_version": 1, "success": False, "error": str(error), "publication_performed": False}
    except COMPRESSED_ERRORS:
        report = {"schema_version": 1, "success": False, "error": "artifact-archive-invalid", "publication_performed": False}
    except (OSError, ValueError, TypeError, KeyError, AttributeError, subprocess.SubprocessError):
        report = {"schema_version": 1, "success": False, "error": "release-input-invalid", "publication_performed": False}
    except KeyboardInterrupt:
        report = {"schema_version": 1, "success": False, "error": "release-interrupted", "publication_performed": False}
    print(json.dumps(report, sort_keys=True))
    return 0 if report["success"] else 1


if __name__ == "__main__":
    sys.exit(main())
