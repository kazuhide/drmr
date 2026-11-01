"""Synthetic codec reports for fail-closed gate tests, never runtime evidence."""
import copy

from roundtrip_reports import native


def complete(contract, scope="smoke", head="a" * 40, tree="b" * 40, target="aarch64-apple-darwin", version="1.0.3",
             archive_hash="c" * 64, binary_hash="d" * 64):
    def leg(profile, mode):
        source = native("mp4")
        source["scope"] = "source-admission"
        boundary = "not-applicable" if profile[0] == "mp4" else "preserve" if mode == "segments" else "resegment"
        return {"format": profile[0], "protection": profile[1], "tracks": ["video"], "drms": contract.drms(profile),
                "analysis": source, "plan": {"schema_version": 9, "plan_id": "1" * 64, "sha256": "2" * 64,
                "processing": {"requested": mode, "resolved": "segments" if mode == "segments" else "stitch",
                               "boundaries": boundary, "segment_duration_ms": 2000 if boundary == "resegment" else None}},
                "conversion": native(profile[0]), "independent_verification": native(profile[0]),
                "output_graph": {"sha256": "3" * 64, "files": 7, "bytes": 10000}, "source_and_plan_unchanged": True}

    records = [{**case, "status": "complete", "source_unchanged": True,
                "rekeyed": case["source"][1] != "clear" and case["target"][1] != "clear",
                "forward": leg(case["target"], case["processing"]), "backward": leg(case["source"], case["processing"]),
                "final_against_original": native(case["source"][0])} for case in contract.cases(scope)]
    refusals = [{**case, "status": "refused", "exit_code": 2 if case["check"] == "missing-key" else 1,
                 "code": sorted(contract.REFUSAL_CODES[case["check"]])[0], "source_unchanged": True,
                 "destination_absent": True} for case in contract.refusals()]
    return {"schema_version": 1, "kind": "native-cli-codec-qualification", "scope": scope, "success": True,
            "source_head": head, "source_tree": tree, "target": target, "version": version,
            "archive_sha256": archive_hash, "binary_sha256": binary_hash, "fixtures": copy.deepcopy(list(contract.FIXTURES)),
            "matrix_sha256": contract.matrix_digest(scope), "records": records, "refusals": refusals,
            "decoder_guard": {"ffmpeg": "unavailable", "ffprobe": "unavailable"}, "decode_evidence": "not-checked",
            "license_playback_verified": False,
            "counts": {"expected": len(records), "complete": len(records), "refusals": len(refusals), "failed": 0, "not_checked": 0},
            "native_ordinary_user": True, "owned_workspace_cleanup": "confirmed", "limits": contract.LIMITS}
