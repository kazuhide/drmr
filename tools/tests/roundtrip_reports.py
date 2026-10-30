"""Synthetic complete reports for contract and release-assembler unit tests."""
import copy


def native(container):
    value = {"schema_version": 2, "required_depth": "native", "scope": "comparison", "passed": True,
             "checks": {"stream_metadata": "pass", "packet_payloads_equal": True, "packet_timing_equal": True},
             "decode_evidence": {"status": "not-checked", "reason": "not-requested"}, "license_playback_verified": False}
    if container != "mp4":
        value["native_manifest"] = {"schema_version": 1, "scope": "manifest-only",
                                    "profile": "dash-static-manifest" if container == "dash" else "hls-base-manifest",
                                    "errors": 0, "warnings": 0, "truncated": False, "incomplete": False, "document_count": 1}
    return value


def complete(contract, head="a" * 40, tree="b" * 40, target="aarch64-apple-darwin", version="1.0.3",
             archive_hash="c" * 64, binary_hash="d" * 64):
    graph = {"sha256": "e" * 64, "files": 7, "bytes": 10000}

    def leg(profile, layout):
        return {"format": profile[0], "protection": profile[1],
                "tracks": ["video", "audio"] if contract.joint(layout) else ["audio"] if layout.startswith("aac") else ["video"],
                "drms": contract.drms(profile), "conversion": native(profile[0]),
                "independent_verification": native(profile[0]), "output_graph": copy.deepcopy(graph)}

    fixtures = []
    for layout in contract.LAYOUTS:
        for profile in contract.INPUTS:
            rate = 44100 if "44100" in layout else 48000 if "48000" in layout else None
            fixtures.append({"id": f"{layout}:{contract.profile_id(profile)}", "graph": copy.deepcopy(graph),
                             "source_graph_sha256": "f" * 64, "verification": native(profile[0]),
                             "sample_rate": rate, "final_presented_samples": 240 if rate else None})
    records = []
    for case in contract.cases():
        record = {**case, "status": "not-qualified" if case["kind"] == "layout-condition" else "complete"}
        if case["kind"] != "layout-condition":
            record.update(forward=leg(case["target"], case["layout"]), source_unchanged=True)
        if case["kind"] == "roundtrip":
            record.update(backward=leg(case["source"], case["layout"]), final_against_original=native(case["source"][0]))
        records.append(record)
    return {"schema_version": 1, "kind": "native-cli-release-roundtrip", "success": True,
            "source_head": head, "source_tree": tree, "target": target, "version": version,
            "archive_sha256": archive_hash, "binary_sha256": binary_hash, "binary_header_target": target,
            "binary_reported_version": version, "fixture_sha256": contract.CLIP_SHA256,
            "matrix_sha256": contract.matrix_digest(), "native_ordinary_user": True, "required_depth": "native",
            "decode_evidence": "not-checked", "license_playback_verified": False,
            "fixture_metadata": copy.deepcopy(contract.FIXTURE_METADATA), "fixture_profiles": fixtures,
            "records": records, "counts": {"expected": 780, "complete": 738, "failed": 0, "not_checked": 0,
                                           "not_qualified": 42, "kinds": contract.counts(contract.cases())},
            "owned_workspace_cleanup": "confirmed", "limits": contract.LIMITS}
