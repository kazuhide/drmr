#!/usr/bin/env python3
"""Prepare a synthetic nine-track Big Buck Bunny input for worker comparisons.

Requires Python 3.9+, FFmpeg and Shaka Packager. The attributed repository clip is
the only source. Eight identical 720p video renditions measure parallel work;
they are not an adaptive-bitrate ladder or an image-quality benchmark.
"""

import argparse
import hashlib
import json
from pathlib import Path
import struct
import subprocess
import sys

FIXTURE_SHA256 = "a0ef6d20f2319c57af35d9c3165e46b669c67b951268cf882dbfff14b5b26e71"
FIXTURE_BYTES = 195472
ATTRIBUTION = """# Big Buck Bunny benchmark media

(c) copyright 2008, Blender Foundation / www.bigbuckbunny.org

Source: https://peach.blender.org/about/
License: Creative Commons Attribution 3.0
https://creativecommons.org/licenses/by/3.0/

This adapted excerpt covers 00:30–00:40 of the official 320x180 movie.
The repository excerpt was re-encoded as H.264 and AAC-LC. This generator scales
video to 1280x720, repeats it as eight identical renditions, and segments the
presentation. This is a synthetic workload, not an adaptive-bitrate ladder.
Keep this attribution and license link when redistributing the media or outputs.
"""


class Failure(Exception):
    """A fixed classification without operator paths or tool diagnostics."""


def prepare(output):
    # Packager descriptors use commas as separators, even inside an argv item.
    if any(character in str(output) for character in ",\r\n\x00"):
        raise Failure("output-path-not-supported")
    fixture = Path(__file__).resolve().parents[1] / "crates/drmr/tests/fixtures/bbb/clip.mp4"
    # Keep the accepted public clip bounded and snapshot the exact checked bytes.
    with fixture.open("rb") as source:
        clip = source.read(FIXTURE_BYTES + 1)
    if len(clip) != FIXTURE_BYTES or hashlib.sha256(clip).hexdigest() != FIXTURE_SHA256:
        raise Failure("fixture-identity-mismatch")
    try:
        output.mkdir(mode=0o700)
    except FileExistsError:
        raise Failure("output-already-exists") from None
    snapshot = output / "source-clip.mp4"
    snapshot.write_bytes(clip)
    (output / "ATTRIBUTION.md").write_text(ATTRIBUTION, encoding="utf-8")
    presentation = output / "source"
    presentation.mkdir()
    video = output / "video.mp4"
    with (output / "generation.log").open("wb") as log:
        def run(argv):
            try:
                subprocess.run([str(arg) for arg in argv], stdin=subprocess.DEVNULL,
                               stdout=log, stderr=log, timeout=180, check=True)
            except (OSError, subprocess.SubprocessError):
                raise Failure("media-tool-failed") from None

        run(["ffmpeg", "-v", "error", "-nostdin", "-threads", "1", "-i", snapshot,
             "-map", "0:v:0", "-an", "-vf", "scale=1280:720", "-c:v", "libx264",
             "-threads", "1", "-preset", "veryfast", "-g", "48", "-keyint_min", "48",
             "-sc_threshold", "0", "-pix_fmt", "yuv420p", video])
        streams = []
        for index in range(8):
            directory = presentation / ("v" + str(index))
            directory.mkdir()
            streams.append("input={},stream=video,init_segment={}/init.mp4,"
                           "segment_template={}/$Number$.m4s".format(video, directory, directory))
        audio = presentation / "a0"
        audio.mkdir()
        streams.append("input={},stream=audio,init_segment={}/init.mp4,"
                       "segment_template={}/$Number$.m4s".format(snapshot, audio, audio))
        run(["packager", *streams, "--segment_duration", "2", "--fragment_duration", "2",
             "--mp4_reset_initial_composition_offset_to_zero=false", "--generate_static_live_mpd",
             "--single_threaded", "--io_cache_size=0", "--mpd_output", presentation / "manifest.mpd"])

    digest = hashlib.sha256()
    resources = []
    for path in sorted(presentation.rglob("*")):
        if not path.is_file():
            continue
        relative = path.relative_to(presentation).as_posix()
        file_digest = hashlib.sha256()
        size = 0
        with path.open("rb") as source:
            while True:
                chunk = source.read(65536)
                if not chunk:
                    break
                size += len(chunk)
                file_digest.update(chunk)
        name = relative.encode("utf-8")
        digest.update(struct.pack(">Q", len(name)))
        digest.update(name)
        digest.update(struct.pack(">Q", size))
        digest.update(file_digest.digest())
        resources.append({"resource": relative, "bytes": size, "sha256": file_digest.hexdigest()})
    report = {"schema_version": 1, "success": True, "fixture_sha256": FIXTURE_SHA256,
              "video_renditions": 8, "audio_tracks": 1, "video_width": 1280, "video_height": 720,
              "duration_seconds": 10, "video_segment_target_seconds": 2,
              "synthetic_identical_video_renditions": True,
              "graph_sha256": digest.hexdigest(), "resources": resources}
    with (output / "input-report.json").open("x", encoding="utf-8") as target:
        json.dump(report, target, indent=2)
        target.write("\n")
    return report


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, required=True, help="New directory in an existing parent.")
    args = parser.parse_args(argv)
    try:
        report = prepare(args.output.absolute())
    except Failure as error:
        report = {"schema_version": 1, "success": False, "error": str(error)}
    except OSError:
        report = {"schema_version": 1, "success": False, "error": "fixture-or-output-unavailable"}
    print(json.dumps({key: value for key, value in report.items() if key != "resources"}, sort_keys=True))
    return 0 if report["success"] else 1


if __name__ == "__main__":
    sys.exit(main())
