#!/usr/bin/env python3
"""Prepare clear adapters and run the attributed BBB clip through the real drmr CLI."""

import argparse
import csv
import io
import hashlib
import json
import os
from pathlib import Path
import platform
import subprocess
import tempfile
import threading
import time
import zipfile

FIXTURE = Path(__file__).resolve().parent
KID = "000102030405060708090a0b0c0d0e0f"
KEY = "10101010101010101010101010101010"
IV = "20202020202020202020202020202020"


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


def measured_run(argv, env):
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


def prepare_adapters(destination):
    destination.mkdir(parents=True)
    for kind in ("video", "audio"):
        (destination / kind).mkdir()
    streams = [
        f"in={FIXTURE / 'clip.mp4'},stream=video,init_segment={destination / 'video/init.mp4'},"
        f"segment_template={destination / 'video/seg_$Number$.m4s'},playlist_name=video/media.m3u8",
        f"in={FIXTURE / 'clip.mp4'},stream=audio,init_segment={destination / 'audio/init.mp4'},"
        f"segment_template={destination / 'audio/seg_$Number$.m4s'},playlist_name=audio/media.m3u8,"
        "hls_group_id=audio,hls_name=English,language=en",
    ]
    run(["packager", *streams, "--segment_duration", "2", "--fragment_duration", "2",
         "--hls_master_playlist_output", destination / "master.m3u8",
         "--mpd_output", destination / "source.mpd", "--generate_static_live_mpd"])
    video = (destination / "video/media.m3u8").read_text()
    durations = [float(line.split(":", 1)[1].rstrip(",")) for line in video.splitlines() if line.startswith("#EXTINF:")]
    assert durations == [2] * 5, durations
    for kind in ("video", "audio"):
        assert "#EXT-X-KEY:" not in (destination / kind / "media.m3u8").read_text()
    assert "ContentProtection" not in (destination / "source.mpd").read_text()
    return destination / "master.m3u8"


def private_json(path, value):
    # Fixtures contain synthetic signaling only. Protect the empty file before
    # writing it so the CLI exercises the same private-config contract on Windows.
    with os.fdopen(os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600), "w", encoding="utf-8") as file:
        if os.name == "nt":
            row = next(csv.reader(io.StringIO(run(["whoami", "/user", "/fo", "csv", "/nh"]))))
            run(["icacls", path, "/inheritance:r", "/grant:r", f"*{row[1]}:(F)"])
        file.write(json.dumps(value))
    return path


def verify_complete_presentations(analysis, snapshot, asset, destination):
    """Decode each continuous presentation, applying initial audio trimming only once."""
    proofs = []
    for track in analysis["tracks"]:
        source = destination / f"source-{track['id']}-complete.mp4"
        source.write_bytes((snapshot / track["init_path"]).read_bytes() +
                           b"".join((snapshot / segment["path"]).read_bytes() for segment in track["segments"]))
        folder = asset / "cbcs" / track["id"]
        encrypted = destination / f"encrypted-{track['id']}-complete.mp4"
        encrypted.write_bytes((folder / "init.mp4").read_bytes() +
                              b"".join((folder / f"seg_{index}.m4s").read_bytes() for index in range(track["segment_count"])))

        def decoded(path, decrypt):
            argv = ["ffmpeg", "-v", "error", "-threads", "1"]
            if decrypt:
                argv += ["-decryption_key", KEY]
            argv += ["-i", str(path), "-map", "0:0"]
            if track["content_type"] == "audio":
                argv += ["-f", "s32le", "-acodec", "pcm_s32le", "-"]
            else:
                argv += ["-fps_mode", "passthrough", "-f", "framemd5", "-"]
            output = subprocess.run(argv, capture_output=True, check=True).stdout
            if track["content_type"] == "video":
                return [line.split(b",")[-2:] for line in output.splitlines() if line and not line.startswith(b"#")]
            return output

        source_decoded, output_decoded = decoded(source, False), decoded(encrypted, True)
        assert source_decoded == output_decoded, f"complete {track['content_type']} presentation changed"
        def digest(value):
            if isinstance(value, list):
                value = b"\n".join(b",".join(row) for row in value)
            return hashlib.sha256(value).hexdigest()
        proofs.append({"track_id": track["id"], "content_type": track["content_type"],
                       "source_decoded_sha256": digest(source_decoded),
                       "output_decoded_sha256": digest(output_decoded),
                       "source_mp4_sha256": hashlib.sha256(source.read_bytes()).hexdigest(),
                       "encrypted_mp4_sha256": hashlib.sha256(encrypted.read_bytes()).hexdigest(),
                       "decoded_frames" if track["content_type"] == "video" else "decoded_samples_per_channel":
                           len(source_decoded) if track["content_type"] == "video" else len(source_decoded) // 8})
        timing = []
        for path, decrypt in ((source, False), (encrypted, True)):
            argv = ["ffprobe", "-v", "error"]
            if decrypt:
                argv += ["-decryption_key", KEY]
            argv += ["-show_packets", "-show_entries", "packet=pts_time,dts_time,duration_time", "-of", "json", path]
            timing.append(json.loads(run(argv))["packets"])
        assert len(timing[0]) == len(timing[1])
        for before, after in zip(*timing):
            for field in ("pts_time", "dts_time", "duration_time"):
                shift = 0 if field == "duration_time" else 2
                assert abs(float(after[field]) - float(before[field]) - shift) < 1 / 45000, field
    return proofs


def regression(binary, destination):
    metadata = verify_fixture()
    destination = destination.resolve()
    destination.mkdir(parents=True, exist_ok=True)
    master = prepare_adapters(destination / "source")
    config = private_json(destination / "source.json", {
        "url": str(master), "policy": {"allowed_local_roots": [str(master.parent)]},
    })
    clean_env = {name: value for name, value in os.environ.items() if not name.startswith("DRM_")}
    analysis_seconds, analysis_memory = measured_run(
        [binary, "analyze-hls", "--source-config", config, "--asset-id", "bbb", "--output", destination / "output"], clean_env)
    asset = destination / "output/bbb"
    analysis_path = asset / "clear/source/analysis.json"
    analysis = json.loads(analysis_path.read_text())
    assert len(analysis["tracks"]) == 2
    video = next(track for track in analysis["tracks"] if track["content_type"] == "video")
    assert video["segment_count"] == 5
    drm = private_json(destination / "drm.json", {
        "widevine_pssh": "1210" + KID,
        "playready_la_url": "https://license.example.invalid/rightsmanager.asmx",
        "fairplay_uri": "skd://bbb-regression",
    })
    summary = asset / "reports/package-hls-summary.json"
    try:
        packaging_seconds, packaging_memory = measured_run(
            [binary, "package-hls", "--analysis", analysis_path, "--drm-config", drm, "--max-workers", "2"],
            {**clean_env, "DRM_KID": KID, "DRM_KEY": KEY, "DRM_IV": IV})
    except RuntimeError as error:
        if summary.exists():
            failed = json.loads(summary.read_text())
            raise RuntimeError(f"packaging findings: {failed['findings']}") from error
        raise
    report = json.loads(summary.read_text())
    assert report["success"] and not report["production_verified"], report
    assert len(report["tracks"]) == 2
    for track in report["tracks"]:
        assert track["disposition"] == "packaged"
        assert {"source-correspondence-pass", "timing-pass", "decoded-identity-pass", "cbcs-signaling-pass"} <= set(track["checks"])
        assert len(track["segments"]) > 0
    presentation_proofs = verify_complete_presentations(analysis, analysis_path.parent, asset, destination)
    return {
        "schema_version": "1", "fixture_sha256": metadata["clip"]["sha256"], "success": True,
        "video_segments": video["segment_count"], "packaged_tracks": len(report["tracks"]),
        "complete_presentation_identity": "pass", "complete_packet_timing": "pass",
        "presentation_proofs": presentation_proofs,
        "timing_seconds": {"analysis": analysis_seconds, "packaging": packaging_seconds},
        "memory": {"analysis": analysis_memory, "packaging": packaging_memory},
        "platform": {"system": platform.system(), "machine": platform.machine()},
        "tools": {name: run([tool, flag]).splitlines()[0]
                  for name, tool, flag in (("ffmpeg", "ffmpeg", "-version"),
                                           ("packager", "packager", "--version"),
                                           ("drmr", binary, "--version"))},
        "evidence": {"packaging": "measured-local", "real_license": "not-verified",
                     "conformance": "not-verified", "windows": "measured-native" if os.name == "nt" else "not-tested"},
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
