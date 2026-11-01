#!/usr/bin/env python3
"""Offline rebuild and independent checks for the attributed HEVC test seeds."""
import argparse
import hashlib
import json
from pathlib import Path
import subprocess

ROOT = Path(__file__).resolve().parent
SOURCE_SHA256 = "a0ef6d20f2319c57af35d9c3165e46b669c67b951268cf882dbfff14b5b26e71"


def command(argv):
    result = subprocess.run([str(value) for value in argv], capture_output=True, check=True)
    return result.stdout


def arguments(profile, output):
    ten = profile == "main10"
    return ["ffmpeg", "-v", "error", "-nostdin", "-y", "-threads", "1",
            "-i", "SOURCE", "-map", "0:v:0", "-an", "-c:v", "libx265",
            "-preset", "medium", "-crf", "30", "-pix_fmt", "yuv420p10le" if ten else "yuv420p",
            "-profile:v", "main10" if ten else "main", "-threads", "1",
            "-x265-params", "pools=1:frame-threads=1:wpp=1:keyint=48:min-keyint=48:scenecut=0:"
            "bframes=2:b-adapt=0:log-level=error:" +
            ("open-gop=1:repeat-headers=1" if ten else "open-gop=0:slices=2"),
            "-tag:v", "hev1" if ten else "hvc1", "-map_metadata", "-1",
            "-metadata", "title=Big Buck Bunny - ten-second HEVC regression excerpt",
            "-metadata", "artist=Blender Foundation",
            "-metadata", "copyright=(c) copyright 2008, Blender Foundation / www.bigbuckbunny.org",
            "-metadata", "comment=Adapted from the attributed regression clip; CC BY 3.0 https://creativecommons.org/licenses/by/3.0/",
            "-movflags", "+faststart", output]


def describe(path):
    value = json.loads(command(["ffprobe", "-v", "error", "-select_streams", "v:0",
                               "-show_streams", "-show_packets", "-show_entries",
                               "stream=codec_name,profile,codec_tag_string,pix_fmt,width,height,has_b_frames,extradata_size:packet=pos,size,pts,dts,duration,flags",
                               "-of", "json", path]))
    stream = {key: value["streams"][0][key] for key in
              ("codec_name", "profile", "codec_tag_string", "width", "height",
               "has_b_frames", "pix_fmt", "extradata_size")}
    data = path.read_bytes()
    at = data.index(b"hvcC")
    config = data[at + 4:at - 4 + int.from_bytes(data[at - 4:at], "big")]
    at = 23
    config_types = {}
    for _ in range(config[22]):
        kind = config[at] & 63
        count = int.from_bytes(config[at + 1:at + 3], "big")
        at += 3
        config_types[str(kind)] = count
        for _ in range(count):
            size = int.from_bytes(config[at:at + 2], "big")
            at += 2 + size
            assert at <= len(config), "invalid hvcC NAL array"
    assert at == len(config)
    types = {}
    for packet in value["packets"]:
        payload = data[int(packet["pos"]):int(packet["pos"]) + int(packet["size"])]
        at = 0
        while at < len(payload):
            size = int.from_bytes(payload[at:at + 4], "big")
            at += 4
            assert size >= 2 and at + size <= len(payload), "invalid HEVC length-prefixed NAL"
            kind = (payload[at] >> 1) & 63
            types[kind] = types.get(kind, 0) + 1
            at += size
        assert at == len(payload)
    assert stream["codec_name"] == "hevc" and len(value["packets"]) == 240
    assert stream["width"] == 320 and stream["height"] == 180
    assert stream["has_b_frames"] > 0
    assert 39 in types or "39" in config_types, "the seed must retain ordinary prefix SEI"
    return {"file": path.name, "sha256": hashlib.sha256(data).hexdigest(), "bytes": len(data),
            "packets": len(value["packets"]), "stream": stream, "configuration_nal_types": config_types,
            "sample_nal_types": {str(kind): count for kind, count in sorted(types.items())}}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--rebuild", type=Path, metavar="OUTPUT_DIRECTORY")
    args = parser.parse_args()
    assert hashlib.sha256((ROOT / "clip.mp4").read_bytes()).hexdigest() == SOURCE_SHA256
    if args.rebuild:
        args.rebuild.mkdir(parents=True, exist_ok=True)
        for profile in ("main", "main10"):
            argv = arguments(profile, args.rebuild / f"hevc-{profile}.mp4")
            argv[argv.index("SOURCE")] = ROOT / "clip.mp4"
            command(argv)
        rows = [describe(args.rebuild / f"hevc-{profile}.mp4") for profile in ("main", "main10")]
    else:
        metadata = json.loads((ROOT / "hevc-provenance.json").read_text())
        rows = [describe(ROOT / row["file"]) for row in metadata["seeds"]]
        assert rows == metadata["seeds"], "fixture identity or structure changed"
    print(json.dumps(rows, indent=2))


if __name__ == "__main__":
    main()
