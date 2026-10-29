#!/usr/bin/env python3
"""Exercise all sixteen protection routes with the attributed ten-second BBB fixture."""

import argparse
import json
import os
from pathlib import Path
import platform
import sys
import time

from regression import FIXTURE, measured_run, private_json, require_media_verification, run, verify_fixture

# Public fixture values, deliberately distinct between source and result roles.
SOURCE_KID = "000102030405060708090a0b0c0d0e0f"
SOURCE_KEY = "10101010101010101010101010101010"
RESULT_KID = "303132333435363738393a3b3c3d3e3f"
RESULT_KEY = "40404040404040404040404040404040"
SOURCE_URI = "https://keys.example.test/bbb/source"
RESULT_URI = "https://keys.example.test/bbb/result"
PROTECTIONS = ("clear", "cenc", "cbcs", "hls-aes128")


def milestone(stage, source=None, target=None):
    message = f"BBB protection routes: {stage}"
    if source is not None:
        message += f" | input={source}"
    if target is not None:
        message += f" | output={target}"
    print(message, file=sys.stderr, flush=True)


def prepare_source(destination):
    destination.mkdir(parents=True)
    streams = []
    for role in ("video", "audio"):
        (destination / role).mkdir()
        streams.append(
            f"in={(FIXTURE / 'clip.mp4').as_posix()},stream={role},"
            f"init_segment={(destination / role).as_posix()}/init.mp4,"
            f"segment_template={(destination / role).as_posix()}/seg_$Number$.m4s")
    run(["packager", "--single_threaded", *streams, "--segment_duration", "2", "--fragment_duration", "2",
         "--mp4_reset_initial_composition_offset_to_zero=false", "--generate_static_live_mpd",
         "--mpd_output", destination / "manifest.mpd"])
    assert len(list((destination / "video").glob("seg_*.m4s"))) == 5
    return destination / "manifest.mpd"


def output_options(protection, kid, key, uri):
    if protection == "clear":
        return None
    if protection == "hls-aes128":
        return {"key": key, "hls_key_uri": uri}
    return {"kid": kid, "key": key}


def convert(binary, source, protection, destination, keys, environment):
    format_name = "hls" if protection == "hls-aes128" else "dash"
    processing = {}

    def inspect_result(stdout):
        actual = json.loads(stdout)["processing"]
        assert actual["route"] == ("fmp4-hls-presentation" if format_name == "hls" else "dash-presentation")
        assert actual["input_preparation"] == ("local-hls-resources" if source.suffix == ".m3u8" else "local-dash-resources")
        assert actual["required_tools"] == ["ffmpeg", "ffprobe", "shaka-packager"]
        processing.update({name: actual[name] for name in ("route", "input_preparation", "required_tools")})

    seconds, memory = measured_run(
        [binary, "convert", "--decode", source, "--to", protection, "--format", format_name,
         "--output", destination, "--keys-file", keys, "--json"], environment, inspect_result)
    manifest = destination / ("master.m3u8" if format_name == "hls" else "manifest.mpd")
    assert manifest.is_file(), "conversion must publish a usable entry manifest"
    return manifest, seconds, memory, processing


def regression(binary, destination):
    milestone("checking fixture")
    metadata = verify_fixture()
    destination.mkdir(parents=True, exist_ok=True)
    environment = {name: value for name, value in os.environ.items() if not (name.upper() if os.name == "nt" else name).startswith("DRM_")}
    milestone("preparing source", target="clear")
    clear = prepare_source(destination / "clear-source")
    sources = {"clear": clear}
    for protection in PROTECTIONS[1:]:
        config = {"schema_version": 1,
                  "output": output_options(protection, SOURCE_KID, SOURCE_KEY, SOURCE_URI)}
        keys = private_json(destination / f"prepare-{protection}.json", config)
        milestone("preparing source", target=protection)
        sources[protection], _, _, _ = convert(binary, clear, protection,
                                           destination / f"source-{protection}", keys, environment)
    rows = []
    for source_protection, source in sources.items():
        for target in PROTECTIONS:
            config = {"schema_version": 1, "input": {}}
            if source_protection == "hls-aes128":
                config["input"]["hls_keys"] = [{"uri": SOURCE_URI, "key": SOURCE_KEY}]
            elif source_protection != "clear":
                config["input"]["keys"] = [{"kid": SOURCE_KID, "key": SOURCE_KEY}]
            output = output_options(target, RESULT_KID, RESULT_KEY, RESULT_URI)
            if output is not None:
                config["output"] = output
            name = f"{source_protection}-to-{target}"
            keys = private_json(destination / f"{name}.json", config)
            milestone("converting", source_protection, target)
            result, elapsed, memory, processing = convert(binary, source, target,
                                               destination / name, keys, environment)
            started = time.perf_counter()
            milestone("independent verification", source_protection, target)
            verification = json.loads(run([binary, "verify", "--decode", result, "--source", source,
                                            "--keys-file", keys, "--json"], environment))
            verification_seconds = time.perf_counter() - started
            require_media_verification(verification)
            milestone("route passed", source_protection, target)
            rows.append({"input": source_protection, "output": target, "passed": True,
                         "conversion_seconds": elapsed, "verification_seconds": verification_seconds,
                         "memory": memory, "verification": verification, "processing": processing})
    milestone("checking tool versions")
    return {"schema_version": 1, "success": True, "routes": rows,
            "fixture_sha256": metadata["clip"]["sha256"], "video_duration_seconds": 10,
            "source_video_segments": 5, "source_video_segment_seconds": 2,
            "baseline": "clear fragmented DASH adapter; source decoder audio padding is retained",
            "platform": {"system": platform.system(), "machine": platform.machine()},
            "tools": {name: run([tool, flag]).splitlines()[0]
                      for name, tool, flag in (("ffmpeg", "ffmpeg", "-version"),
                                               ("packager", "packager", "--version"),
                                               ("drmr", binary, "--version"))},
            "evidence": {"real_license": "not-verified", "device_playback": "not-verified",
                         "independent_conformance": "not-verified",
                         "windows": "measured-native" if os.name == "nt" else "not-tested"}}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--drmr", required=True, type=Path)
    parser.add_argument("--output", required=True, type=Path)
    arguments = parser.parse_args()
    report = regression(arguments.drmr.resolve(), arguments.output.resolve())
    if os.environ.get("DRMR_CONVERSION_REPORT"):
        Path(os.environ["DRMR_CONVERSION_REPORT"]).write_text(json.dumps(report, indent=2) + "\n")
    print(json.dumps(report, sort_keys=True))
    milestone("passed")


if __name__ == "__main__":
    try:
        main()
    except Exception:
        milestone("failed")
        raise SystemExit(1) from None
