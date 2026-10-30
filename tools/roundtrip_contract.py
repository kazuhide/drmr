"""The finite release-only conversion matrix and its fail-closed report contract."""
import hashlib
import importlib.util
import json
from pathlib import Path
import re

ROOT = Path(__file__).resolve().parents[1]


def module(name):
    spec = importlib.util.spec_from_file_location(name, ROOT / ("tools/" + name + ".py"))
    result = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(result)
    return result


EXAMPLES = module("conversion_examples")
EVIDENCE = module("media_evidence")
LAYOUTS = ("video", "aac44100", "aac48000", "video-aac44100", "video-aac48000")
OUTPUTS = tuple((container, protection) for container in ("mp4", "dash", "hls-fmp4", "hls-ts")
                for protection in (("clear", "hls-aes128", "hls-sample-aes") if container == "hls-ts"
                                   else ("clear", "cenc", "cbcs", "hls-aes128") if container == "hls-fmp4"
                                   else ("clear", "cenc", "cbcs")))
CLIP_SHA256 = "a0ef6d20f2319c57af35d9c3165e46b669c67b951268cf882dbfff14b5b26e71"
LIMITS = ("Native complete-sample, metadata, clock and playback-range comparisons on the attributed ten-second clip; "
          "two-second segments, H.264, AAC 44.1/48 kHz including a partial final presented AAC frame. "
          "Automatic processing and default SegmentTemplate DASH addressing; canonical shared container metadata. "
          "No default decoding, DRM license acquisition, device playback, GUI, signature or build-origin certification.")
FIXTURE_METADATA = {
    "mp4_metadata_scope": "common-representable-values-and-semantic-language",
    "layouts": {layout: {"audio_rendition": {
        "group": "default-audio-group", "name": "stream_1", "language": "en", "default": False, "autoselect": True,
    } if layout.startswith("video-") else None} for layout in LAYOUTS},
}
LAYOUT_CONDITION = "joint-dash-hls-to-mp4-not-qualified"
REVERSE_CONDITION = "reverse-joint-dash-hls-to-mp4-not-qualified"


class Failure(RuntimeError):
    """Fixed public diagnostics only."""


def require(value, code):
    if not value:
        raise Failure(code)


def sha(value):
    return hashlib.sha256(value).hexdigest()


def canonical(value):
    return json.dumps(value, sort_keys=True, separators=(",", ":")).encode("ascii")


def hex_value(value, length=64):
    return isinstance(value, str) and re.fullmatch("[0-9a-f]{" + str(length) + "}", value) is not None


def profile_id(profile):
    return "/".join(profile)


def joint(layout):
    return layout.startswith("video-")


def supported(layout, source, target):
    return not (joint(layout) and target[0] == "mp4" and source[0] in ("dash", "hls-fmp4"))


def cases():
    result = []
    for layout in LAYOUTS:
        row = ("hls-ts-audio" if layout.startswith("aac") else "hls-ts-video")
        for source in OUTPUTS:
            recipe_source = row if source[0] == "hls-ts" else source[0]
            for target in OUTPUTS:
                output = "hls" if target[0] == "hls-fmp4" else target[0]
                recipe = EXAMPLES.identifier(recipe_source, output, target[1])
                if not supported(layout, source, target):
                    kind, condition = "layout-condition", LAYOUT_CONDITION
                elif not supported(layout, target, source):
                    kind, condition = "forward-only", REVERSE_CONDITION
                else:
                    kind, condition = "roundtrip", None
                result.append({"id": f"{layout}:{profile_id(source)}->{profile_id(target)}", "layout": layout,
                               "source": list(source), "target": list(target), "recipe": recipe,
                               "kind": kind, "condition": condition})
    return result


def counts(records):
    return {name: sum(record["kind"] == name for record in records)
            for name in ("roundtrip", "forward-only", "layout-condition")}


def matrix_digest():
    return sha(canonical({"examples": EXAMPLES.routes(), "inputs": EXAMPLES.INPUTS,
                          "fixture_metadata": FIXTURE_METADATA, "cases": cases()}))


def exact(value, keys, code="roundtrip-schema-invalid"):
    require(isinstance(value, dict) and set(value) == set(keys), code)


def graph(value):
    exact(value, ("sha256", "files", "bytes"))
    require(hex_value(value["sha256"]) and type(value["files"]) is int and 0 < value["files"] <= 64
            and type(value["bytes"]) is int and 0 < value["bytes"] <= 64 * 1024 * 1024,
            "roundtrip-graph-invalid")


def native_proof(value, container, protection=None):
    proof = EVIDENCE.v2_proof(value)
    require(proof is not None and proof == value and proof["required_depth"] == "native"
            and proof["decode_evidence"] == {"status": "not-checked", "reason": "not-requested"},
            "roundtrip-native-proof-invalid")
    if container == "mp4":
        require("native_manifest" not in proof, "roundtrip-manifest-proof-invalid")
    else:
        manifest = proof.get("native_manifest", {})
        profile = "dash-static-manifest" if container == "dash" else "hls-base-manifest"
        require(manifest.get("profile") in ((profile, "apple-vod-manifest") if container != "dash" else (profile,)),
                "roundtrip-manifest-proof-invalid")
        if container == "hls-fmp4" and protection == "cenc":
            require(manifest.get("profile") == "hls-base-manifest", "roundtrip-manifest-proof-invalid")


def drms(profile):
    if profile[0] in ("dash", "hls-fmp4") and profile[1] == "cenc":
        return ["widevine", "playready"]
    if profile[0] == "dash" and profile[1] == "cbcs":
        return ["widevine", "playready"]
    if profile == ("hls-fmp4", "cbcs"):
        return ["fairplay", "widevine", "playready"]
    if profile == ("hls-ts", "hls-sample-aes"):
        return ["fairplay"]
    return []


def leg(value, expected, layout):
    exact(value, ("format", "protection", "tracks", "drms", "conversion", "independent_verification", "output_graph"))
    require(value["format"] == expected[0] and value["protection"] == expected[1]
            and value["tracks"] == (["video", "audio"] if joint(layout) else ["audio"] if layout.startswith("aac") else ["video"])
            and value["drms"] == drms(expected), "roundtrip-profile-invalid")
    native_proof(value["conversion"], expected[0], expected[1])
    native_proof(value["independent_verification"], expected[0], expected[1])
    graph(value["output_graph"])


def validate(report, head, tree, target, version, archive_hash, binary_hash):
    require(len(EXAMPLES.routes()) == 65 and {case["recipe"] for case in cases()}
            == {EXAMPLES.identifier(*route) for route in EXAMPLES.routes()}, "roundtrip-matrix-coverage-invalid")
    exact(report, ("schema_version", "kind", "success", "source_head", "source_tree", "target", "version",
                   "archive_sha256", "binary_sha256", "binary_header_target", "binary_reported_version",
                   "fixture_sha256", "matrix_sha256", "native_ordinary_user", "required_depth", "decode_evidence",
                   "license_playback_verified", "fixture_metadata", "fixture_profiles", "records", "counts", "owned_workspace_cleanup", "limits"))
    require(type(report["schema_version"]) is int and report["schema_version"] == 1
            and report["kind"] == "native-cli-release-roundtrip" and report["success"] is True
            and report["source_head"] == head and report["source_tree"] == tree
            and report["target"] == target and report["version"] == version
            and report["archive_sha256"] == archive_hash and report["binary_sha256"] == binary_hash
            and report["binary_header_target"] == target and report["binary_reported_version"] == version
            and report["fixture_sha256"] == CLIP_SHA256 and report["matrix_sha256"] == matrix_digest()
            and report["native_ordinary_user"] is True and report["required_depth"] == "native"
            and report["decode_evidence"] == "not-checked" and report["license_playback_verified"] is False
            and report["fixture_metadata"] == FIXTURE_METADATA
            and all(type(report["fixture_metadata"]["layouts"][layout]["audio_rendition"][name]) is bool
                    for layout in LAYOUTS if joint(layout) for name in ("default", "autoselect"))
            and report["owned_workspace_cleanup"] == "confirmed" and report["limits"] == LIMITS,
            "roundtrip-binding-invalid")
    fixtures = report["fixture_profiles"]
    expected_fixtures = {f"{layout}:{profile_id(profile)}" for layout in LAYOUTS for profile in OUTPUTS}
    require(isinstance(fixtures, list) and len(fixtures) == len(expected_fixtures), "roundtrip-fixtures-incomplete")
    seen = set()
    for fixture in fixtures:
        exact(fixture, ("id", "graph", "source_graph_sha256", "verification", "sample_rate", "final_presented_samples"))
        require(fixture["id"] in expected_fixtures and fixture["id"] not in seen, "roundtrip-fixtures-incomplete")
        seen.add(fixture["id"])
        layout, profile = fixture["id"].split(":")
        container, protection = profile.split("/")
        rate = 44100 if "44100" in layout else 48000 if "48000" in layout else None
        require(fixture["sample_rate"] == rate and fixture["final_presented_samples"] == (240 if rate else None)
                and (rate is None or type(fixture["sample_rate"]) is int and type(fixture["final_presented_samples"]) is int)
                and hex_value(fixture["source_graph_sha256"]), "roundtrip-fixture-profile-invalid")
        graph(fixture["graph"])
        native_proof(fixture["verification"], container, protection)
    expected = cases()
    records = report["records"]
    require(isinstance(records, list) and len(records) == len(expected), "roundtrip-records-incomplete")
    for record, case in zip(records, expected):
        fields = ("status",) if case["kind"] == "layout-condition" else ("status", "forward", "source_unchanged")
        if case["kind"] == "roundtrip":
            fields += ("backward", "final_against_original")
        exact(record, (*case, *fields))
        require(all(record[key] == value for key, value in case.items()), "roundtrip-case-binding-invalid")
        if case["kind"] == "layout-condition":
            require(record["status"] == "not-qualified", "roundtrip-condition-invalid")
            continue
        require(record["status"] == "complete" and record["source_unchanged"] is True, "roundtrip-case-incomplete")
        leg(record["forward"], case["target"], case["layout"])
        if case["kind"] == "roundtrip":
            leg(record["backward"], case["source"], case["layout"])
            native_proof(record["final_against_original"], case["source"][0], case["source"][1])
    exact(report["counts"], ("expected", "complete", "failed", "not_checked", "not_qualified", "kinds"))
    require(all(type(report["counts"][name]) is int for name in ("expected", "complete", "failed", "not_checked", "not_qualified"))
            and isinstance(report["counts"]["kinds"], dict)
            and all(type(value) is int for value in report["counts"]["kinds"].values()), "roundtrip-counts-invalid")
    expected_counts = counts(expected)
    require(report["counts"] == {"expected": len(expected), "complete": expected_counts["roundtrip"] + expected_counts["forward-only"],
                                 "failed": 0, "not_checked": 0, "not_qualified": expected_counts["layout-condition"],
                                 "kinds": expected_counts},
            "roundtrip-counts-invalid")
    return report
