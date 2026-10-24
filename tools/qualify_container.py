#!/usr/bin/env python3
"""Qualify one immutable, native drmr image without publishing it or changing host inputs."""

import argparse
from contextlib import ExitStack
import hashlib
import importlib.util
import http.client
import http.server
import json
import os
from pathlib import Path, PurePosixPath
import re
import shutil
import socket
import stat
import struct
import subprocess
import tempfile
import threading
import time
import uuid


ROOT = Path(__file__).resolve().parents[1]
EVIDENCE_SPEC = importlib.util.spec_from_file_location("media_evidence", ROOT / "tools/media_evidence.py")
EVIDENCE = importlib.util.module_from_spec(EVIDENCE_SPEC)
EVIDENCE_SPEC.loader.exec_module(EVIDENCE)
FIXTURE = ROOT / "crates/drmr/tests/fixtures/bbb/clip.mp4"
CLIP_SHA256 = "a0ef6d20f2319c57af35d9c3165e46b669c67b951268cf882dbfff14b5b26e71"
KEY = "40404040404040404040404040404040"
KID = "303132333435363738393a3b3c3d3e3f"
MAX_OUTPUT = 8 * 1024 * 1024


class Refusal(RuntimeError):
    """Fixed diagnostic: raw arguments, keys and tool output never enter an exception."""

    def __init__(self, code, evidence=None):
        super().__init__(code)
        self.evidence = evidence


def load_json(raw):
    def unique(pairs):
        result = {}
        for key, value in pairs:
            if key in result:
                raise Refusal("json-invalid")
            result[key] = value
        return result

    def invalid_constant(_):
        raise Refusal("json-invalid")

    try:
        return json.loads(raw, object_pairs_hook=unique, parse_constant=invalid_constant)
    except (ValueError, TypeError, UnicodeError, RecursionError):
        raise Refusal("json-invalid") from None


def require_verification(report):
    if isinstance(report, dict) and report.get("schema_version") == 2:
        if EVIDENCE.v2_proof(report, require_decoded=True) is None:
            raise Refusal("media-proof-invalid")
        return
    if not isinstance(report, dict):
        raise Refusal("media-proof-invalid")
    checks = report.get("checks")
    if (report.get("passed") is not True or report.get("media_verified") is not True
            or report.get("license_playback_verified") is not False
            or not isinstance(checks, dict) or checks.get("stream_metadata") != "pass"
            or any(checks.get(field) is not True for field in (
                "packet_payloads_equal", "packet_timing_equal", "decoded_content_equal"))):
        raise Refusal("media-proof-invalid")


def require_video(evidence):
    streams = evidence.get("streams", [])
    packets = evidence.get("packets", [])
    if (len(streams) != 1 or streams[0].get("codec_name") != "h264"
            or not re.fullmatch(r"1/[1-9][0-9]*", streams[0].get("time_base", ""))
            or streams[0].get("nb_read_frames") != "240" or len(packets) != 240):
        raise Refusal("video-proof-invalid")
    for packet in packets:
        if (any(type(packet.get(field)) is not int for field in ("pts", "dts", "duration"))
                or packet["duration"] <= 0
                or not re.fullmatch(r"SHA256:[0-9a-f]{64}", packet.get("data_hash", ""))):
            raise Refusal("video-proof-invalid")


def require_native(daemon, image):
    aliases = {"arm64": "arm64", "aarch64": "arm64", "amd64": "amd64", "x86_64": "amd64"}
    if daemon not in aliases or image not in aliases or aliases[daemon] != aliases[image]:
        raise Refusal("native-architecture-required")


def require_failure(status, stdout, code, exit_code):
    report = load_json(stdout)
    if (status != exit_code or not isinstance(report, dict)
            or report.get("schema_version") != 1 or report.get("command") != "convert"
            or report.get("status") != "failed" or not isinstance(report.get("error"), dict)
            or report["error"].get("code") != code
            or not isinstance(report["error"].get("hint"), str)
            or not report["error"]["hint"] or "last_activity" not in report["error"]):
        raise Refusal("failure-proof-invalid")
    return {"exit_code": status, "code": code}


def require_stop(report):
    fields = ("success", "driver_still_alive", "independent_tool_group", "immediate_lock_released",
              "tool_absent", "descendant_absent")
    if (not isinstance(report, dict) or any(report.get(field) is not True for field in fields)
            or report.get("kind") != "sigterm-cli-return" or report.get("exit_code") != 130
            or type(report.get("elapsed_milliseconds")) is not int
            or not 0 <= report["elapsed_milliseconds"] < 5000):
        raise Refusal("stop-proof-invalid")


def sha256(path):
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def hashes(root):
    return {path.relative_to(root).as_posix(): sha256(path)
            for path in sorted(root.rglob("*")) if path.is_file()}


def run(arguments, timeout=300):
    # Anonymous spools avoid PIPE deadlocks. Limits are checked every 10ms, so
    # writes between checks can exceed the cap; oversized output is never parsed.
    deadline = time.monotonic() + timeout
    with tempfile.TemporaryFile() as out, tempfile.TemporaryFile() as err:
        try:
            process = subprocess.Popen(arguments, stdin=subprocess.DEVNULL, stdout=out, stderr=err)
        except OSError:
            raise Refusal("command-unavailable") from None
        try:
            while True:
                if any(os.fstat(stream.fileno()).st_size > MAX_OUTPUT for stream in (out, err)):
                    raise Refusal("command-output-limit")
                status = process.poll()
                if status is not None:
                    break
                if time.monotonic() >= deadline:
                    raise Refusal("command-deadline")
                time.sleep(min(0.01, max(0, deadline - time.monotonic())))
        except BaseException:
            process.kill()
            try:
                process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                raise Refusal("command-client-reap-unconfirmed") from None
            raise
        out.seek(0)
        err.seek(0)
        return status, out.read(MAX_OUTPUT + 1), err.read(MAX_OUTPUT + 1)


def successful(arguments, timeout=300):
    status, stdout, _ = run(arguments, timeout)
    if status != 0:
        raise Refusal("command-failed")
    return stdout


class Image:
    def __init__(self, reference, workspace, qualifiers):
        info = load_json(successful(["docker", "image", "inspect", reference]))[0]
        self.identifier = info["Id"]
        if not re.fullmatch(r"sha256:[0-9a-f]{64}", self.identifier):
            raise Refusal("image-id-invalid")
        daemon = load_json(successful(["docker", "info", "--format", "{{json .Architecture}} "]))
        require_native(daemon, info["Architecture"])
        self.architecture = info["Architecture"]
        self.info = info
        self.workspace = workspace.resolve()
        self.qualifiers = qualifiers.resolve()
        self.names = []
        self.uid = os.getuid()
        self.gid = os.getgid()
        self.last_failure = None
        if self.uid == 0:
            raise Refusal("nonroot-host-user-required")

    def command(self, entry, arguments, network="none", readonly=False, limited=False, empty_path=False):
        name = "drmr-qualify-" + uuid.uuid4().hex
        self.names.append(name)
        command = ["docker", "run", "--rm", "--init", "--name", name,
                   "--label", "io.drmr.qualification=owned", "--network", network,
                   "--user", f"{self.uid}:{self.gid}",
                   "--mount", f"type=bind,source={self.workspace / 'input'},target=/input,readonly",
                   "--mount", f"type=bind,source={self.workspace / 'keys'},target=/keys,readonly",
                   "--mount", f"type=bind,source={self.workspace / 'work'},target=/work" + (",readonly" if readonly else ""),
                   "--mount", f"type=bind,source={self.qualifiers},target=/qualifiers,readonly",
                   "--entrypoint", entry, self.identifier]
        if limited:
            command[2:2] = ["--memory", "1g", "--cpus", "2"]
        if empty_path:
            command[2:2] = ["--env", "PATH=/no-installed-tools"]
        return command + [str(arg) for arg in arguments]

    def invoke(self, arguments, entry="/usr/local/bin/drmr", **options):
        return run(self.command(entry, arguments, **options))

    def checked(self, arguments, entry="/usr/local/bin/drmr", **options):
        status, stdout, stderr = self.invoke(arguments, entry, **options)
        if KEY.encode() in stdout + stderr:
            raise Refusal("secret-displayed")
        if status != 0:
            self.last_failure = {"exit_code": status,
                                 "stdout_sha256": hashlib.sha256(stdout).hexdigest(),
                                 "stderr_sha256": hashlib.sha256(stderr).hexdigest()}
            try:
                parsed = load_json(stdout)
                code = parsed.get("error", {}).get("code") if isinstance(parsed, dict) else None
                allowed = {"resource-budget", "finite-audio-tail-not-supported", "invalid-input-media",
                           "media-tool-failed", "media-tool-unavailable-or-timeout", "media-cleanup-unconfirmed",
                           "source-normalization-verification-failed", "round-trip-verification-failed",
                           "progressive-mp4-profile-not-supported", "dash-presentation-duration-mismatch"}
                if code in allowed:
                    self.last_failure["fixed_cli_code"] = code
            except (Refusal, AttributeError, TypeError):
                pass
            raise Refusal("image-command-failed")
        return stdout

    def close(self):
        if not self.names:
            return
        # Only names created by this instance; never prune other workloads.
        # Already removed --rm containers can make rm return nonzero. Confirm
        # actual absence with a successful daemon query instead of ignoring it.
        run(["docker", "rm", "--force", *self.names], timeout=30)
        remaining = successful([
            "docker", "container", "ls", "--all", "--filter",
            "label=io.drmr.qualification=owned", "--format", "{{.Names}}"], timeout=30)
        if set(remaining.decode("ascii").splitlines()).intersection(self.names):
            raise Refusal("owned-container-cleanup-unconfirmed")


def provenance(image, revision, tree):
    if not all(re.fullmatch(r"[0-9a-f]{40}", value) for value in (revision, tree)):
        raise Refusal("source-identity-invalid")
    labels = image.info.get("Config", {}).get("Labels", {})
    if (labels.get("org.opencontainers.image.revision") != revision
            or labels.get("io.drmr.source.tree") != tree):
        raise Refusal("source-label-mismatch")
    configuration = image.info["Config"]
    if (configuration.get("User") != "10001:10001" or configuration.get("WorkingDir") != "/work"
            or configuration.get("Entrypoint") != ["drmr"]
            or KEY in json.dumps(configuration)):
        raise Refusal("runtime-configuration-invalid")
    manifest = load_json(image.checked(["/usr/share/drmr/build-manifest.json"], entry="/bin/cat"))
    if (manifest.get("schema_version") != 1 or manifest.get("source_commit") != revision
            or manifest.get("source_tree") != tree
            or manifest.get("target_architecture") != image.architecture):
        raise Refusal("source-manifest-mismatch")
    actual = {}
    for name in ("drmr", "ffmpeg", "ffprobe", "packager"):
        record = manifest.get("binaries", {}).get(name, {})
        if record.get("path") != "/usr/local/bin/" + name:
            raise Refusal("binary-manifest-invalid")
        digest = image.checked([record["path"]], entry="/usr/bin/sha256sum").decode("ascii").split()[0]
        if not re.fullmatch(r"[0-9a-f]{64}", digest) or digest != record.get("sha256"):
            raise Refusal("runtime-binary-mismatch")
        actual[name] = digest
    uid = image.checked(["-u"], entry="/usr/bin/id").decode("ascii").strip()
    if uid != str(image.uid):
        raise Refusal("runtime-user-mismatch")
    inventories = {}
    for name in ("licenses", "sources"):
        inventory = load_json(image.checked([f"/usr/share/drmr/{name}/inventory.json"], entry="/bin/cat"))
        if not isinstance(inventory, list) or not inventory:
            raise Refusal("notice-source-inventory-missing")
        selected = {}
        for row in inventory:
            relative = row.get("path") if isinstance(row, dict) else None
            if (not isinstance(relative, str) or not relative or PurePosixPath(relative).is_absolute()
                    or any(part in (".", "..") for part in relative.split("/"))
                    or "\n" in relative or "\r" in relative or "\\" in relative
                    or relative in selected or not re.fullmatch(r"[0-9a-f]{64}", row.get("sha256", ""))):
                raise Refusal("notice-source-inventory-invalid")
            selected[f"/usr/share/drmr/{name}/{relative}"] = row["sha256"]
        output = image.checked(list(selected), entry="/usr/bin/sha256sum").decode("utf-8").splitlines()
        if len(output) != len(selected):
            raise Refusal("notice-source-files-missing")
        for line, (path, digest) in zip(output, selected.items()):
            if line != digest + "  " + path:
                raise Refusal("notice-source-file-changed")
        inventories[name] = {"files": len(selected), "inventory_sha256": hashlib.sha256(
            json.dumps(inventory, sort_keys=True, separators=(",", ":")).encode("utf-8")).hexdigest()}
    qualifier_manifest = load_json((image.qualifiers / "qualification-manifest.json").read_bytes())
    if (qualifier_manifest.get("source_commit") != revision or qualifier_manifest.get("source_tree") != tree
            or qualifier_manifest.get("target_architecture") != image.architecture
            or qualifier_manifest.get("drmr_sha256") != actual["drmr"]
            or qualifier_manifest.get("rustc") != manifest.get("rust", {}).get("rustc")):
        raise Refusal("qualifier-source-compiler-mismatch")
    qualifier_hashes = {}
    for name in ("container-qualification", "drmr-native-fixture", "container-resources"):
        path = image.qualifiers / name
        digest = sha256(path)
        with path.open("rb") as stream:
            header = stream.read(20)
        if (digest != qualifier_manifest.get("artifacts", {}).get(name, {}).get("sha256")
                or header[:6] != b"\x7fELF\x02\x01" or len(header) < 20
                or struct.unpack("<H", header[18:20])[0] != {"amd64": 62, "arm64": 183}[image.architecture]):
            raise Refusal("qualifier-binary-mismatch")
        qualifier_hashes[name] = digest
    return {"source_commit": revision, "source_tree": tree, "binaries_sha256": actual,
            "runtime_uid": image.uid, "runtime_gid": image.gid,
            "notice_source_inventories": inventories, "qualifiers_sha256": qualifier_hashes,
            "qualifier_rustc": qualifier_manifest["rustc"],
            "qualifier_scope": "same source/compiler; test-only core/std observers, not runtime additions"}


def video_evidence(image, path):
    raw = image.checked(["-v", "error", "-select_streams", "v:0", "-count_frames",
                         "-show_packets", "-show_streams", "-show_data_hash", "sha256",
                         "-show_entries", "stream=codec_name,time_base,nb_read_frames:packet=pts,dts,duration,data_hash",
                         "-of", "json", path], entry="/usr/local/bin/ffprobe")
    result = load_json(raw)
    require_video(result)
    return result


def finite_audio_evidence(image):
    profile = load_json(image.checked([
        "-v", "error", "-count_packets", "-show_entries",
        "stream=codec_name,codec_type,sample_rate,channels,nb_read_packets", "-of", "json", "/input/clip.mp4"],
        entry="/usr/local/bin/ffprobe"))
    streams = profile.get("streams", [])
    if (len(streams) != 2 or streams[0].get("nb_read_packets") != "240"
            or streams[0].get("codec_name") != "h264" or streams[1].get("codec_name") != "aac"
            or streams[1].get("nb_read_packets") != "470" or streams[1].get("sample_rate") != "48000"
            or streams[1].get("channels") != 2):
        raise Refusal("finite-original-profile-changed")
    decoded = []
    for path in ("/input/clip.mp4", "/work/finite.mp4"):
        pcm = image.checked(["-v", "error", "-nostdin", "-i", path, "-map", "0:a:0",
                             "-f", "s32le", "-acodec", "pcm_s32le", "-"], entry="/usr/local/bin/ffmpeg")
        if len(pcm) != 480_000 * 2 * 4:
            raise Refusal("finite-decoded-sample-count-changed")
        decoded.append(hashlib.sha256(pcm).hexdigest())
    if decoded[0] != decoded[1]:
        raise Refusal("finite-decoded-content-changed")
    return {"video_packets": 240, "audio_packets_including_priming": 470, "sample_rate": 48_000,
            "channels": 2, "decoded_samples_per_channel": 480_000, "source_result_pcm_sha256": decoded[0]}


def media(image, root):
    from container_source_window import preserve_video_window

    original = video_evidence(image, "/input/clip.mp4")
    image.checked(["-v", "error", "-nostdin", "-copyts", "-i", "/input/clip.mp4",
                   "-map", "0:v:0", "-an", "-c", "copy", "/work/video.mp4"],
                  entry="/usr/local/bin/ffmpeg")
    derived = video_evidence(image, "/work/video.mp4")
    if derived != original:
        raise Refusal("video-remux-changed-original")
    # Preparation is separate from immutable source mounts used by all conversions.
    for kind in ("dash", "hls"):
        destination = root / "work" / kind
        destination.mkdir(mode=0o700)
        image.checked([
            f"in=/work/video.mp4,stream=video,init_segment=/work/{kind}/init.mp4,segment_template=/work/{kind}/seg_$Number$.m4s,playlist_name=video.m3u8",
            "--segment_duration", "2", "--fragment_duration", "2",
            "--mp4_reset_initial_composition_offset_to_zero=false",
            *( ["--generate_static_live_mpd", "--mpd_output", "/work/dash/manifest.mpd"] if kind == "dash"
               else ["--hls_master_playlist_output", "/work/hls/master.m3u8"])],
            entry="/usr/local/bin/packager")
        if len(list(destination.glob("seg_*.m4s"))) != 5:
            raise Refusal("five-source-segments-required")
        # Preserve the actual progressive video window before treating this as an immutable source.
        try:
            preserve_video_window(root / "work/video.mp4", destination / "init.mp4")
        except ValueError:
            raise Refusal("source-window-preparation-invalid") from None
        shutil.copytree(destination, root / "input" / kind)
    audio_ts = root / "work/audio-ts"
    audio_ts.mkdir(mode=0o700)
    image.checked([
        "-v", "error", "-nostdin", "-copyts", "-i", "/input/clip.mp4", "-map", "0:a:0",
        "-vn", "-c", "copy", "-f", "hls", "-hls_time", "2", "-hls_list_size", "0",
        "-hls_segment_filename", "/work/audio-ts/seg_%03d.ts", "/work/audio-ts/master.m3u8"],
        entry="/usr/local/bin/ffmpeg")
    shutil.copytree(audio_ts, root / "input/audio-ts")
    before = hashes(root / "input")
    rows = []
    prepared_sources = {}
    for kind in ("dash", "hls"):
        source = f"/input/{kind}/" + ("manifest.mpd" if kind == "dash" else "master.m3u8")
        prepared_sources[kind] = load_json(image.checked([
            "verify", "--decode", source, "--source", "/work/video.mp4", "--max-workers", "1", "--json"]))
        require_verification(prepared_sources[kind])
        for protection in ("clear", "cbcs"):
            output = f"/work/{kind}-{protection}"
            options = ["--keys-file", "/keys/safe.json"] if protection == "cbcs" else []
            converted = load_json(image.checked(["convert", "--decode", source, "--to", protection, "--format", kind,
                                                 "--output", output, "--max-workers", "1", "--json", *options]))
            require_verification(converted["verification"])
            manifest = output + ("/manifest.mpd" if kind == "dash" else "/master.m3u8")
            verified = load_json(image.checked(["verify", "--decode", manifest, "--source", source,
                                                "--max-workers", "1", "--json", *options]))
            require_verification(verified)
            rows.append({"input": kind, "output": kind, "protection": protection,
                         "conversion": converted["verification"], "independent_verification": verified})
    audio_converted = load_json(image.checked([
        "convert", "--decode", "/input/audio-ts/master.m3u8", "--to", "cbcs", "--format", "hls",
        "--output", "/work/audio-ts-cbcs", "--max-workers", "1", "--json",
        "--keys-file", "/keys/safe.json"]))
    require_verification(audio_converted["verification"])
    audio_verified = load_json(image.checked([
        "verify", "--decode", "/work/audio-ts-cbcs/master.m3u8", "--source", "/input/audio-ts/master.m3u8",
        "--max-workers", "1", "--json", "--keys-file", "/keys/safe.json"]))
    require_verification(audio_verified)
    finite = load_json(image.checked(["convert", "--decode", "/input/clip.mp4", "--to", "clear", "--format", "mp4",
                                      "--output", "/work/finite.mp4", "--max-workers", "1", "--json"]))
    require_verification(finite["verification"])
    finite_verify = load_json(image.checked(["verify", "--decode", "/work/finite.mp4", "--source", "/input/clip.mp4",
                                             "--max-workers", "1", "--json"]))
    require_verification(finite_verify)
    if hashes(root / "input") != before:
        raise Refusal("source-changed")
    return {"prepared_source_against_all_original_video": prepared_sources,
            "representative_video_only_routes": rows, "full_original_finite_av": finite_verify,
            "audio_only_ts_to_fmp4_hls": {"input_protection": "clear", "output_protection": "cbcs",
                                          "conversion": audio_converted["verification"],
                                          "independent_verification_against_original_ts": audio_verified},
            "finite_original_counts": finite_audio_evidence(image),
            "video_packets": 240, "decoded_video_frames": 240, "video_segments": 5,
            "source_hashes": before}


def permissions(image, root):
    before = hashes(root / "input")
    keys_before = hashes(root / "keys")
    unsafe = root / "keys/unsafe.json"
    shutil.copyfile(root / "keys/safe.json", unsafe)
    unsafe.chmod(0o644)
    arguments = ["convert", "--decode", "/input/dash/manifest.mpd", "--to", "cbcs", "--format", "dash",
                 "--output", "/work/unsafe-key-output", "--keys-file", "/keys/unsafe.json", "--json"]
    status, stdout, stderr = image.invoke(arguments)
    if KEY.encode() in stdout + stderr:
        raise Refusal("secret-displayed")
    unsafe_result = require_failure(status, stdout, "key-file-invalid", 2)
    if (root / "work/unsafe-key-output").exists():
        raise Refusal("unsafe-key-output-published")
    unsafe.unlink()
    arguments = ["convert", "--decode", "/input/dash/manifest.mpd", "--to", "clear", "--format", "dash",
                 "--output", "/work/readonly-output", "--json"]
    status, stdout, _ = image.invoke(arguments, readonly=True)
    readonly = require_failure(status, stdout, "temporary-workspace-unavailable", 1)
    if (root / "work/readonly-output").exists():
        raise Refusal("readonly-output-published")
    existing_before = hashes(root / "work/dash-clear")
    arguments[arguments.index("/work/readonly-output")] = "/work/dash-clear"
    status, stdout, _ = image.invoke(arguments)
    existing = require_failure(status, stdout, "output-exists-or-unsafe", 1)
    if (hashes(root / "work/dash-clear") != existing_before or hashes(root / "input") != before
            or hashes(root / "keys") != keys_before):
        raise Refusal("refusal-changed-existing-data")
    return {"unsafe_key": unsafe_result, "readonly_output": readonly, "existing_output": existing}


def lifecycle(image, root):
    before = hashes(root / "input")
    report = load_json(image.checked(["stop", "/usr/local/bin/drmr", "/qualifiers/drmr-native-fixture",
                                      "/input/clip.mp4", "/work"],
                                     entry="/qualifiers/container-qualification"))
    require_stop(report)
    actual = load_json((root / "work/stop-control/stdout").read_bytes())
    if (actual.get("schema_version") != 1 or actual.get("command") != "convert"
            or actual.get("status") != "cancelled" or actual.get("error", {}).get("code") != "cancelled"):
        raise Refusal("cancelled-result-invalid")
    if (root / "work/cancelled-output").exists() or hashes(root / "input") != before:
        raise Refusal("stop-changed-source-or-published")
    return report


def resource_workers(raw):
    invalid = "container-resource-progress-invalid"
    if not isinstance(raw, bytes) or not raw or len(raw) > MAX_OUTPUT:
        raise Refusal(invalid)
    counters = {"elapsed_ms", "scopes_started", "scopes_closed", "active_scopes", "tools_started",
                "tools_closed", "active_tool_count", "work_units_completed"}
    required = counters | {"schema_version", "entry_point", "command", "run_id", "event", "activity", "task", "active_tools"}
    optional = {"workers", "resources_started", "resources_completed", "bytes_received", "work_units_total"}
    activities = {"configuration", "acquisition", "input-preparation", "inspection", "normalization", "packaging",
                  "verification", "publication", "tool-check", "server-startup", "serving"}
    workers, run_id, elapsed, finished, last_workers = [], None, -1, False, False
    for line in raw.splitlines():
        try:
            record = load_json(line)
        except Refusal:
            raise Refusal(invalid) from None
        if (finished or not isinstance(record, dict) or not required.issubset(record)
                or set(record) - required - optional or type(record["schema_version"]) is not int
                or record["schema_version"] != 1 or record["entry_point"] != "cli" or record["command"] != "convert"
                or not isinstance(record["run_id"], str)
                or re.fullmatch(r"[0-9]{1,20}-[0-9]{1,40}", record["run_id"]) is None
                or record["event"] not in ("activity", "progress", "heartbeat", "finished")
                or (record["activity"] is not None
                    and (not isinstance(record["activity"], str) or record["activity"] not in activities))
                or not isinstance(record["task"], str) or not 0 < len(record["task"]) <= 4096
                or any(type(record[name]) is not int or not 0 <= record[name] < 2 ** 64
                       for name in counters | (optional.intersection(record)))
                or not isinstance(record["active_tools"], list) or len(record["active_tools"]) > 3
                or any(tool not in ("ffmpeg", "ffprobe", "packager") for tool in record["active_tools"])
                or len(set(record["active_tools"])) != len(record["active_tools"])):
            raise Refusal(invalid)
        if (run_id is not None and record["run_id"] != run_id) or record["elapsed_ms"] < elapsed:
            raise Refusal(invalid)
        run_id, elapsed = record["run_id"], record["elapsed_ms"]
        finished, last_workers = record["event"] == "finished", "workers" in record
        if last_workers:
            if not 1 <= record["workers"] <= 2:
                raise Refusal(invalid)
            workers.append(record["workers"])
    if not finished:
        raise Refusal(invalid)
    if not workers:
        raise Refusal("container-resource-workers-missing")
    if not last_workers:
        raise Refusal(invalid)
    return workers


def resources(image):
    observed = load_json(image.checked([], entry="/qualifiers/container-resources", limited=True))
    if (observed.get("kind") != "native-hardware-observation"
            or observed.get("memory_source") != "process-cgroup"
            or type(observed.get("effective_cpus")) is not int
            or not 1 <= observed["effective_cpus"] <= 2
            or type(observed.get("available_memory_bytes")) is not int
            or not 0 < observed["available_memory_bytes"] <= 1024 ** 3):
        source = observed.get("memory_source")
        cpus = observed.get("effective_cpus")
        memory = observed.get("available_memory_bytes")
        raise Refusal("container-resource-observation-invalid", {
            "memory_source": source if source in ("host", "unknown", "process-cgroup") else "invalid",
            "effective_cpus": cpus if type(cpus) is int and 0 <= cpus <= 100_000 else None,
            "available_memory_bytes": memory if type(memory) is int and 0 <= memory < 2 ** 63 else None})
    status, stdout, stderr = image.invoke([
        "convert", "--decode", "/input/dash/manifest.mpd", "--to", "clear", "--format", "dash",
        "--output", "/work/resource-limited", "--progress", "json", "--json"], limited=True)
    failure = {"exit_code": status if type(status) is int and -(2 ** 31) <= status < 2 ** 32 else None,
               "stdout_sha256": hashlib.sha256(stdout).hexdigest(),
               "stderr_sha256": hashlib.sha256(stderr).hexdigest()}
    if type(status) is not int or status != 0:
        raise Refusal("container-resource-cli-failed", failure)
    try:
        workers = resource_workers(stderr)
    except Refusal as error:
        raise Refusal(str(error), failure) from None
    try:
        converted = load_json(stdout)
    except Refusal:
        raise Refusal("container-resource-cli-failed", failure) from None
    fields = {"schema_version", "input", "output", "verification"}
    proof_fields = {"schema_version", "required_depth", "scope", "passed", "checks", "decode_evidence",
                    "license_playback_verified", "native_manifest"}
    conversion_proof = converted.get("verification") if isinstance(converted, dict) else None
    if (not isinstance(converted, dict) or not fields.issubset(converted)
            or set(converted) - fields - {"processing"}
            or type(converted["schema_version"]) is not int or converted["schema_version"] != 2
            or not isinstance(converted["input"], dict) or not isinstance(converted["output"], dict)
            or not isinstance(conversion_proof, dict) or set(conversion_proof) != proof_fields
            or EVIDENCE.v2_proof(conversion_proof, require_decoded=True) is None
            or conversion_proof["native_manifest"]["profile"] != "dash-static-manifest"):
        raise Refusal("container-resource-cli-failed", failure)
    verified = load_json(image.checked([
        "verify", "--decode", "/work/resource-limited/manifest.mpd", "--source", "/input/dash/manifest.mpd", "--json"], limited=True))
    require_verification(verified)
    return {"limits": {"memory_bytes": 1024 ** 3, "cpus": 2},
            "separate_native_hardware_observation": observed,
            "actual_cli_workers": [int(count) for count in workers],
            "independent_verification": verified,
            "limitations": "Native probe and CLI use separate containers with the same specified limits; this is not a hidden CLI budget field or an exact memory maximum."}


def http_request(port, method, path, body=None, headers=None):
    connection = http.client.HTTPConnection("127.0.0.1", port, timeout=2)
    try:
        raw = None if body is None else json.dumps(body).encode("utf-8")
        request_headers = {"Content-Type": "application/json", "Origin": f"http://127.0.0.1:{port}"}
        request_headers.update(headers or {})
        connection.request(method, path, raw, request_headers)
        response = connection.getresponse()
        content = response.read(65_537)
        if len(content) > 65_536:
            raise Refusal("http-output-limit")
        return response.status, content
    finally:
        connection.close()


def web_token(value):
    return isinstance(value, str) and re.fullmatch(r"[0-9a-f]{32}", value) is not None


def common_web_context(port):
    """Resolve the sole protected fixture policy through the actual common API."""
    status, content = http_request(port, "GET", "/api/common/capabilities")
    capabilities = load_json(content)
    if (status != 200 or not isinstance(capabilities, dict)
            or capabilities.get("schema_version") != 1 or not web_token(capabilities.get("server_id"))
            or not isinstance(capabilities.get("roots"), list)):
        raise Refusal("web-reference-invalid")
    roots = [root.get("root_ref") for root in capabilities["roots"]
             if isinstance(root, dict) and isinstance(root.get("roles"), list) and "fetch-policy" in root["roles"]]
    if len(roots) != 1 or not web_token(roots[0]):
        raise Refusal("web-reference-invalid")
    server = capabilities["server_id"]
    status, content = http_request(port, "GET", "/api/common/files?root_ref=" + roots[0] + "&role=fetch-policy")
    page = load_json(content)
    if (status != 200 or not isinstance(page, dict) or page.get("schema_version") != 1
            or page.get("server_id") != server or page.get("next_cursor") is not None
            or not isinstance(page.get("entries"), list) or len(page["entries"]) != 1):
        raise Refusal("web-reference-invalid")
    entry = page["entries"][0]
    if not isinstance(entry, dict) or entry.get("kind") != "file" or not web_token(entry.get("file_ref")):
        raise Refusal("web-reference-invalid")
    return server, entry["file_ref"]


def submit_web_analysis(port, server, policy, url):
    creation = uuid.uuid4().hex
    status, content = http_request(port, "POST", "/api/common/sessions", {
        "schema_version": 1, "server_id": server, "creation_request_id": creation,
        "source": {"kind": "url", "url": url, "fetch_policy_ref": policy}})
    session = load_json(content)
    if (status != 201 or not isinstance(session, dict) or session.get("schema_version") != 1
            or session.get("server_id") != server or session.get("creation_request_id") != creation
            or not web_token(session.get("session_id"))):
        raise Refusal("web-real-session-not-created")
    request = uuid.uuid4().hex
    status, content = http_request(port, "POST", "/api/common/jobs", {
        "schema_version": 1, "server_id": server, "session_id": session["session_id"],
        "request_id": request, "operation": {"kind": "analyze"},
        "runtime": {"decode": True,"keys_ref": None, "max_workers": 1}})
    job = load_json(content)
    if (status != 202 or not isinstance(job, dict) or job.get("schema_version") != 1
            or job.get("server_id") != server
            or job.get("session_id") != session["session_id"] or job.get("request_id") != request
            or job.get("operation") != "analyze" or job.get("state") != "running"
            or not web_token(job.get("job_id"))):
        raise Refusal("web-real-job-not-launched")
    return job["job_id"]


def verify_web_host_guards(port):
    cases = (("untrusted-authority", "other.invalid", 403, "untrusted-host"),
             ("malformed-authority", "user@localhost", 400, "invalid-host"))
    records = []
    for case, host, expected_status, expected_error in cases:
        status, content = http_request(port, "GET", "/api/common/capabilities", headers={"Host": host})
        parsed = load_json(content)
        detail = parsed.get("error") if isinstance(parsed, dict) else None
        error = detail.get("code") if isinstance(detail, dict) else None
        if (status != expected_status or error != expected_error or parsed.get("schema_version") != 1
                or not web_token(parsed.get("server_id"))):
            raise Refusal("web-host-guard-failed", {
                "case": case, "actual_status": status,
                "actual_error": error if error in ("untrusted-host", "invalid-host") else "unexpected"})
        records.append({"case": case, "status": status, "error": error})
    return records


def available_port():
    with socket.socket() as listener:
        listener.bind(("127.0.0.1", 0))
        return listener.getsockname()[1]


def wait_file(path, process):
    deadline = time.monotonic() + 30
    while not path.is_file():
        returncode = process.poll()
        if returncode is not None:
            # The child may publish the barrier and exit between the two observations.
            # Observe that completed publication before declaring its barrier missing.
            if path.is_file():
                return
            raise Refusal("web-lifecycle-barrier-missing", {
                "reason": "driver-exited", "driver_returncode": returncode,
                "barrier": path.name if path.name in WEB_BARRIERS else "unknown"})
        if time.monotonic() >= deadline:
            raise Refusal("web-lifecycle-barrier-missing", {
                "reason": "deadline", "driver_returncode": None,
                "barrier": path.name if path.name in WEB_BARRIERS else "unknown"})
        time.sleep(0.01)


WEB_BARRIERS = ("stop-job", "stop-ready", "stop-confirmed", "shutdown-job", "shutdown-ready")
WEB_PHASES = (
    "web-await-stop-job", "web-check-stop-listener", "web-await-stop-fixture",
    "web-await-stop-terminal", "web-check-stop-return", "web-await-shutdown-job",
    "web-await-shutdown-fixture", "web-await-shutdown-exit",
    "web-check-shutdown-return", "web-shutdown-return-state",
)
WEB_CHECKS = (
    "registry-bounds", "registry-entry", "registry-record", "fixture-deadline", "owner-exit",
    "tool-parent", "tool-group", "descendant-parent", "descendant-group", "owner-group",
    "held-lock", "tool-absent", "descendant-absent", "group-absent", "lock-released",
    "job-state", "loopback-listener",
)
WEB_JOB_STATES = ("running", "interrupted", "failed", "succeeded", "unavailable")


def web_failure_evidence(root, process):
    # Keep only fixed labels, typed statuses and bounded log fingerprints before cleanup.
    # Raw tool output, arguments, key material and references never enter this evidence.
    control = root / "work/web-control"
    evidence = {"driver_returncode": None if process is None else process.poll(),
                "server_returncode": None, "server_signal": None, "server_exit_observed": False,
                "driver_phase": None, "driver_check": None, "driver_job_state": None,
                "error_kind": None, "errno": None,
                "barriers": {name: (control / name).is_file() for name in WEB_BARRIERS},
                "logs": {}}
    for label, path in (("driver-stderr", root / "work/web-driver.stderr"),
                        ("server-stderr", control / "server.stderr")):
        try:
            if not stat.S_ISREG(path.lstat().st_mode):
                continue
            with path.open("rb") as stream:
                data = stream.read(MAX_OUTPUT + 1)
            if len(data) > MAX_OUTPUT:
                evidence["logs"][label] = {"output_limit": True}
                continue
            evidence["logs"][label] = {"snapshot_bytes": len(data), "sha256": hashlib.sha256(data).hexdigest()}
            if label != "driver-stderr":
                continue
            for line in data.decode("ascii", errors="replace").splitlines():
                for token, field, allowed in (("phase", "driver_phase", WEB_PHASES),
                                              ("check", "driver_check", WEB_CHECKS),
                                              ("job-state", "driver_job_state", WEB_JOB_STATES)):
                    prefix = f"container-qualification: {token}="
                    if line.startswith(prefix):
                        value = line.removeprefix(prefix)
                        if value in allowed:
                            evidence[field] = value
                error = re.fullmatch(r"container-qualification: lifecycle-unconfirmed kind=([A-Za-z]+) errno=(None|Some\((-?[0-9]{1,10})\))", line)
                if error:
                    known = {"Other", "NotFound", "PermissionDenied", "InvalidData", "InvalidInput", "TimedOut",
                             "WouldBlock", "ConnectionRefused", "ConnectionReset", "BrokenPipe", "UnexpectedEof"}
                    evidence["error_kind"] = error[1] if error[1] in known else "unrecognized"
                    evidence["errno"] = None if error[3] is None else int(error[3])
                status = re.fullmatch(r"container-qualification: owner-exit code=(None|Some\((-?[0-9]{1,10})\)) signal=(None|Some\(([0-9]{1,3})\))", line)
                if status:
                    evidence.update(server_exit_observed=True,
                                    server_returncode=None if status[2] is None else int(status[2]),
                                    server_signal=None if status[4] is None else int(status[4]))
        except OSError:
            continue
    return evidence


def atomic_text(path, value):
    temporary = path.with_suffix(".pending")
    with temporary.open("x", encoding="ascii") as stream:
        stream.write(value)
    temporary.replace(path)


def web(image, root):
    class Handler(http.server.SimpleHTTPRequestHandler):
        protocol_version = "HTTP/1.1"

        def __init__(self, *args, **kwargs):
            super().__init__(*args, directory=str(root / "input/hls"), **kwargs)

        def log_message(self, *_):
            pass

    class Server(http.server.ThreadingHTTPServer):
        request_queue_size = 128
        daemon_threads = True

    fixture = Server(("127.0.0.1", 0), Handler)
    thread = threading.Thread(target=fixture.serve_forever, daemon=True)
    thread.start()
    port = available_port()
    origin = f"http://127.0.0.1:{fixture.server_port}"
    configuration = root / "work/web-config"
    configuration.mkdir(mode=0o700)
    with open(configuration / "fetch.json", "x", encoding="utf-8",
              opener=lambda path, flags: os.open(path, flags, 0o600)) as stream:
        json.dump({"allowed_origins": [origin], "allow_http": True, "allow_private_network": True}, stream)
    stdout = root / "work/web-driver.stdout"
    stderr = root / "work/web-driver.stderr"
    command = image.command("/qualifiers/container-qualification", [
        "web", "/usr/local/bin/drmr", "/qualifiers/drmr-native-fixture", "/work", str(port)], network="host")
    before = hashes(root / "input")
    process = None
    try:
        with stdout.open("xb") as out, stderr.open("xb") as err:
            process = subprocess.Popen(command, stdin=subprocess.DEVNULL, stdout=out, stderr=err)
            deadline = time.monotonic() + 15
            while True:
                try:
                    status, _ = http_request(port, "GET", "/api/common/capabilities")
                    if status != 200:
                        raise Refusal("web-capabilities-failed")
                    break
                except (ConnectionError, OSError):
                    if process.poll() is not None or time.monotonic() >= deadline:
                        raise Refusal("web-loopback-unavailable") from None
                    time.sleep(0.02)
            # These are actual image HTTP/API checks, not a browser or GUI qualification.
            for path in ("/api/common/sessions", "/api/common/jobs"):
                status, content = http_request(port, "POST", path, {}, {"Origin": "https://other.invalid"})
                if status != 403 or load_json(content).get("error", {}).get("code") != "same-origin-required":
                    raise Refusal("web-origin-guard-failed")
            host_guard_cases = verify_web_host_guards(port)
            server, policy = common_web_context(port)
            controls = root / "work/web-control"
            ids = []
            for label in ("stop", "shutdown"):
                job = submit_web_analysis(port, server, policy, origin + "/master.m3u8")
                ids.append(job)
                atomic_text(controls / (label + "-job"), job)
                wait_file(controls / (label + "-ready"), process)
                if label == "stop":
                    status, _ = http_request(port, "POST", f"/api/common/jobs/{job}/stop", {})
                    if status not in (200, 202):
                        raise Refusal("web-stop-refused")
                    wait_file(controls / "stop-confirmed", process)
            try:
                if process.wait(timeout=15) != 0:
                    raise Refusal("web-owned-driver-failed")
            except subprocess.TimeoutExpired:
                raise Refusal("web-owned-driver-deadline") from None
        if stdout.stat().st_size > MAX_OUTPUT or stderr.stat().st_size > MAX_OUTPUT:
            raise Refusal("web-driver-output-limit")
        result = load_json(stdout.read_bytes())
        fields = ("success", "driver_still_alive", "immediate_lock_released", "tool_absent",
                  "descendant_absent", "listener_released")
        if (result.get("kind") != "web-stop-and-orderly-shutdown"
                or any(result.get(field) is not True for field in fields)):
            raise Refusal("web-owned-proof-invalid")
        if hashes(root / "input") != before:
            raise Refusal("web-changed-source")
        return {"status": "http-api-qualified", "browser_gui": "not-tested", "host_origin_guards": True,
                "host_guard_cases": host_guard_cases, "two_real_jobs": len(ids), "lifecycle": result}
    except Refusal as error:
        evidence = web_failure_evidence(root, process)
        if error.evidence is not None:
            evidence["failure_context"] = error.evidence
        raise Refusal(str(error), evidence) from None
    finally:
        fixture.shutdown()
        fixture.server_close()
        thread.join(timeout=5)
        if thread.is_alive():
            raise Refusal("fixture-http-cleanup-unconfirmed")
        if process is not None and process.poll() is None:
            # The owned container remains in Image's ledger and is removed before the workspace.
            process.terminate()
            try:
                process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait(timeout=5)


def require_empty_path_discovery(status, raw):
    missing = load_json(raw)
    tools = missing.get("tools") if isinstance(missing, dict) else None
    if (status != 1 or not isinstance(missing, dict) or missing.get("ready") is not False
            or missing.get("mode") != "discovery" or not isinstance(tools, list) or len(tools) != 3
            or any(not isinstance(tool, dict) or tool.get("available") is not False for tool in tools)
            or {tool.get("name") for tool in tools} != {"ffmpeg", "ffprobe", "packager"}):
        raise Refusal("empty-path-discovery-invalid")
    return {"exit_code": status, "ready": False, "tools_missing": 3}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--image", required=True)
    parser.add_argument("--qualifiers", required=True, type=Path)
    parser.add_argument("--expected-revision", required=True)
    parser.add_argument("--expected-tree", required=True)
    parser.add_argument("--web", action="store_true", help="Require native Linux host-network HTTP and active-job shutdown checks")
    parser.add_argument("--report", required=True, type=Path)
    arguments = parser.parse_args()
    if arguments.report.exists():
        raise Refusal("report-already-exists")
    if sha256(FIXTURE) != CLIP_SHA256:
        raise Refusal("fixture-changed")
    image = None
    report = {"schema_version": 1, "success": False}
    try:
        with ExitStack() as stack:
            temporary = stack.enter_context(tempfile.TemporaryDirectory(prefix="drmr-container-qualification-"))
            root = Path(temporary)
            for name in ("input", "keys", "work"):
                (root / name).mkdir(mode=0o700)
            shutil.copyfile(FIXTURE, root / "input/clip.mp4")
            config = root / "keys/safe.json"
            with open(config, "x", encoding="utf-8", opener=lambda path, flags: os.open(path, flags, 0o600)) as file:
                json.dump({"schema_version": 1, "output": {
                    "kid": KID, "key": KEY, "hls_key_uri": "https://keys.example.invalid/container"}}, file)
            image = Image(arguments.image, root, arguments.qualifiers)
            stack.callback(image.close)
            report.update(image_id=image.identifier, architecture=image.architecture)
            report["active_gate"] = "provenance"
            report["provenance"] = provenance(image, arguments.expected_revision, arguments.expected_tree)
            report["active_gate"] = "diagnosis"
            diagnosis = load_json(image.checked(["doctor", "--probe", "--route", "all", "--json"]))
            if diagnosis.get("ready") is not True or diagnosis.get("cleanup_status") != "confirmed":
                raise Refusal("diagnosis-failed")
            report["diagnosis"] = diagnosis
            status, stdout, _ = image.invoke(["doctor", "--route", "all", "--json"], empty_path=True)
            report["empty_path_discovery"] = require_empty_path_discovery(status, stdout)
            image.checked(["-c", "for tool in python python3 node cargo rustc cmake gcc g++; do if command -v \"$tool\" >/dev/null 2>&1; then exit 1; fi; done"], entry="/bin/sh")
            report["active_gate"] = "media"
            report["media"] = media(image, root)
            report["active_gate"] = "permissions"
            report["permissions"] = permissions(image, root)
            report["active_gate"] = "lifecycle"
            report["lifecycle"] = lifecycle(image, root)
            report["active_gate"] = "resources"
            report["resources"] = resources(image)
            report["active_gate"] = "web"
            report["web"] = web(image, root) if arguments.web else {"status": "not-tested", "reason": "host-network-not-requested"}
            if hashes(root / "input") != report["media"]["source_hashes"]:
                raise Refusal("source-changed")
        # A receipt becomes successful only after all owned cleanup callbacks
        # have returned, including exceptions outside the expected refusal type.
        report["success"] = True
        report.pop("active_gate", None)
    except Refusal as error:
        report["success"] = False
        report["failure_code"] = str(error)
        if error.evidence is not None:
            report["failure_evidence"] = error.evidence
        if image is not None and image.last_failure is not None:
            report["failed_command"] = image.last_failure
    except (OSError, KeyError, TypeError, subprocess.TimeoutExpired):
        report["success"] = False
        report["failure_code"] = "qualification-input-or-report-invalid"
    finally:
        with arguments.report.open("x", encoding="utf-8") as stream:
            json.dump(report, stream, sort_keys=True, indent=2)
            stream.write("\n")
    print(json.dumps({"success": report["success"], "failure_code": report.get("failure_code")}))
    return 0 if report["success"] else 1


if __name__ == "__main__":
    raise SystemExit(main())
