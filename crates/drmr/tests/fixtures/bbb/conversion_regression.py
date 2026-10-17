#!/usr/bin/env python3
"""Exercise all sixteen protection routes with the attributed ten-second BBB fixture."""

import argparse
import json
import os
from pathlib import Path
import platform
import time

from regression import FIXTURE, measured_run, private_json, run, verify_fixture

# Public fixture values, deliberately distinct between source and result roles.
SOURCE_KID = "000102030405060708090a0b0c0d0e0f"
SOURCE_KEY = "10101010101010101010101010101010"
RESULT_KID = "303132333435363738393a3b3c3d3e3f"
RESULT_KEY = "40404040404040404040404040404040"
SOURCE_URI = "https://keys.example.test/bbb/source"
RESULT_URI = "https://keys.example.test/bbb/result"
PROTECTIONS = ("clear", "cenc", "cbcs", "hls-aes128")


def prepare_source(destination):
    destination.mkdir(parents=True)
    streams = []
    for role in ("video", "audio"):
        (destination / role).mkdir()
        streams.append(
            f"in={FIXTURE / 'clip.mp4'},stream={role},"
            f"init_segment={destination / role}/init.mp4,"
            f"segment_template={destination / role}/seg_$Number$.m4s")
    run(["packager", *streams, "--segment_duration", "2", "--fragment_duration", "2",
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
    seconds, memory = measured_run(
        [binary, "convert", source, "--to", protection, "--format", format_name,
         "--output", destination, "--keys-file", keys, "--json"], environment)
    manifest = destination / ("master.m3u8" if format_name == "hls" else "manifest.mpd")
    assert manifest.is_file(), "conversion must publish a usable entry manifest"
    return manifest, seconds, memory


def regression(binary, destination):
    metadata = verify_fixture()
    destination.mkdir(parents=True, exist_ok=True)
    environment = {name: value for name, value in os.environ.items() if not name.startswith("DRM_")}
    clear = prepare_source(destination / "clear-source")
    sources = {"clear": clear}
    for protection in PROTECTIONS[1:]:
        config = {"schema_version": 1,
                  "output": output_options(protection, SOURCE_KID, SOURCE_KEY, SOURCE_URI)}
        keys = private_json(destination / f"prepare-{protection}.json", config)
        sources[protection], _, _ = convert(binary, clear, protection,
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
            result, elapsed, memory = convert(binary, source, target,
                                               destination / name, keys, environment)
            started = time.perf_counter()
            verification = json.loads(run([binary, "verify", result, "--source", source,
                                            "--keys-file", keys, "--json"], environment))
            verification_seconds = time.perf_counter() - started
            assert verification["passed"] is True and verification["media_verified"] is True
            assert all(verification["checks"][check] is True for check in
                       ("packet_payloads_equal", "packet_timing_equal", "decoded_content_equal"))
            assert verification["license_playback_verified"] is False
            rows.append({"input": source_protection, "output": target, "passed": True,
                         "conversion_seconds": elapsed, "verification_seconds": verification_seconds,
                         "memory": memory, "verification": verification})
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
                         "independent_conformance": "not-verified"}}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--drmr", required=True, type=Path)
    parser.add_argument("--output", required=True, type=Path)
    arguments = parser.parse_args()
    report = regression(arguments.drmr.resolve(), arguments.output.resolve())
    if os.environ.get("DRMR_CONVERSION_REPORT"):
        Path(os.environ["DRMR_CONVERSION_REPORT"]).write_text(json.dumps(report, indent=2) + "\n")
    print(json.dumps(report, sort_keys=True))


if __name__ == "__main__":
    main()
