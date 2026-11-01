"""Finite HEVC/VP9 archive coverage, separate from the H.264/AAC matrix."""
import hashlib
import importlib.util
import json
from pathlib import Path
import re

ROOT = Path(__file__).resolve().parents[1]
_SPEC = importlib.util.spec_from_file_location("codec_media_evidence", ROOT / "tools/media_evidence.py")
EVIDENCE = importlib.util.module_from_spec(_SPEC)
_SPEC.loader.exec_module(EVIDENCE)
_SEEDS = (
    ("hevc-main", "hevc", 1, 8, "bb6264c4a9a6aaa74b5afc00e157eea8129a3d38f4d2263b0226c091ccbfbf3a"),
    ("hevc-main10", "hevc", 2, 10, "5cd4bf734245767211030e735f70f0bbcd03c5e070b138d1a99fe76f2234d3fb"),
    ("vp9-p0-8", "vp9", 0, 8, "d9bb653ddf7b7011b4739973f5d6bc17009057d4dac46d68c84e0b006bdc0314"),
    ("vp9-p1-8", "vp9", 1, 8, "cb49f95665a8e345a27e29d04c6ce0c16e5d61f75968981768c0720e1e7b3025"),
    ("vp9-p2-10", "vp9", 2, 10, "7e58323555be2e6e04eaf630da55ef7edc7b81a06fd5d13e96514b27d1cba44d"),
    ("vp9-p2-12", "vp9", 2, 12, "f021996104adc0d35370a8ad469a873d39b77bef4237aaad1038dac516128e21"),
    ("vp9-p3-10", "vp9", 3, 10, "abfd16dbd14f30acfce852e0e8f59776e9c47287725ce4b2fdd5027ed65544e4"),
    ("vp9-p3-12", "vp9", 3, 12, "96c93b12834b2997c17d39f5da73a89154d20c932bfaff322c2276b292457d02"),
)
FIXTURES = tuple({"id": name, "codec": codec, "profile": profile, "bit_depth": depth,
                  "path": "crates/drmr/tests/fixtures/bbb/" + name + ".mp4", "sha256": digest}
                 for name, codec, profile, depth, digest in _SEEDS)
LIMITS = ("Complete native samples, metadata, clocks, presentation ranges and generated manifest/DRM declarations; "
          "ten-second, video-only, attributed HEVC Main/Main10 and six VP9 profile/depth fixtures. "
          "Decoder executables are unavailable. No decoded-pixel, HDR, DRM license, device playback, GUI, "
          "signature or build-origin certification. VP9 CBCS and HEVC/VP9 MPEG-TS output must be refused.")


class Failure(RuntimeError):
    """Fixed public diagnostic only."""


def require(value, code):
    if not value:
        raise Failure(code)


def sha(value):
    return hashlib.sha256(value).hexdigest()


def profiles(fixture):
    modes = ("clear", "cenc", "cbcs") if fixture["codec"] == "hevc" else ("clear", "cenc")
    return tuple((container, mode) for container in ("mp4", "dash", "hls-fmp4")
                 for mode in (*modes, "hls-aes128") if mode != "hls-aes128" or container == "hls-fmp4")


def cases(scope):
    require(scope in ("smoke", "full"), "codec-scope-invalid")
    result = []

    def add(fixture, source, target, processing):
        result.append({"id": fixture["id"] + ":" + "/".join(source) + "->" + "/".join(target) + ":" + processing,
                       "fixture": fixture["id"], "source": list(source), "target": list(target), "processing": processing})

    for fixture in FIXTURES:
        if scope == "smoke":
            target = ("hls-fmp4", "cbcs") if fixture["codec"] == "hevc" else ("dash", "cenc")
            add(fixture, ("mp4", "clear"), target, "auto")
            continue
        for source in profiles(fixture):
            for target in profiles(fixture):
                add(fixture, source, target, "auto")
        for container in ("dash", "hls-fmp4"):
            for protection in (("cenc", "cbcs") if fixture["codec"] == "hevc" else ("cenc",)):
                for processing in ("segments", "stitch"):
                    add(fixture, (container, "clear"), (container, protection), processing)
    return result


def matrix_digest(scope):
    return sha(json.dumps({"fixtures": FIXTURES, "cases": cases(scope), "refusals": refusals()}, sort_keys=True,
                          separators=(",", ":")).encode("ascii"))


def refusals():
    result = []
    for fixture in FIXTURES:
        for kind in ("ts-output", "missing-key", "wrong-key", "changed-sample"):
            result.append({"id": fixture["id"] + ":" + kind, "fixture": fixture["id"], "check": kind})
        if fixture["codec"] == "vp9":
            result.append({"id": fixture["id"] + ":cbcs-output", "fixture": fixture["id"], "check": "cbcs-output"})
    return result


def exact(value, fields):
    require(isinstance(value, dict) and set(value) == set(fields), "codec-report-schema-invalid")


def hexadecimal(value, length=64):
    return isinstance(value, str) and re.fullmatch("[0-9a-f]{" + str(length) + "}", value) is not None


def graph(value):
    exact(value, ("sha256", "files", "bytes"))
    require(hexadecimal(value["sha256"]) and type(value["files"]) is int and 0 < value["files"] <= 64
            and type(value["bytes"]) is int and 0 < value["bytes"] <= 64 * 1024 * 1024, "codec-graph-invalid")


def proof(value, container, scope="comparison"):
    parsed = EVIDENCE.v2_proof(value, scope=scope)
    require(parsed is not None and parsed == value and parsed["required_depth"] == "native"
            and parsed["scope"] == scope
            and parsed["decode_evidence"] == {"status": "not-checked", "reason": "not-requested"},
            "codec-native-proof-invalid")
    if scope == "comparison":
        if container == "mp4":
            require("native_manifest" not in value, "codec-manifest-proof-invalid")
        else:
            expected = ("dash-static-manifest",) if container == "dash" else ("hls-base-manifest", "apple-vod-manifest")
            require(value.get("native_manifest", {}).get("profile") in expected, "codec-manifest-proof-invalid")


def drms(profile):
    container, protection = profile
    if container == "mp4" or protection in ("clear", "hls-aes128"):
        return []
    return ["fairplay", "widevine", "playready"] if container == "hls-fmp4" and protection == "cbcs" else ["widevine", "playready"]


def leg(value, profile, processing):
    exact(value, ("format", "protection", "tracks", "drms", "analysis", "plan", "conversion",
                  "independent_verification", "output_graph", "source_and_plan_unchanged"))
    require(value["format"] == profile[0] and value["protection"] == profile[1] and value["tracks"] == ["video"]
            and value["drms"] == drms(profile) and value["source_and_plan_unchanged"] is True, "codec-leg-invalid")
    proof(value["analysis"], "mp4", "source-admission")
    plan = value["plan"]
    exact(plan, ("schema_version", "plan_id", "sha256", "processing"))
    require(type(plan["schema_version"]) is int and plan["schema_version"] == 9
            and hexadecimal(plan["plan_id"]) and hexadecimal(plan["sha256"]), "codec-plan-invalid")
    mode = plan["processing"]
    exact(mode, ("requested", "resolved", "boundaries", "segment_duration_ms"))
    require(mode["requested"] == processing and mode["resolved"] in ("segments", "stitch")
            and (processing == "auto" or mode["resolved"] == processing), "codec-processing-invalid")
    boundary = "not-applicable" if profile[0] == "mp4" else "preserve" if mode["resolved"] == "segments" else "resegment"
    duration = 2000 if boundary == "resegment" else None
    require(mode["boundaries"] == boundary and mode["segment_duration_ms"] == duration
            and (duration is None or type(mode["segment_duration_ms"]) is int), "codec-processing-invalid")
    proof(value["conversion"], profile[0])
    proof(value["independent_verification"], profile[0])
    if profile == ["hls-fmp4", "cenc"] or profile == ("hls-fmp4", "cenc"):
        require(value["conversion"]["native_manifest"]["profile"] == "hls-base-manifest"
                and value["independent_verification"]["native_manifest"]["profile"] == "hls-base-manifest",
                "codec-manifest-proof-invalid")
    graph(value["output_graph"])


REFUSAL_CODES = {
    "ts-output": frozenset(("execution-plan-not-supported",)),
    "cbcs-output": frozenset(("vp9-protection-scheme-not-supported",)),
    "missing-key": frozenset(("input-key-required",)),
    "wrong-key": frozenset(("verification-mismatch", "invalid-hevc-syntax", "invalid-vp9-syntax",
                            "native-hevc-configuration-not-supported", "native-vp9-configuration-not-supported")),
    "changed-sample": frozenset(("verification-mismatch", "invalid-hevc-syntax", "invalid-vp9-syntax")),
}


def validate(report, scope, head, tree, target, version, archive_hash, binary_hash):
    exact(report, ("schema_version", "kind", "scope", "success", "source_head", "source_tree", "target", "version",
                   "archive_sha256", "binary_sha256", "fixtures", "matrix_sha256", "records", "refusals",
                   "decoder_guard", "decode_evidence", "license_playback_verified", "counts", "native_ordinary_user", "owned_workspace_cleanup", "limits"))
    require(type(report["schema_version"]) is int and report["schema_version"] == 1
            and report["kind"] == "native-cli-codec-qualification" and report["scope"] == scope
            and report["success"] is True and report["source_head"] == head and report["source_tree"] == tree
            and report["target"] == target and report["version"] == version
            and report["archive_sha256"] == archive_hash and report["binary_sha256"] == binary_hash
            and report["fixtures"] == list(FIXTURES) and report["matrix_sha256"] == matrix_digest(scope)
            and all(type(item["profile"]) is int and type(item["bit_depth"]) is int for item in report["fixtures"])
            and report["decoder_guard"] == {"ffmpeg": "unavailable", "ffprobe": "unavailable"}
            and report["decode_evidence"] == "not-checked" and report["license_playback_verified"] is False
            and report["native_ordinary_user"] is True
            and report["owned_workspace_cleanup"] == "confirmed"
            and report["limits"] == LIMITS, "codec-report-binding-invalid")
    expected = cases(scope)
    require(isinstance(report["records"], list) and len(report["records"]) == len(expected), "codec-records-incomplete")
    for record, case in zip(report["records"], expected):
        exact(record, (*case, "status", "source_unchanged", "rekeyed", "forward", "backward", "final_against_original"))
        require(all(record[name] == value for name, value in case.items()) and record["status"] == "complete"
                and record["source_unchanged"] is True
                and type(record["rekeyed"]) is bool
                and record["rekeyed"] == (case["source"][1] != "clear" and case["target"][1] != "clear"),
                "codec-case-binding-invalid")
        leg(record["forward"], case["target"], case["processing"])
        leg(record["backward"], case["source"], case["processing"])
        proof(record["final_against_original"], case["source"][0])
    expected_refusals = refusals()
    require(isinstance(report["refusals"], list) and len(report["refusals"]) == len(expected_refusals), "codec-refusals-incomplete")
    for record, case in zip(report["refusals"], expected_refusals):
        exact(record, (*case, "status", "exit_code", "code", "source_unchanged", "destination_absent"))
        require(all(record[name] == value for name, value in case.items()) and record["status"] == "refused"
                and type(record["exit_code"]) is int and record["exit_code"] == (2 if case["check"] == "missing-key" else 1)
                and record["code"] in REFUSAL_CODES[case["check"]] and record["source_unchanged"] is True
                and record["destination_absent"] is True, "codec-refusal-proof-invalid")
    expected_counts = {"expected": len(expected), "complete": len(expected), "refusals": len(expected_refusals),
                       "failed": 0, "not_checked": 0}
    require(report["counts"] == expected_counts and all(type(value) is int for value in report["counts"].values()),
            "codec-counts-invalid")
    return report
