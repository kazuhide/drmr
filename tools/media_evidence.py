"""Allowlisted version-two media evidence for maintainer reports.

Legacy readers retain their own version-one contracts. This module never maps
unrequested decoding to decoded equality or to successful decoder execution.
"""


NATIVE_CHECKS = ("stream_metadata", "packet_payloads_equal", "packet_timing_equal")


def v2_proof(value, scope="comparison", require_decoded=False):
    if (scope not in ("source-admission", "comparison")
            or not isinstance(value, dict) or type(value.get("schema_version")) is not int
            or value["schema_version"] != 2 or value.get("scope") != scope
            or value.get("required_depth") not in ("native", "decoded")
            or value.get("passed") is not True or value.get("license_playback_verified") is not False
            or "failure" in value or "media_verified" in value):
        return None
    checks = value.get("checks")
    if (not isinstance(checks, dict) or set(checks) != set(NATIVE_CHECKS)
            or checks["stream_metadata"] != "pass"
            or checks["packet_payloads_equal"] is not True or checks["packet_timing_equal"] is not True):
        return None
    decoded = value.get("decode_evidence")
    if not isinstance(decoded, dict):
        return None
    status = decoded.get("status")
    if status == "not-checked":
        if (set(decoded) != {"status", "reason"} or decoded["reason"] != "not-requested"
                or value["required_depth"] != "native" or require_decoded):
            return None
    elif status == "source-decoded":
        if set(decoded) != {"status"} or scope != "source-admission":
            return None
    elif status == "content-compared":
        if set(decoded) != {"status", "equal"} or scope != "comparison" or decoded["equal"] is not True:
            return None
    else:
        return None
    if require_decoded and value["required_depth"] != "decoded":
        return None
    result = {"schema_version": 2, "required_depth": value["required_depth"], "scope": scope,
              "passed": True, "checks": dict(checks), "decode_evidence": dict(decoded),
              "license_playback_verified": False}
    if "native_manifest" in value:
        native = value["native_manifest"]
        fields = ("schema_version", "scope", "profile", "errors", "warnings", "truncated", "incomplete", "document_count")
        if (not isinstance(native, dict) or type(native.get("schema_version")) is not int
                or native["schema_version"] != 1 or native.get("scope") != "manifest-only"
                or native.get("profile") not in ("dash-static-manifest", "hls-base-manifest", "apple-vod-manifest")
                or type(native.get("errors")) is not int or native["errors"] != 0
                or type(native.get("warnings")) is not int or native["warnings"] < 0
                or native.get("truncated") is not False or native.get("incomplete") is not False
                or type(native.get("document_count")) is not int or native["document_count"] < 1):
            return None
        result["native_manifest"] = {name: native[name] for name in fields}
    return result
