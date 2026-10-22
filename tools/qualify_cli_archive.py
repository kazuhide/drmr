#!/usr/bin/env python3
"""Execute one verified native CLI archive against the attributed ten-second clip.

Requires Python 3.9+, a clean committed source checkout, offline Cargo dependency
sources, and the supported media tools on PATH. This helper builds and downloads
nothing. Only the extracted CLI is executed; source-tree CLI binaries are never
selected. Reports contain allowlisted evidence, not paths, keys, or raw output.
"""

import argparse
import base64
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import platform
import re
import secrets
import shutil
import signal
import stat
import struct
import subprocess
import sys
import tarfile
import tempfile
import time
import zipfile
import zlib


ROOT = Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location("verify_archive", ROOT / "tools/verify_archive.py")
VERIFY = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(VERIFY)
WINDOW_SPEC = importlib.util.spec_from_file_location(
    "container_source_window", ROOT / "tools/container_source_window.py")
WINDOW = importlib.util.module_from_spec(WINDOW_SPEC)
WINDOW_SPEC.loader.exec_module(WINDOW)
CLIP = "crates/drmr/tests/fixtures/bbb/clip.mp4"
CLIP_BYTES = 195472
CLIP_SHA256 = "a0ef6d20f2319c57af35d9c3165e46b669c67b951268cf882dbfff14b5b26e71"
MAX_OUTPUT = 8 * 1024 * 1024
MAX_GRAPH_BYTES = 64 * 1024 * 1024
SEMVER = r"(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)(?:-[0-9A-Za-z]+(?:[.-][0-9A-Za-z]+)*)?"
CHECKS = ("packet_payloads_equal", "packet_timing_equal", "decoded_content_equal")
AAC44100_FRAMES = 432
MEDIA_LIMITS = (
    "One native packaged CLI, one ten-second video-only Clear DASH to CBCS DASH saved-plan route "
    "and one generated ten-second 44.1 kHz stereo AAC Clear HLS/TS to CBCS DASH saved-plan route. "
    "Full original video comparison, complete generated audio frame/clock evidence and CLI media comparisons, "
    "and native manifest linting; no independent external audio PCM comparison, DRM license/device playback, "
    "GUI, installer, signature, or build-origin certification.")
ARCHIVE_ERRORS = (VERIFY.Failure, OSError, ValueError, TypeError, KeyError, EOFError, NotImplementedError,
                  tarfile.TarError, zipfile.BadZipFile, zlib.error)


class Refusal(RuntimeError):
    """A fixed diagnostic, never an operator value or external-tool message."""

    def __init__(self, code, stage=None, evidence=None):
        super().__init__(code)
        self.stage = stage
        self.evidence = evidence


class Parser(argparse.ArgumentParser):
    def error(self, _):
        raise Refusal("arguments-invalid")


def require(condition, code):
    if not condition:
        raise Refusal(code)


def load_json(raw):
    def unique(pairs):
        result = {}
        for key, value in pairs:
            require(key not in result, "json-invalid")
            result[key] = value
        return result

    def constant(_):
        raise Refusal("json-invalid")

    try:
        return json.loads(raw, object_pairs_hook=unique, parse_constant=constant)
    except (ValueError, TypeError, UnicodeError, RecursionError):
        raise Refusal("json-invalid") from None


def require_native(target, system=None, machine=None, elevated=None):
    try:
        expected_system, expected_arch = VERIFY.target_info(target)
    except VERIFY.Failure:
        raise Refusal("native-target-required") from None
    actual_system = {"Darwin": "macos", "Linux": "linux", "Windows": "windows"}.get(system or platform.system())
    aliases = {"x86_64": "x86_64", "amd64": "x86_64", "aarch64": "aarch64", "arm64": "aarch64"}
    actual_arch = aliases.get((machine or platform.machine()).lower())
    require((actual_system, actual_arch) == (expected_system, expected_arch), "native-target-required")
    if elevated is None:
        if os.name == "nt":
            import ctypes
            elevated = bool(ctypes.windll.shell32.IsUserAnAdmin())
        else:
            elevated = os.geteuid() == 0
    require(not elevated, "ordinary-user-required")


def require_version(source, expected):
    require(re.fullmatch(SEMVER, expected) is not None, "expected-version-invalid")
    manifest = source.read("Cargo.toml").decode("utf-8")
    sections = re.findall(r"(?ms)^\[workspace\.package\]\s*\n(.*?)(?=^\[|\Z)", manifest)
    require(len(sections) == 1, "source-version-invalid")
    versions = re.findall(r'(?m)^version\s*=\s*"([^"\r\n]+)"\s*$', sections[0])
    require(versions == [expected], "source-version-mismatch")


def secure_windows(path, directory):
    """Apply the same current-user-only ACL profile documented for Windows users."""
    if os.name != "nt":
        return
    # The script is fixed; the owned path crosses the boundary as an environment
    # value, never as PowerShell source or a shell-interpreted command argument.
    script = r"""
$ErrorActionPreference = 'Stop'
$identity = [System.Security.Principal.WindowsIdentity]::GetCurrent().User
$directory = $env:DRMR_QUALIFY_DIRECTORY -eq '1'
$acl = if ($directory) { [System.Security.AccessControl.DirectorySecurity]::new() } else { [System.Security.AccessControl.FileSecurity]::new() }
$acl.SetOwner($identity)
$acl.SetAccessRuleProtection($true, $false)
$inheritance = if ($directory) { [System.Security.AccessControl.InheritanceFlags]'ContainerInherit, ObjectInherit' } else { [System.Security.AccessControl.InheritanceFlags]::None }
$rule = [System.Security.AccessControl.FileSystemAccessRule]::new($identity, [System.Security.AccessControl.FileSystemRights]::FullControl, $inheritance, [System.Security.AccessControl.PropagationFlags]::None, [System.Security.AccessControl.AccessControlType]::Allow)
$acl.AddAccessRule($rule)
# Persist only the owner and DACL we modified. Set-Acl copies unrelated audit
# and group sections too, which can require privileges an ordinary user lacks.
$sections = [System.Security.AccessControl.AccessControlSections]'Owner, Access'
$checked = if ($directory) {
    [System.IO.Directory]::SetAccessControl($env:DRMR_QUALIFY_PATH, $acl)
    [System.IO.Directory]::GetAccessControl($env:DRMR_QUALIFY_PATH, $sections)
} else {
    [System.IO.File]::SetAccessControl($env:DRMR_QUALIFY_PATH, $acl)
    [System.IO.File]::GetAccessControl($env:DRMR_QUALIFY_PATH, $sections)
}
$rules = @($checked.GetAccessRules($true, $true, [System.Security.Principal.SecurityIdentifier]))
if (-not $checked.AreAccessRulesProtected -or -not $checked.GetOwner([System.Security.Principal.SecurityIdentifier]).Equals($identity) -or $rules.Count -ne 1) {
    throw 'Private ACL verification failed'
}
$actual = $rules[0]
if (-not $actual.IdentityReference.Equals($identity) -or $actual.IsInherited -or $actual.AccessControlType -ne [System.Security.AccessControl.AccessControlType]::Allow -or $actual.FileSystemRights -ne [System.Security.AccessControl.FileSystemRights]::FullControl -or $actual.InheritanceFlags -ne $inheritance -or $actual.PropagationFlags -ne [System.Security.AccessControl.PropagationFlags]::None) {
    throw 'Private ACL verification failed'
}
"""
    environment = dict(os.environ, DRMR_QUALIFY_PATH=str(path), DRMR_QUALIFY_DIRECTORY="1" if directory else "0")
    encoded = base64.b64encode(script.encode("utf-16le")).decode("ascii")
    try:
        result = subprocess.run(["powershell.exe", "-NoLogo", "-NoProfile", "-NonInteractive", "-EncodedCommand", encoded],
                                stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
                                env=environment, timeout=20)
    except (OSError, subprocess.SubprocessError):
        raise Refusal("private-acl-unavailable") from None
    require(result.returncode == 0, "private-acl-unavailable")


def private_write(path, data):
    with path.open("xb") as stream:
        if os.name != "nt":
            os.chmod(path, 0o600)
        secure_windows(path, False)
        stream.write(data)


class Workspace:
    """Cleanup authority comes from this creation, plus the unchanged root identity."""

    def __init__(self, parent=None):
        self.path = Path(tempfile.mkdtemp(prefix="drmr-cli-archive-", dir=parent)).resolve()
        metadata = self.path.lstat()
        self.identity = (metadata.st_dev, metadata.st_ino)
        try:
            secure_windows(self.path, True)
        except BaseException:
            self.close()
            raise

    def close(self):
        try:
            metadata = self.path.lstat()
            require(stat.S_ISDIR(metadata.st_mode) and not self.path.is_symlink()
                    and not getattr(metadata, "st_file_attributes", 0) & 0x400
                    and (metadata.st_dev, metadata.st_ino) == self.identity,
                    "owned-cleanup-unconfirmed")
            shutil.rmtree(self.path)
        except OSError:
            raise Refusal("owned-cleanup-unconfirmed") from None


def snapshot_archive(source, destination):
    """Verify and execute one private snapshot, even if the original is replaced."""
    flags = os.O_RDONLY | getattr(os, "O_NONBLOCK", 0) | getattr(os, "O_NOFOLLOW", 0)
    try:
        require(not source.is_symlink(), "archive-input-invalid")
        with os.fdopen(os.open(source, flags), "rb") as original:
            before = os.fstat(original.fileno())
            require(stat.S_ISREG(before.st_mode) and 0 < before.st_size <= VERIFY.MAX_TOTAL
                    and not getattr(before, "st_file_attributes", 0) & 0x400, "archive-input-invalid")
            size = 0
            with destination.open("xb") as output:
                os.chmod(destination, 0o600)
                while True:
                    block = original.read(min(65536, VERIFY.MAX_TOTAL + 1 - size))
                    if not block:
                        break
                    size += len(block)
                    require(size <= VERIFY.MAX_TOTAL, "archive-input-invalid")
                    output.write(block)
            after = os.fstat(original.fileno())
            require(size == before.st_size and (before.st_dev, before.st_ino, before.st_size, before.st_mtime_ns)
                    == (after.st_dev, after.st_ino, after.st_size, after.st_mtime_ns), "archive-input-changed")
    except OSError:
        raise Refusal("archive-input-invalid") from None


def extract_cli(path, target, expected_digest, destination):
    """Extract only the verified regular executable, never use extractall()."""
    archive = None
    try:
        archive = VERIFY.Archive(path)
        _, _, paths = VERIFY.layout(archive, "cli", target)
        name = paths["drmr"]
        evidence = VERIFY.binary(archive, name, target)
        require(evidence["sha256"] == expected_digest, "extracted-binary-mismatch")
        digest = hashlib.sha256()
        with archive.open(name) as original, destination.open("xb") as output:
            while True:
                block = original.read(65536)
                if not block:
                    break
                digest.update(block)
                output.write(block)
        require(digest.hexdigest() == expected_digest, "extracted-binary-mismatch")
        destination.chmod(0o700)
        return destination
    except ARCHIVE_ERRORS:
        raise Refusal("archive-extraction-invalid") from None
    finally:
        if archive is not None:
            archive.close()


def terminate(process):
    """Ask drmr to reap its independently grouped children before a forced fallback."""
    if process.poll() is not None:
        return
    if os.name == "posix":
        try:
            process.send_signal(signal.SIGTERM)
            process.wait(timeout=5)
            return
        except ProcessLookupError:
            return
        except subprocess.TimeoutExpired:
            pass
    else:
        # taskkill /T is limited to this still-owned process and its children.
        try:
            subprocess.run(["taskkill.exe", "/PID", str(process.pid), "/T", "/F"],
                           stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL,
                           stderr=subprocess.DEVNULL, timeout=10)
        except (OSError, subprocess.SubprocessError):
            pass
    try:
        if os.name == "posix":
            os.killpg(process.pid, signal.SIGKILL)
        else:
            process.kill()
    except ProcessLookupError:
        pass
    except PermissionError:
        if process.poll() is None:
            process.kill()
    try:
        process.wait(timeout=5)
    except subprocess.TimeoutExpired:
        raise Refusal("command-cleanup-unconfirmed") from None
    # Media children can create independent groups. A forced CLI termination
    # cannot establish complete cleanup, so this outcome can never pass.
    raise Refusal("command-cleanup-unconfirmed")


class Runner:
    def __init__(self, workspace, secrets=()):
        self.workspace = workspace
        self.secrets = tuple(secrets)
        self.stages = []
        self.environment = {key: value for key, value in os.environ.items() if not key.startswith("DRMR_")}
        temporary = workspace / "temporary"
        temporary.mkdir(mode=0o700)
        self.environment.update(TMPDIR=str(temporary), TMP=str(temporary), TEMP=str(temporary))

    def run(self, arguments, stage, timeout=300):
        started = time.monotonic()
        deadline = started + timeout
        with tempfile.TemporaryFile(dir=self.workspace) as stdout, tempfile.TemporaryFile(dir=self.workspace) as stderr:
            try:
                process = subprocess.Popen([str(argument) for argument in arguments], cwd=self.workspace,
                                           env=self.environment, stdin=subprocess.DEVNULL,
                                           stdout=stdout, stderr=stderr, start_new_session=os.name == "posix")
            except OSError:
                raise Refusal("command-unavailable", stage) from None
            try:
                while True:
                    require(max(os.fstat(stdout.fileno()).st_size, os.fstat(stderr.fileno()).st_size) <= MAX_OUTPUT,
                            "command-output-limit")
                    status = process.poll()
                    if status is not None:
                        break
                    require(time.monotonic() < deadline, "command-deadline")
                    time.sleep(min(0.01, max(0, deadline - time.monotonic())))
                require(max(os.fstat(stdout.fileno()).st_size, os.fstat(stderr.fileno()).st_size) <= MAX_OUTPUT,
                        "command-output-limit")
                stdout.seek(0)
                stderr.seek(0)
                raw = stdout.read(MAX_OUTPUT + 1)
                diagnostic = stderr.read(MAX_OUTPUT + 1)
                require(not any(value and (value.lower() in raw.lower() or value.lower() in diagnostic.lower())
                                for value in self.secrets), "secret-displayed")
                if status != 0:
                    evidence = {"exit_code": status, "stdout_sha256": hashlib.sha256(raw).hexdigest(),
                                "stderr_sha256": hashlib.sha256(diagnostic).hexdigest()}
                    try:
                        payload = load_json(raw)
                        code = payload.get("error", {}).get("code") if isinstance(payload, dict) else None
                        if code in {"media-tool-failed", "media-tool-unavailable-or-timeout", "temporary-workspace-unavailable",
                                    "key-file-invalid", "native-manifest-lint-failed", "resource-budget",
                                    "execution-plan-invalid", "execution-plan-source-changed", "invalid-input-media",
                                    "round-trip-verification-failed", "source-normalization-verification-failed"}:
                            evidence["fixed_cli_code"] = code
                    except (Refusal, TypeError, AttributeError):
                        pass
                    raise Refusal("command-failed", evidence=evidence)
            except BaseException as error:
                try:
                    terminate(process)
                except Refusal as cleanup:
                    cleanup.stage = stage
                    raise cleanup from None
                if isinstance(error, Refusal):
                    error.stage = stage
                raise
        self.stages.append({"stage": stage, "elapsed_milliseconds": round((time.monotonic() - started) * 1000),
                            "exit_code": 0})
        return raw

    def json(self, arguments, stage, timeout=300):
        return load_json(self.run(arguments, stage, timeout))


def read_regular(path, limit):
    flags = os.O_RDONLY | getattr(os, "O_NONBLOCK", 0) | getattr(os, "O_NOFOLLOW", 0)
    require(not path.is_symlink(), "media-graph-invalid")
    with os.fdopen(os.open(path, flags), "rb") as stream:
        before = os.fstat(stream.fileno())
        require(stat.S_ISREG(before.st_mode) and before.st_size <= limit
                and not getattr(before, "st_file_attributes", 0) & 0x400, "media-graph-limit")
        data = stream.read(limit + 1)
        after = os.fstat(stream.fileno())
        require(len(data) == before.st_size and (before.st_dev, before.st_ino, before.st_size, before.st_mtime_ns)
                == (after.st_dev, after.st_ino, after.st_size, after.st_mtime_ns), "media-graph-changed")
    return data


def graph_identity(root):
    digest = hashlib.sha256()
    files, total, directory_count = [], 0, 0
    for directory, children, names in os.walk(root, followlinks=False):
        directory_count += 1
        require(directory_count <= 64 and len(children) <= 64 and len(names) <= 64, "media-graph-limit")
        for name in children:
            path = Path(directory) / name
            require(not path.is_symlink() and not getattr(path.lstat(), "st_file_attributes", 0) & 0x400,
                    "media-graph-invalid")
        for name in names:
            path = Path(directory) / name
            metadata = path.lstat()
            require(stat.S_ISREG(metadata.st_mode) and not path.is_symlink()
                    and not getattr(metadata, "st_file_attributes", 0) & 0x400, "media-graph-invalid")
            total += metadata.st_size
            files.append(path)
            require(len(files) <= 64 and total <= MAX_GRAPH_BYTES, "media-graph-limit")
    require(bool(files), "media-graph-invalid")
    actual_total = 0
    for path in sorted(files):
        name = path.relative_to(root).as_posix().encode("utf-8")
        data = read_regular(path, MAX_GRAPH_BYTES)
        actual_total += len(data)
        require(actual_total <= MAX_GRAPH_BYTES, "media-graph-limit")
        digest.update(struct.pack(">Q", len(name)))
        digest.update(name)
        digest.update(struct.pack(">Q", len(data)))
        digest.update(hashlib.sha256(data).digest())
    return {"sha256": digest.hexdigest(), "files": len(files), "bytes": actual_total}


def video_evidence(report):
    require(isinstance(report, dict), "video-proof-invalid")
    streams, packets = report.get("streams"), report.get("packets")
    require(isinstance(streams, list) and len(streams) == 1 and isinstance(streams[0], dict)
            and streams[0].get("codec_name") == "h264" and streams[0].get("nb_read_frames") == "240"
            and re.fullmatch(r"1/[1-9][0-9]*", streams[0].get("time_base", "")) is not None
            and isinstance(packets, list) and len(packets) == 240, "video-proof-invalid")
    for packet in packets:
        require(isinstance(packet, dict)
                and all(type(packet.get(name)) is int for name in ("pts", "dts", "duration"))
                and packet["duration"] > 0
                and re.fullmatch(r"SHA256:[0-9a-f]{64}", packet.get("data_hash", "")) is not None,
                "video-proof-invalid")
    return report


def audio_evidence(report):
    require(isinstance(report, dict), "aac44100-proof-invalid")
    streams, packets = report.get("streams"), report.get("packets")
    require(isinstance(streams, list) and len(streams) == 1 and isinstance(streams[0], dict)
            and isinstance(packets, list) and len(packets) == AAC44100_FRAMES,
            "aac44100-proof-invalid")
    stream = streams[0]
    require(stream.get("codec_name") == "aac" and stream.get("profile") == "LC"
            and stream.get("sample_rate") == "44100" and type(stream.get("channels")) is int
            and stream["channels"] == 2 and stream.get("time_base") == "1/90000"
            and stream.get("nb_read_frames") == str(AAC44100_FRAMES), "aac44100-proof-invalid")
    clock = hashlib.sha256()
    previous = -1
    for packet in packets:
        require(isinstance(packet, dict)
                and all(type(packet.get(name)) is int for name in ("pts", "dts", "duration"))
                and previous < packet["pts"] < 1 << 33 and packet["dts"] == packet["pts"]
                and packet["duration"] == 2089, "aac44100-proof-invalid")
        clock.update(struct.pack(">qqq", packet["pts"], packet["dts"], packet["duration"]))
        previous = packet["pts"]
    return {"codec_name": "aac", "profile": "LC", "sample_rate": 44100, "channels": 2,
            "time_base": "1/90000", "frame_count": AAC44100_FRAMES, "packet_count": len(packets),
            "expected_decoded_samples_per_channel": len(packets) * 1024,
            "first_pts": packets[0]["pts"], "first_dts": packets[0]["dts"],
            "last_pts": packets[-1]["pts"], "last_dts": packets[-1]["dts"],
            "last_duration": packets[-1]["duration"], "packet_clock_sha256": clock.hexdigest()}


def verification(report):
    require(isinstance(report, dict) and type(report.get("schema_version")) is int and report.get("schema_version") == 1
            and report.get("passed") is True and report.get("media_verified") is True
            and report.get("license_playback_verified") is False, "media-proof-invalid")
    checks = report.get("checks")
    require(isinstance(checks, dict) and checks.get("stream_metadata") == "pass"
            and all(checks.get(name) is True for name in CHECKS), "media-proof-invalid")
    native = report.get("native_manifest")
    require(isinstance(native, dict) and type(native.get("schema_version")) is int
            and native.get("schema_version") == 1 and native.get("scope") == "manifest-only"
            and native.get("profile") == "dash-static-manifest"
            and type(native.get("errors")) is int and native["errors"] == 0
            and type(native.get("warnings")) is int and native["warnings"] >= 0
            and native.get("incomplete") is False and native.get("truncated") is False
            and type(native.get("document_count")) is int and native["document_count"] == 1,
            "manifest-proof-invalid")
    return {"media_verified": True, "license_playback_verified": False,
            "checks": {"stream_metadata": "pass", **{name: True for name in CHECKS}},
            "native_manifest": {name: native[name] for name in (
                "schema_version", "scope", "profile", "errors", "warnings", "truncated", "incomplete", "document_count")}}


def doctor(report):
    require(isinstance(report, dict) and report.get("schema_version") == 1 and report.get("mode") == "probe"
            and report.get("profile") == "all" and report.get("ready") is True
            and report.get("cleanup_status") == "confirmed" and report.get("cancelled") is False
            and report.get("media_compatibility_checked") is False, "doctor-proof-invalid")
    tools = report.get("tools")
    expected = {"ffmpeg", "ffprobe", "packager"}
    require(isinstance(tools, list) and len(tools) == len(expected)
            and all(isinstance(tool, dict) for tool in tools)
            and {tool.get("name") for tool in tools} == expected, "doctor-proof-invalid")
    result = []
    for tool in tools:
        require(tool.get("available") is True and tool.get("required") is True
                and tool.get("version_status") == "supported"
                and tool.get("feature_status") == "available" and tool.get("failure_code") is None,
                "doctor-proof-invalid")
        # Version banners and executable paths are deliberately not copied.
        result.append({name: tool[name] for name in ("name", "available", "required", "version_status", "feature_status")})
    return {"ready": True, "cleanup_status": "confirmed", "tools": result}


def conversion_proof(report, plan_id):
    require(isinstance(report, dict) and report.get("plan_id") == plan_id
            and isinstance(report.get("input"), dict) and report["input"].get("format") == "dash"
            and report["input"].get("protection") == "clear"
            and isinstance(report.get("output"), dict) and report["output"].get("format") == "dash"
            and report["output"].get("protection") == "cbcs", "conversion-proof-invalid")
    return verification(report.get("verification"))


def audio_conversion_proof(report, plan_id):
    require(isinstance(report, dict) and report.get("plan_id") == plan_id
            and isinstance(report.get("input"), dict) and report["input"].get("format") == "hls"
            and report["input"].get("protection") == "clear"
            and isinstance(report.get("output"), dict) and report["output"].get("format") == "dash"
            and report["output"].get("protection") == "cbcs", "aac44100-conversion-proof-invalid")
    return verification(report.get("verification"))


def planned_intent(intent, output):
    require(isinstance(intent, dict) and set(intent) == {"format", "target", "output"}
            and intent.get("format") == "dash" and intent.get("target") == "cbcs"
            and isinstance(intent.get("output"), str), "plan-proof-invalid")
    # The destination does not exist yet. Rust and Python may retain different
    # Windows extended-path prefixes, so compare its existing parent's identity
    # and exact leaf name instead of the full path's spelling.
    try:
        planned_output = Path(intent["output"])
        require(planned_output.is_absolute() and planned_output.name == output.name
                and planned_output.parent.samefile(output.parent), "plan-proof-invalid")
    except (OSError, ValueError):
        raise Refusal("plan-proof-invalid") from None


def media(cli, runner, source):
    root = runner.workspace
    clip = source.read(CLIP)
    require(len(clip) == CLIP_BYTES and hashlib.sha256(clip).hexdigest() == CLIP_SHA256, "fixture-identity-mismatch")
    private_write(root / "clip.mp4", clip)
    probe = ["ffprobe", "-v", "error", "-select_streams", "v:0", "-count_frames", "-show_packets",
             "-show_streams", "-show_data_hash", "sha256", "-show_entries",
             "stream=codec_name,time_base,nb_read_frames:packet=pts,dts,duration,data_hash", "-of", "json"]
    original = video_evidence(runner.json([*probe, "clip.mp4"], "fixture-probe"))
    runner.run(["ffmpeg", "-v", "error", "-nostdin", "-copyts", "-i", "clip.mp4", "-map", "0:v:0",
                "-an", "-c", "copy", "video.mp4"], "fixture-video")
    require(video_evidence(runner.json([*probe, "video.mp4"], "prepared-video-probe")) == original,
            "prepared-video-changed")
    presentation = root / "dash"
    presentation.mkdir(mode=0o700)
    runner.run(["packager", "in=video.mp4,stream=video,init_segment=dash/init.mp4,segment_template=dash/seg_$Number$.m4s",
                "--segment_duration", "2", "--fragment_duration", "2",
                "--mp4_reset_initial_composition_offset_to_zero=false", "--generate_static_live_mpd",
                "--mpd_output", "dash/manifest.mpd"], "fixture-segments")
    require(len(list(presentation.glob("seg_*.m4s"))) == 5, "five-source-segments-required")
    try:
        WINDOW.preserve_video_window(root / "video.mp4", presentation / "init.mp4")
    except ValueError:
        raise Refusal("prepared-window-invalid") from None
    before = graph_identity(presentation)
    source_path = presentation / "manifest.mpd"
    # Fixture preparation is separately compared against all original video.
    prepared = verification(runner.json([cli, "verify", source_path, "--source", root / "video.mp4",
                                         "--max-workers", "1", "--json"], "prepared-source-verification"))
    analyzed = runner.json([cli, "analyze", source_path, "--max-workers", "1", "--json"], "analyze")
    require(isinstance(analyzed, dict) and analyzed.get("kind") == "input-analysis"
            and analyzed.get("schema_version") == 1 and analyzed.get("input_admitted") is True
            and analyzed.get("actual_format") == "dash" and analyzed.get("track_protection") == ["clear"],
            "analysis-proof-invalid")
    input_checks = analyzed.get("input_checks")
    require(isinstance(input_checks, dict) and input_checks.get("stream_metadata") == "pass"
            and all(input_checks.get(name) is True for name in CHECKS), "analysis-proof-invalid")
    source_inventory = analyzed.get("source")
    require(isinstance(source_inventory, dict) and re.fullmatch(r"[0-9a-f]{64}", source_inventory.get("graph_digest", "")) is not None,
            "analysis-proof-invalid")
    plan_path = root / "conversion.plan.json"
    output = root / "converted"
    planned = runner.json([cli, "plan", source_path, "--to", "cbcs", "--format", "dash", "--output", output,
                           "--save", plan_path, "--max-workers", "1", "--json"], "plan")
    plan_bytes = read_regular(plan_path, 1024 * 1024)
    require(isinstance(planned, dict) and planned == load_json(plan_bytes)
            and planned.get("kind") == "execution-plan" and planned.get("schema_version") == 1
            and re.fullmatch(r"[0-9a-f]{64}", planned.get("plan_id", "")) is not None
            and planned.get("source") == source_inventory and planned.get("source_format") == "dash",
            "plan-proof-invalid")
    planned_intent(planned.get("intent"), output)
    key = secrets.token_hex(16)
    runner.secrets = (key.encode("ascii"),)
    key_path = root / "keys.json"
    private_write(key_path, json.dumps({"schema_version": 1, "output": {"kid": secrets.token_hex(16), "key": key}}).encode("utf-8"))
    converted = runner.json([cli, "convert", "--plan", plan_path, "--keys-file", key_path,
                             "--max-workers", "1", "--json"], "convert")
    converted_proof = conversion_proof(converted, planned["plan_id"])
    verified = runner.json([cli, "verify", "--plan", plan_path, "--keys-file", key_path,
                            "--max-workers", "1", "--json"], "independent-verify")
    require(isinstance(verified, dict) and verified.get("plan_id") == planned["plan_id"]
            and verified.get("source_unchanged") is True and verified.get("output_matches_plan") is True,
            "saved-plan-proof-invalid")
    verified_proof = verification(verified.get("verification"))
    require(before == graph_identity(presentation) and read_regular(plan_path, 1024 * 1024) == plan_bytes,
            "saved-original-or-plan-changed")
    require((output / "manifest.mpd").is_file(), "output-manifest-missing")
    return {"fixture_sha256": CLIP_SHA256, "fixture_duration_seconds": 10,
            "scope": "all 240 original H.264 video packets and frames; audio not selected",
            "input_format": "dash", "input_protection": "clear", "output_format": "dash", "output_protection": "cbcs",
            "segment_target_seconds": 2, "input_segments": 5, "source_graph": before,
            "cli_source_graph_sha256": source_inventory["graph_digest"],
            "plan_schema_version": 1, "plan_id": planned["plan_id"],
            "plan_sha256": hashlib.sha256(plan_bytes).hexdigest(), "saved_original_and_plan_unchanged": True,
            "prepared_source_against_original_video": prepared, "conversion": converted_proof,
            "independent_verification": verified_proof, "output_graph": graph_identity(output)}


def audio_media(cli, runner, source):
    root = runner.workspace
    clip = source.read(CLIP)
    require(len(clip) == CLIP_BYTES and hashlib.sha256(clip).hexdigest() == CLIP_SHA256
            and read_regular(root / "clip.mp4", CLIP_BYTES) == clip, "fixture-identity-mismatch")
    presentation = root / "aac44100-input"
    presentation.mkdir(mode=0o700)
    runner.run(["ffmpeg", "-v", "error", "-nostdin", "-threads", "1", "-i", "clip.mp4",
                "-map", "0:a:0", "-vn", "-c:a", "aac", "-threads", "1", "-ar", "44100", "-ac", "2",
                "-hls_time", "2", "-hls_playlist_type", "vod", "-hls_segment_filename",
                "aac44100-input/segment-%03d.ts", "aac44100-input/media.m3u8"], "fixture-aac44100")
    source_path = presentation / "media.m3u8"
    text = read_regular(source_path, 65536).decode("utf-8")
    resources = [line for line in text.splitlines() if line and not line.startswith("#")]
    require(2 <= len(resources) <= 8
            and all(re.fullmatch(r"segment-[0-9]{3}\.ts", name) is not None for name in resources),
            "aac44100-source-segments-invalid")
    before = graph_identity(presentation)
    audio = audio_evidence(runner.json([
        "ffprobe", "-v", "error", "-protocol_whitelist", "file,crypto", "-select_streams", "a:0",
        "-count_frames", "-show_packets", "-show_streams", "-show_entries",
        "stream=codec_name,profile,sample_rate,channels,time_base,nb_read_frames:packet=pts,dts,duration",
        "-of", "json", source_path], "fixture-aac44100-probe"))
    analyzed = runner.json([cli, "analyze", source_path, "--max-workers", "1", "--json"], "aac44100-analyze")
    require(isinstance(analyzed, dict) and type(analyzed.get("schema_version")) is int
            and analyzed.get("schema_version") == 1 and analyzed.get("kind") == "input-analysis"
            and analyzed.get("input_admitted") is True and analyzed.get("actual_format") == "hls-ts"
            and analyzed.get("track_protection") == ["clear"], "aac44100-analysis-proof-invalid")
    input_checks = analyzed.get("input_checks")
    require(isinstance(input_checks, dict) and input_checks.get("stream_metadata") == "pass"
            and all(input_checks.get(name) is True for name in CHECKS), "aac44100-analysis-proof-invalid")
    source_inventory = analyzed.get("source")
    require(isinstance(source_inventory, dict)
            and re.fullmatch(r"[0-9a-f]{64}", source_inventory.get("graph_digest", "")) is not None,
            "aac44100-analysis-proof-invalid")
    plan_path = root / "aac44100.plan.json"
    output = root / "aac44100-converted"
    planned = runner.json([cli, "plan", source_path, "--to", "cbcs", "--format", "dash", "--output", output,
                           "--save", plan_path, "--max-workers", "1", "--json"], "aac44100-plan")
    plan_bytes = read_regular(plan_path, 1024 * 1024)
    require(isinstance(planned, dict) and planned == load_json(plan_bytes)
            and type(planned.get("schema_version")) is int and planned.get("schema_version") == 1
            and planned.get("kind") == "execution-plan"
            and re.fullmatch(r"[0-9a-f]{64}", planned.get("plan_id", "")) is not None
            and planned.get("source") == source_inventory and planned.get("source_format") == "hls-ts",
            "aac44100-plan-proof-invalid")
    planned_intent(planned.get("intent"), output)
    key = secrets.token_hex(16)
    runner.secrets += (key.encode("ascii"),)
    key_path = root / "aac44100-keys.json"
    private_write(key_path, json.dumps({"schema_version": 1, "output": {
        "kid": secrets.token_hex(16), "key": key}}).encode("utf-8"))
    converted = runner.json([cli, "convert", "--plan", plan_path, "--keys-file", key_path,
                             "--max-workers", "1", "--json"], "aac44100-convert")
    converted_proof = audio_conversion_proof(converted, planned["plan_id"])
    verified = runner.json([cli, "verify", "--plan", plan_path, "--keys-file", key_path,
                            "--max-workers", "1", "--json"], "aac44100-independent-verify")
    require(isinstance(verified, dict) and verified.get("plan_id") == planned["plan_id"]
            and verified.get("source_unchanged") is True and verified.get("output_matches_plan") is True,
            "aac44100-saved-plan-proof-invalid")
    verified_proof = verification(verified.get("verification"))
    require(before == graph_identity(presentation) and read_regular(plan_path, 1024 * 1024) == plan_bytes,
            "aac44100-saved-original-or-plan-changed")
    require((output / "manifest.mpd").is_file(), "output-manifest-missing")
    return {"fixture_sha256": CLIP_SHA256, "fixture_duration_seconds": 10,
            "scope": "all generated 44.1 kHz AAC frames; fixture audio resampled from the attributed clip",
            "input_format": "hls-ts", "input_protection": "clear", "output_format": "dash", "output_protection": "cbcs",
            "segment_target_seconds": 2, "input_segments": len(resources), "source_audio": audio,
            "source_graph": before, "cli_source_graph_sha256": source_inventory["graph_digest"],
            "plan_schema_version": 1, "plan_id": planned["plan_id"],
            "plan_sha256": hashlib.sha256(plan_bytes).hexdigest(), "saved_original_and_plan_unchanged": True,
            "conversion": converted_proof, "independent_verification": verified_proof,
            "output_graph": graph_identity(output)}


def qualify(archive_path, target, version, source_path):
    require_native(target)
    source = VERIFY.Source(source_path)
    require_version(source, version)
    workspace = Workspace()
    try:
        snapshot = workspace.path / "candidate.archive"
        snapshot_archive(archive_path, snapshot)
        try:
            expected = VERIFY.build_expectation(source, "cli", target, VERIFY.metadata(source, "cli", target))
            archive_proof = VERIFY.qualify(snapshot, source, expected)
        except ARCHIVE_ERRORS + (subprocess.SubprocessError,):
            raise Refusal("archive-verification-failed") from None
        require(archive_proof.get("success") is True, "archive-verification-failed")
        suffix = ".exe" if VERIFY.target_info(target)[0] == "windows" else ""
        cli = extract_cli(snapshot, target, archive_proof["binaries"]["drmr"]["sha256"], workspace.path / ("drmr" + suffix))
        runner = Runner(workspace.path)
        require(runner.run([cli, "--version"], "version", 20).strip() == ("drmr " + version).encode("ascii"),
                "binary-version-mismatch")
        tools = doctor(runner.json([cli, "doctor", "--probe", "--route", "all", "--json"], "doctor", 120))
        proof = media(cli, runner, source)
        audio = audio_media(cli, runner, source)
        require(VERIFY.file_digest(snapshot) == archive_proof["archive_sha256"]
                and VERIFY.file_digest(cli) == archive_proof["binaries"]["drmr"]["sha256"], "qualified-artifact-changed")
        return {"schema_version": 1, "success": True, "kind": "native-cli-archive-media",
                "source_head": source.head, "source_tree": source.tree, "target": target,
                "version": version, "binary_reported_version": version,
                "archive_sha256": archive_proof["archive_sha256"],
                "binary_sha256": archive_proof["binaries"]["drmr"]["sha256"],
                "archive_files": archive_proof["files"], "dependency_packages": archive_proof["packages"],
                "native_ordinary_user": True, "doctor": tools, "media": proof, "aac_44100": audio,
                "stages": runner.stages,
                "owned_workspace_cleanup": "confirmed",
                "limits": MEDIA_LIMITS}
    finally:
        workspace.close()


def main(argv=None):
    parser = Parser(description=__doc__)
    parser.add_argument("--archive", type=Path, required=True)
    parser.add_argument("--target", required=True)
    parser.add_argument("--expected-version", required=True)
    parser.add_argument("--source", type=Path, default=ROOT)
    parser.add_argument("--report", type=Path, required=True, help="Create one new JSON report outside the checkout.")
    report_path = None
    try:
        args = parser.parse_args(argv)
        require(not args.report.exists() and not args.report.is_symlink(), "report-destination-exists")
        require(not args.report.resolve().is_relative_to(args.source.resolve()), "report-must-be-outside-source")
        report_path = args.report
        result = qualify(args.archive, args.target, args.expected_version, args.source)
    except Refusal as error:
        result = {"schema_version": 1, "success": False, "kind": "native-cli-archive-media", "failure_code": str(error)}
        if error.stage is not None:
            result["failed_stage"] = error.stage
        if error.evidence is not None:
            result["failure_evidence"] = error.evidence
    except (OSError, ValueError, TypeError, KeyError, VERIFY.Failure, subprocess.SubprocessError):
        result = {"schema_version": 1, "success": False, "kind": "native-cli-archive-media", "failure_code": "input-or-tool-unavailable"}
    except KeyboardInterrupt:
        result = {"schema_version": 1, "success": False, "kind": "native-cli-archive-media", "failure_code": "qualification-interrupted"}
    if report_path is not None:
        try:
            private_write(report_path, (json.dumps(result, indent=2) + "\n").encode("utf-8"))
        except (OSError, Refusal):
            print('{"schema_version":1,"success":false,"failure_code":"report-unavailable"}')
            return 1
    print(json.dumps(result, sort_keys=True))
    return 0 if result["success"] else 1


if __name__ == "__main__":
    sys.exit(main())
