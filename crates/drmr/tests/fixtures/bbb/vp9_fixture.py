#!/usr/bin/env python3
"""Offline generation and independent inspection of the attributed VP9 seeds."""
import argparse
from fractions import Fraction
import hashlib
import json
from pathlib import Path
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parent
SOURCE_SHA256 = "a0ef6d20f2319c57af35d9c3165e46b669c67b951268cf882dbfff14b5b26e71"
PROFILES = ((0, 8), (1, 8), (2, 10), (2, 12), (3, 10), (3, 12))


def command(argv):
    result = subprocess.run([str(value) for value in argv], capture_output=True, check=True)
    return result.stdout


def arguments(profile, depth, output, pass_number):
    pixel = "yuv444p" if profile & 1 else "yuv420p"
    if depth != 8:
        pixel += f"{depth}le"
    args = ["ffmpeg", "-v", "error", "-nostdin", "-y", "-threads", "1", "-filter_threads", "1",
            "-i", "SOURCE", "-map", "0:v:0", "-an", "-c:v", "libvpx-vp9",
            "-pix_fmt", pixel, "-profile:v", str(profile), "-b:v", "100k", "-crf", "35",
            "-deadline", "good", "-cpu-used", "4", "-threads", "1", "-row-mt", "0",
            "-g", "48", "-keyint_min", "48", "-lag-in-frames", "25", "-auto-alt-ref", "1",
            "-arnr-maxframes", "7", "-tile-columns", "0", "-frame-parallel", "1",
            "-pass", str(pass_number), "-passlogfile", "PASS_LOG"]
    if pass_number == 1:
        return args + ["-f", "null", "-"]
    return args + ["-tag:v", "vp09", "-map_metadata", "-1",
                   "-metadata", "title=Big Buck Bunny - ten-second VP9 regression excerpt",
                   "-metadata", "artist=Blender Foundation",
                   "-metadata", "copyright=(c) copyright 2008, Blender Foundation / www.bigbuckbunny.org",
                   "-metadata", "comment=Adapted from the attributed regression clip; CC BY 3.0 https://creativecommons.org/licenses/by/3.0/",
                   "-movflags", "+faststart", output]


def frames(sample):
    """Inspect the independent tail index, without relying on drmr's parser."""
    marker = sample[-1]
    if marker & 0xe0 != 0xc0:
        return [sample], False
    count, width = (marker & 7) + 1, ((marker >> 3) & 3) + 1
    index_size = 2 + count * width
    assert len(sample) >= index_size and sample[-index_size] == marker
    position = len(sample) - index_size + 1
    sizes = [int.from_bytes(sample[position + i * width:position + (i + 1) * width], "little")
             for i in range(count)]
    assert all(sizes) and sum(sizes) + index_size == len(sample)
    result, position = [], 0
    for size in sizes:
        result.append(sample[position:position + size])
        position += size
    return result, True


def frame_flags(frame):
    bits = [(byte >> shift) & 1 for byte in frame[:2] for shift in range(7, -1, -1)]
    assert bits[:2] == [1, 0]
    profile = bits[2] | (bits[3] << 1)
    at = 4
    if profile == 3:
        assert bits[at] == 0
        at += 1
    if bits[at]:
        return profile, False, True, True
    return profile, bits[at + 1] == 0, bits[at + 2] == 1, False


def describe(path, profile, depth):
    value = json.loads(command(["ffprobe", "-v", "error", "-select_streams", "v:0",
                               "-count_frames", "-show_streams", "-show_packets", "-show_entries",
                               "stream=codec_name,profile,codec_tag_string,pix_fmt,width,height,nb_read_frames,time_base:packet=pos,size,pts,dts,duration,flags",
                               "-of", "json", path]))
    fields = ("codec_name", "profile", "codec_tag_string", "pix_fmt", "width", "height",
              "nb_read_frames", "time_base")
    stream = {key: value["streams"][0][key] for key in fields}
    assert stream["codec_name"] == "vp9" and stream["codec_tag_string"] == "vp09"
    assert stream["width"] == 320 and stream["height"] == 180
    assert int(stream["nb_read_frames"]) == 240 and len(value["packets"]) == 240
    data = path.read_bytes()
    config_at = data.index(b"vpcC")
    config_size = int.from_bytes(data[config_at - 4:config_at], "big")
    config = data[config_at + 4:config_at - 4 + config_size]
    assert len(config) == 12 and config[:4] == b"\x01\x00\x00\x00"
    assert config[4] == profile and config[6] >> 4 == depth and config[-2:] == b"\x00\x00"
    coded = hidden = existing = indexed = largest = 0
    keys = []
    for packet in value["packets"]:
        sample = data[int(packet["pos"]):int(packet["pos"]) + int(packet["size"])]
        subframes, has_index = frames(sample)
        indexed += has_index
        largest = max(largest, len(subframes))
        coded += len(subframes)
        displayed = 0
        for ordinal, frame in enumerate(subframes):
            actual_profile, key, show, show_existing = frame_flags(frame)
            assert actual_profile == profile
            hidden += not show
            existing += show_existing
            displayed += show
            if ordinal == 0 and key:
                assert "K" in packet["flags"]
                keys.append(str(int(packet["pts"]) * Fraction(stream["time_base"])))
        assert displayed == 1, "one displayed picture per MP4 sample is required"
    assert hidden > 0 and indexed > 0, "the fixture must exercise ordinary altref superframes"
    assert keys == ["0", "2", "4", "6", "8"], keys
    decoded = command(["ffmpeg", "-v", "error", "-nostdin", "-xerror", "-threads", "1", "-i", path,
                       "-map", "0:v:0", "-threads", "1", "-fps_mode", "passthrough", "-c:v", "rawvideo",
                       "-f", "hash", "-hash", "sha256", "-"]).decode().strip()
    assert decoded.startswith("SHA256=") and len(decoded) == 71
    return {"file": path.name, "profile": profile, "bit_depth": depth,
            "sha256": hashlib.sha256(data).hexdigest(), "bytes": len(data), "stream": stream,
            "vpcc_payload_hex": config.hex(),
            "samples": len(value["packets"]), "coded_frames": coded, "hidden_frames": hidden,
            "show_existing_frames": existing, "superframe_samples": indexed,
            "max_frames_per_sample": largest, "key_sample_times": keys,
            "decoded_video_sha256": decoded.split("=", 1)[1]}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--rebuild", type=Path, metavar="OUTPUT_DIRECTORY")
    args = parser.parse_args()
    assert hashlib.sha256((ROOT / "clip.mp4").read_bytes()).hexdigest() == SOURCE_SHA256
    if args.rebuild:
        args.rebuild.mkdir(parents=True, exist_ok=True)
        for profile, depth in PROFILES:
            with tempfile.TemporaryDirectory(prefix="vp9-pass-") as temporary:
                for number in (1, 2):
                    argv = arguments(profile, depth, args.rebuild / f"vp9-p{profile}-{depth}.mp4", number)
                    argv[argv.index("SOURCE")] = ROOT / "clip.mp4"
                    argv[argv.index("PASS_LOG")] = Path(temporary) / "stats"
                    command(argv)
        rows = [describe(args.rebuild / f"vp9-p{p}-{d}.mp4", p, d) for p, d in PROFILES]
    else:
        metadata = json.loads((ROOT / "vp9-provenance.json").read_text())
        rows = [describe(ROOT / row["file"], row["profile"], row["bit_depth"]) for row in metadata["seeds"]]
        assert rows == metadata["seeds"], "fixture identity or decoded media changed"
    print(json.dumps(rows, indent=2))


if __name__ == "__main__":
    main()
