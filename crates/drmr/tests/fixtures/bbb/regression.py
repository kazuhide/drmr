#!/usr/bin/env python3
"""Exercise saved plans and two DRM outputs with the attributed BBB fixture."""

import argparse
import csv
import io
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import platform
import subprocess
import struct
import tempfile
import threading
import time
import zipfile

FIXTURE = Path(__file__).resolve().parent
EVIDENCE_SPEC = importlib.util.spec_from_file_location(
    "media_evidence", FIXTURE.parents[4] / "tools/media_evidence.py")
EVIDENCE = importlib.util.module_from_spec(EVIDENCE_SPEC)
EVIDENCE_SPEC.loader.exec_module(EVIDENCE)
KID = "000102030405060708090a0b0c0d0e0f"
KEY = "10101010101010101010101010101010"
HLS_KID = "303132333435363738393a3b3c3d3e3f"
HLS_KEY = "404142434445464748494a4b4c4d4e4f"


def failure_detail(stderr, stdout):
    errors = []
    for line in stderr.splitlines():
        try:
            row = json.loads(line)
        except ValueError:
            continue
        if isinstance(row, dict) and row.get("level") == "error":
            errors.append(line)
    return "\n".join(errors[-20:]) or (stderr or stdout)[-4000:]


def run(argv, env=None):
    result = subprocess.run([str(arg) for arg in argv], capture_output=True, text=True, env=env)
    if result.returncode:
        raise RuntimeError(f"{argv[0]} exited {result.returncode}: {failure_detail(result.stderr, result.stdout)}")
    return result.stdout


def rebuild_clip(archive_path, output):
    """Reproduce the encoding settings after verifying the official source bytes."""
    metadata = json.loads((FIXTURE / "provenance.json").read_text())
    archive_bytes = archive_path.read_bytes()
    assert hashlib.sha256(archive_bytes).hexdigest() == metadata["source"]["archive_sha256"], "source archive digest changed"
    with zipfile.ZipFile(archive_path) as archive:
        source_bytes = archive.read(metadata["source"]["member"])
    assert hashlib.sha256(source_bytes).hexdigest() == metadata["source"]["member_sha256"], "source movie digest changed"
    with tempfile.TemporaryDirectory(prefix="drmr-bbb-source-") as directory:
        source = Path(directory) / "source.mp4"
        source.write_bytes(source_bytes)
        replacements = {"SOURCE": str(source), "OUTPUT": str(output.resolve())}
        run([replacements.get(arg, arg) for arg in metadata["generation"]["argv"]])
    print(json.dumps({"sha256": hashlib.sha256(output.read_bytes()).hexdigest(),
                      "matches_committed_clip": output.read_bytes() == (FIXTURE / "clip.mp4").read_bytes()}))


def measured_run(argv, env, observe_stdout=None):
    """Sample resident memory separately for the CLI and its living descendants."""
    started = time.perf_counter()
    process = subprocess.Popen([str(arg) for arg in argv], stdout=subprocess.PIPE,
                               stderr=subprocess.PIPE, text=True, env=env)
    finished = threading.Event()
    samples = []

    def sample():
        while not finished.is_set():
            try:
                rows = [tuple(map(int, line.split())) for line in
                        run(["ps", "-axo", "pid=,ppid=,rss="]).splitlines()]
                by_pid = {pid: (parent, rss * 1024) for pid, parent, rss in rows}
                if process.pid in by_pid:
                    descendants = {process.pid}
                    while True:
                        expanded = descendants | {pid for pid, (parent, _) in by_pid.items() if parent in descendants}
                        if expanded == descendants:
                            break
                        descendants = expanded
                    samples.append((by_pid[process.pid][1],
                                    sum(by_pid[pid][1] for pid in descendants - {process.pid})))
            except (OSError, RuntimeError, ValueError):
                pass
            finished.wait(0.1)

    sampler = threading.Thread(target=sample)
    sampler.start()
    stdout, stderr = process.communicate()
    elapsed = time.perf_counter() - started
    finished.set()
    sampler.join()
    if process.returncode:
        raise RuntimeError(f"{Path(argv[0]).name} exited {process.returncode}: {failure_detail(stderr, stdout)}")
    if observe_stdout is not None:
        observe_stdout(stdout)
    memory = {"status": "sampled" if samples else "unmeasured", "sample_interval_seconds": 0.1,
              "samples": len(samples),
              "cli_parent_sampled_peak_rss_bytes": max((row[0] for row in samples), default=None),
              "child_process_tree_sampled_peak_rss_bytes": max((row[1] for row in samples), default=None)}
    if not samples:
        memory["reason"] = "No usable process-table sample was available while the CLI was running."
    return elapsed, memory


def verify_fixture():
    metadata = json.loads((FIXTURE / "provenance.json").read_text())
    clip = FIXTURE / metadata["clip"]["file"]
    data = clip.read_bytes()
    assert hashlib.sha256(data).hexdigest() == metadata["clip"]["sha256"], "clip digest changed"
    probe = json.loads(run(["ffprobe", "-v", "error", "-show_streams", "-show_format", "-of", "json", clip]))
    video = next(stream for stream in probe["streams"] if stream["codec_type"] == "video")
    audio = next(stream for stream in probe["streams"] if stream["codec_type"] == "audio")
    assert video["codec_name"] == "h264" and video["avg_frame_rate"] == "24/1"
    assert audio["codec_name"] == "aac" and audio["profile"] == "LC"
    assert abs(float(probe["format"]["duration"]) - 10) < 0.001
    packets = json.loads(run(["ffprobe", "-v", "error", "-select_streams", "v:0", "-show_packets",
                              "-show_entries", "packet=pts_time,pos,size,flags", "-of", "json", clip]))["packets"]
    # Read the length-prefixed H.264 NAL units, checking actual IDR slices, not just key flags.
    idr_times = []
    width = int(video["nal_length_size"])
    for packet in packets:
        payload = data[int(packet["pos"]):int(packet["pos"]) + int(packet["size"])]
        position = 0
        idr = False
        while position < len(payload):
            size = int.from_bytes(payload[position:position + width], "big")
            position += width
            assert size and position + size <= len(payload), "invalid AVC packet"
            idr |= payload[position] & 31 == 5
            position += size
        if idr:
            assert "K" in packet["flags"]
            idr_times.append(float(packet["pts_time"]))
    assert idr_times == [0, 2, 4, 6, 8], idr_times
    assert len(packets) == 240
    return metadata


def prepare_source(destination):
    """Copy all original H.264/AAC packets into five clear transport segments."""
    destination.mkdir(parents=True)
    playlist = destination / "media.m3u8"
    run(["ffmpeg", "-v", "error", "-nostdin", "-i", FIXTURE / "clip.mp4",
         "-map", "0:v:0", "-map", "0:a:0", "-c", "copy", "-f", "hls",
         "-hls_time", "2", "-hls_playlist_type", "vod", "-hls_segment_filename",
         destination / "segment-%03d.ts", playlist])
    durations = [float(line.split(":", 1)[1].rstrip(",")) for line in
                 playlist.read_text().splitlines() if line.startswith("#EXTINF:")]
    assert durations == [2] * 5, durations
    assert "#EXT-X-KEY:" not in playlist.read_text()
    return playlist


def private_json(path, value):
    # Fixtures contain synthetic signaling only. Protect the empty file before
    # writing it so the CLI exercises the same private-config contract on Windows.
    with os.fdopen(os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600), "w", encoding="utf-8") as file:
        if os.name == "nt":
            row = next(csv.reader(io.StringIO(run(["whoami", "/user", "/fo", "csv", "/nh"]))))
            run(["icacls", path, "/inheritance:r", "/grant:r", f"*{row[1]}:(F)"])
        file.write(json.dumps(value))
    return path


def require_media_verification(report, scope="comparison", manifest=True):
    """Require full coded packets, timestamps, decoded media and native manifests."""
    assert EVIDENCE.v2_proof(report, scope=scope, require_decoded=True) is not None
    if manifest:
        native = report["native_manifest"]
        assert native["errors"] == 0 and native["incomplete"] is False and native["truncated"] is False
        assert native["document_count"] >= 1


def source_fingerprints(root):
    return {path.relative_to(root).as_posix(): hashlib.sha256(path.read_bytes()).hexdigest()
            for path in sorted(root.rglob("*")) if path.is_file()}


def regression(binary, destination):
    metadata = verify_fixture()
    destination = destination.resolve()
    destination.mkdir(parents=True, exist_ok=True)
    source = prepare_source(destination / "source")
    source_before = source_fingerprints(source.parent)
    clean_env = {name: value for name, value in os.environ.items()
                 if not (name.upper() if os.name == "nt" else name).startswith("DRM_")}
    timings, memory, results = {}, {}, {}

    def execute(stage, arguments):
        def capture(stdout):
            assert KEY not in stdout and HLS_KEY not in stdout, "content key leaked"
            results[stage] = json.loads(stdout)
        timings[stage], memory[stage] = measured_run([binary, *arguments], clean_env, capture)
        return results[stage]

    inspected = json.loads(run([binary, "analyze", "--metadata-only", source, "--json"], clean_env))
    assert inspected["format"] == "hls" and inspected["media_verified"] is False
    streams = json.loads(run(["ffprobe", "-v", "error", "-show_streams", "-of", "json",
                              source.parent / "segment-000.ts"]))["streams"]
    assert len(streams) == 2 and {stream["codec_type"] for stream in streams} == {"video", "audio"}
    analysis = execute("analysis", ["analyze", "--decode", source, "--max-workers", "2", "--json"])
    assert analysis["input_admitted"] is True and analysis["actual_format"] == "hls-ts"
    assert all(value == "clear" for value in analysis["track_protection"])
    assert analysis["schema_version"] == 2
    require_media_verification(analysis["verification"], scope="source-admission", manifest=False)

    outputs = private_json(destination / "outputs.json", {"schema_version": 1, "outputs": [
        {"id": "dash", "format": "dash", "target": "cbcs", "drms": ["widevine", "playready"]},
        {"id": "hls", "format": "hls", "target": "cbcs", "drms": ["fairplay"]},
    ]})
    plan_path = destination / "conversion.plan.json"
    output = destination / "result"
    planned = execute("planning", ["plan", "--decode", source, "--outputs", outputs, "--output", output,
                                    "--save", plan_path, "--max-workers", "2", "--json"])
    plan_bytes = plan_path.read_bytes()
    assert planned == json.loads(plan_bytes) and planned["schema_version"] == 4
    assert planned["required_depth"] == "decoded"
    assert planned["kind"] == "execution-plan" and len(planned["outputs"]) == 2
    assert not output.exists()
    # A complete version-one PSSH declares the output KID without opaque test data.
    payload = bytes.fromhex("01000000edef8ba979d64acea3c827dcd51d21ed")
    payload += struct.pack(">I", 1) + bytes.fromhex(KID) + struct.pack(">I", 0)
    pssh = (struct.pack(">I4s", len(payload) + 8, b"pssh") + payload).hex()
    keys = private_json(destination / "keys.json", {"schema_version": 2, "outputs": {
        "dash": {"kid": KID, "key": KEY, "signaling": {"system": "multi-drm",
                 "widevine_pssh": pssh, "playready_license_url": "https://license.example.invalid/playready"}},
        "hls": {"kid": HLS_KID, "key": HLS_KEY, "hls_key_uri": "skd://bbb-regression",
                "signaling": {"system": "multi-drm", "fairplay": True}},
    }})
    converted = execute("packaging", ["convert", "--decode", "--plan", plan_path, "--keys-file", keys,
                                       "--max-workers", "2", "--json"])
    assert converted["kind"] == "bundle-conversion" and converted["plan_id"] == planned["plan_id"]
    assert {row["id"] for row in converted["outputs"]} == {"dash", "hls"}
    for row in converted["outputs"]:
        require_media_verification(row["conversion"]["verification"])
        assert row["signaling"]["verified"] is True
        assert row["signaling"]["systems"] == (["widevine", "playready"] if row["id"] == "dash" else ["fairplay"])
    verified = execute("verification", ["verify", "--decode", "--plan", plan_path, "--keys-file", keys,
                                         "--max-workers", "2", "--json"])
    assert verified["kind"] == "bundle-verification" and verified["plan_id"] == planned["plan_id"]
    assert verified["passed"] is True and verified["source_unchanged"] is True
    assert {row["id"] for row in verified["outputs"]} == {"dash", "hls"}
    for row in verified["outputs"]:
        assert row["output_matches_plan"] is True and row["signaling"]["verified"] is True
        require_media_verification(row["verification"])
    assert source_fingerprints(source.parent) == source_before and plan_path.read_bytes() == plan_bytes
    assert (output / "dash/manifest.mpd").is_file() and (output / "hls/master.m3u8").is_file()
    return {
        "schema_version": 1, "workflow": "saved-plan-multi-output", "success": True,
        "fixture_sha256": metadata["clip"]["sha256"], "video_segments": 5, "packaged_tracks": 2,
        "complete_presentation_identity": "pass", "complete_packet_timing": "pass",
        "source_and_plan_unchanged": True, "plan_id": planned["plan_id"],
        "outputs": verified["outputs"], "timing_seconds": timings, "memory": memory,
        "platform": {"system": platform.system(), "machine": platform.machine()},
        "tools": {name: run([tool, flag]).splitlines()[0]
                  for name, tool, flag in (("ffmpeg", "ffmpeg", "-version"),
                                           ("packager", "packager", "--version"),
                                           ("drmr", binary, "--version"))},
        "evidence": {"packaging": "measured-native", "real_license": "not-verified",
                     "external_conformance": "not-requested", "native_manifest": "pass",
                     "windows": "measured-native" if os.name == "nt" else "not-tested"},
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--verify-fixture", action="store_true")
    parser.add_argument("--rebuild-from-archive", type=Path)
    parser.add_argument("--clip-output", type=Path)
    parser.add_argument("--drmr", type=Path)
    parser.add_argument("--output", type=Path)
    args = parser.parse_args()
    if args.rebuild_from_archive:
        if args.clip_output is None:
            parser.error("--clip-output is required when rebuilding the clip")
        rebuild_clip(args.rebuild_from_archive, args.clip_output)
        return
    if args.verify_fixture:
        verify_fixture()
        return
    if args.drmr is None or args.output is None:
        parser.error("--drmr and --output are required for the regression")
    report = regression(args.drmr.resolve(), args.output)
    if os.environ.get("DRMR_MEDIA_REPORT"):
        Path(os.environ["DRMR_MEDIA_REPORT"]).write_text(json.dumps(report, indent=2) + "\n")
    print(json.dumps(report, sort_keys=True))


if __name__ == "__main__":
    main()
