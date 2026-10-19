#!/usr/bin/env python3
"""Qualify one immutable, native drmr image without publishing it or changing host inputs."""

import argparse
from contextlib import ExitStack
import hashlib
import http.client
import http.server
import json
import os
from pathlib import Path, PurePosixPath
import re
import shutil
import socket
import struct
import subprocess
import tempfile
import threading
import time
import uuid


ROOT = Path(__file__).resolve().parents[1]
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
    for name in ("drmr", "ffmpeg", "ffprobe", "mp4dump", "packager"):
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
        shutil.copytree(destination, root / "input" / kind)
    before = hashes(root / "input")
    rows = []
    prepared_sources = {}
    for kind in ("dash", "hls"):
        source = f"/input/{kind}/" + ("manifest.mpd" if kind == "dash" else "master.m3u8")
        prepared_sources[kind] = load_json(image.checked([
            "verify", source, "--source", "/work/video.mp4", "--max-workers", "1", "--json"]))
        require_verification(prepared_sources[kind])
        for protection in ("clear", "cbcs"):
            output = f"/work/{kind}-{protection}"
            options = ["--keys-file", "/keys/safe.json"] if protection == "cbcs" else []
            converted = load_json(image.checked(["convert", source, "--to", protection, "--format", kind,
                                                 "--output", output, "--max-workers", "1", "--json", *options]))
            require_verification(converted["verification"])
            manifest = output + ("/manifest.mpd" if kind == "dash" else "/master.m3u8")
            verified = load_json(image.checked(["verify", manifest, "--source", source,
                                                "--max-workers", "1", "--json", *options]))
            require_verification(verified)
            rows.append({"input": kind, "output": kind, "protection": protection,
                         "conversion": converted["verification"], "independent_verification": verified})
    finite = load_json(image.checked(["convert", "/input/clip.mp4", "--to", "clear", "--format", "mp4",
                                      "--output", "/work/finite.mp4", "--max-workers", "1", "--json"]))
    require_verification(finite["verification"])
    finite_verify = load_json(image.checked(["verify", "/work/finite.mp4", "--source", "/input/clip.mp4",
                                             "--max-workers", "1", "--json"]))
    require_verification(finite_verify)
    if hashes(root / "input") != before:
        raise Refusal("source-changed")
    return {"prepared_source_against_all_original_video": prepared_sources,
            "representative_video_only_routes": rows, "full_original_finite_av": finite_verify,
            "finite_original_counts": finite_audio_evidence(image),
            "video_packets": 240, "decoded_video_frames": 240, "video_segments": 5,
            "source_hashes": before}


def permissions(image, root):
    before = hashes(root / "input")
    keys_before = hashes(root / "keys")
    unsafe = root / "keys/unsafe.json"
    shutil.copyfile(root / "keys/safe.json", unsafe)
    unsafe.chmod(0o644)
    arguments = ["convert", "/input/dash/manifest.mpd", "--to", "cbcs", "--format", "dash",
                 "--output", "/work/unsafe-key-output", "--keys-file", "/keys/unsafe.json", "--json"]
    status, stdout, stderr = image.invoke(arguments)
    if KEY.encode() in stdout + stderr:
        raise Refusal("secret-displayed")
    unsafe_result = require_failure(status, stdout, "key-file-invalid", 2)
    if (root / "work/unsafe-key-output").exists():
        raise Refusal("unsafe-key-output-published")
    unsafe.unlink()
    arguments = ["convert", "/input/dash/manifest.mpd", "--to", "clear", "--format", "dash",
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
        "convert", "/input/dash/manifest.mpd", "--to", "clear", "--format", "dash",
        "--output", "/work/resource-limited"], limited=True)
    workers = re.findall(rb"Using up to ([0-9]+) workers", stderr)
    if (status != 0 or not workers or any(not 1 <= int(count) <= 2 for count in workers)
            or b"Verified stream metadata, packet payloads, decoded content, and packet timing." not in stdout):
        raise Refusal("container-resource-cli-failed")
    verified = load_json(image.checked([
        "verify", "/work/resource-limited/manifest.mpd", "--source", "/input/dash/manifest.mpd", "--json"], limited=True))
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


def available_port():
    with socket.socket() as listener:
        listener.bind(("127.0.0.1", 0))
        return listener.getsockname()[1]


def wait_file(path, process):
    deadline = time.monotonic() + 30
    while not path.is_file():
        if process.poll() is not None or time.monotonic() >= deadline:
            raise Refusal("web-lifecycle-barrier-missing")
        time.sleep(0.01)


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
    profiles = root / "work/web-profiles.json"
    with open(profiles, "x", encoding="utf-8", opener=lambda path, flags: os.open(path, flags, 0o600)) as stream:
        json.dump({"default": {"policy": {"allowed_origins": [origin], "allow_http": True,
                                         "allow_private_network": True}}}, stream)
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
                    status, _ = http_request(port, "GET", "/api/catalog")
                    if status != 200:
                        raise Refusal("web-catalog-failed")
                    break
                except (ConnectionError, OSError):
                    if process.poll() is not None or time.monotonic() >= deadline:
                        raise Refusal("web-loopback-unavailable") from None
                    time.sleep(0.02)
            # These are actual image HTTP/API checks, not a browser or GUI qualification.
            for path in ("/api/queue", "/api/hls/analyses"):
                status, content = http_request(port, "POST", path, {}, {"Origin": "https://other.invalid"})
                if status != 403 or load_json(content).get("error") != "same-origin-required":
                    raise Refusal("web-origin-guard-failed")
            status, content = http_request(port, "GET", "/api/catalog", headers={"Host": "other.invalid"})
            if status != 400 or load_json(content).get("error") != "invalid-host":
                raise Refusal("web-host-guard-failed")
            controls = root / "work/web-control"
            ids = []
            for label in ("stop", "shutdown"):
                body = {"asset_id": "container-" + label, "url": origin + "/master.m3u8", "access_profile": "default"}
                status, content = http_request(port, "POST", "/api/hls/analyses", body)
                result = load_json(content)
                job = result.get("job_id") if isinstance(result, dict) else None
                if status != 202 or not isinstance(job, str) or not re.fullmatch(r"[0-9a-f]{32}", job):
                    raise Refusal("web-real-job-not-launched")
                ids.append(job)
                atomic_text(controls / (label + "-job"), job)
                wait_file(controls / (label + "-ready"), process)
                if label == "stop":
                    status, _ = http_request(port, "POST", f"/api/jobs/{job}/stop", {})
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
                "two_real_jobs": len(ids), "lifecycle": result}
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
                json.dump({"schema_version": 1, "output": {"kid": KID, "key": KEY}}, file)
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
            missing = load_json(stdout)
            if (status != 1 or missing.get("ready") is not False or missing.get("mode") != "discovery"
                    or len(missing.get("tools", [])) != 4
                    or any(tool.get("available") is not False for tool in missing["tools"])):
                raise Refusal("empty-path-discovery-invalid")
            report["empty_path_discovery"] = {"exit_code": status, "ready": False, "tools_missing": 4}
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
