"""Strict source-bound evidence for the two finite, single-process fuzz targets."""
import hashlib
import importlib.util
import json
from pathlib import Path
import re
import sys

sys.dont_write_bytecode = True
ROOT = Path(__file__).resolve().parents[1]


def module(name):
    spec = importlib.util.spec_from_file_location(name, ROOT / ("tools/" + name + ".py"))
    value = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(value)
    return value


VERIFY = module("verify_archive")
CODECS = module("codec_contract")
TARGETS = (("hevc", 4, 2), ("vp9", 18, 2))
TOOLCHAIN = {"nightly": "nightly-2026-11-01", "cargo_fuzz": "0.13.2", "libfuzzer_sys": "0.4.13",
             "target": "x86_64-unknown-linux-gnu", "sanitizer": "address"}
BOUNDS = {"seconds": 60, "max_len": 65536, "timeout_seconds": 2, "rss_mib": 512,
          "malloc_mib": 128, "workers": 1, "seed": 1}
IDENTITY_FIELDS = ("manifest_sha256", "lock_sha256", "seed_manifest_sha256", "harness_sha256",
                   "corpus_sha256", "vendor_patch_sha256")
LIMITS = ("Two finite Linux address-sanitized codec-parser fuzz campaigns with attributed initial seeds; "
          "no proof of absence of defects, complete bitstream support, decoded equality or DRM-device playback.")
FAILURE_CODES = frozenset((
    "arguments-invalid", "fuzz-report-invalid", "fuzz-report-schema-invalid", "fuzz-seed-manifest-invalid",
    "fuzz-vendor-identity-invalid", "fuzz-report-missing-or-invalid", "fuzz-report-binding-invalid",
    "fuzz-source-identity-invalid", "fuzz-execution-settings-invalid", "fuzz-audit-invalid", "fuzz-targets-incomplete",
    "fuzz-target-evidence-invalid", "fuzz-cancelled", "fuzz-source-changed", "fuzz-statistics-invalid", "fuzz-binary-changed",
    "fuzz-toolchain-mismatch", "fuzz-auditor-mismatch", "fuzz-metadata-invalid", "fuzz-audit-database-invalid",
    "fuzz-dependency-advisory", "fuzz-audit-database-changed", "fuzz-seed-identity-invalid", "fuzz-report-destination-invalid",
    "fuzz-qualification-failed", "fuzz-timeout", "fuzz-cleanup-unconfirmed", "fuzz-report-write-failed"))


class Failure(Exception):
    """A fixed diagnostic that never includes source or child-process values."""

    def __init__(self, code):
        super().__init__(code if type(code) is str and code in FAILURE_CODES else "fuzz-qualification-failed")


def require(value, code="fuzz-report-invalid"):
    if not value:
        raise Failure(code)


def exact(value, fields):
    require(type(value) is dict and set(value) == set(fields), "fuzz-report-schema-invalid")


def sha(data):
    return hashlib.sha256(data).hexdigest()


def canonical(value):
    return json.dumps(value, sort_keys=True, separators=(",", ":")).encode("ascii")


def hexadecimal(value, length=64):
    return type(value) is str and re.fullmatch("[0-9a-f]{" + str(length) + "}", value) is not None


def seeds(source):
    raw = source.read("fuzz/seed-manifest.json")
    require(len(raw) <= 65536, "fuzz-seed-manifest-invalid")
    value = VERIFY.loads(raw)
    exact(value, ("schema_version", "seeds", "source_fixtures"))
    require(type(value["schema_version"]) is int and value["schema_version"] == 1
            and value["source_fixtures"] == [{"path": item["path"], "sha256": item["sha256"]} for item in CODECS.FIXTURES]
            and type(value["seeds"]) is list and len(value["seeds"]) == 26, "fuzz-seed-manifest-invalid")
    names = []
    for seed in value["seeds"]:
        exact(seed, ("path", "sha256", "bytes", "accepted", "samples"))
        require(type(seed["path"]) is str and re.fullmatch(r"(?:hevc|vp9)/[a-z0-9-]+", seed["path"]) is not None
                and hexadecimal(seed["sha256"]) and type(seed["bytes"]) is int and 0 < seed["bytes"] <= BOUNDS["max_len"]
                and type(seed["accepted"]) is bool and type(seed["samples"]) is int and 0 <= seed["samples"] <= 16,
                "fuzz-seed-manifest-invalid")
        names.append(seed["path"])
    require(len(set(names)) == len(names), "fuzz-seed-manifest-invalid")
    for target, accepted, refused in TARGETS:
        selected = [item for item in value["seeds"] if item["path"].startswith(target + "/")]
        require(sum(item["accepted"] for item in selected) == accepted and len(selected) == accepted + refused,
                "fuzz-seed-manifest-invalid")
    return value["seeds"]


def identity(source):
    corpus = seeds(source)
    vendor = VERIFY.command(["git", "rev-parse", source.head + ":vendor/private-fs",
                             source.head + ":vendor/process-wrap"], cwd=source.path).decode("ascii").splitlines()
    require(len(vendor) == 2 and all(hexadecimal(item, 40) for item in vendor), "fuzz-vendor-identity-invalid")
    return {"manifest_sha256": sha(source.read("fuzz/Cargo.toml")), "lock_sha256": sha(source.read("fuzz/Cargo.lock")),
            "seed_manifest_sha256": sha(source.read("fuzz/seed-manifest.json")),
            "harness_sha256": sha(canonical([[target, sha(source.read("fuzz/fuzz_targets/" + target + ".rs"))]
                                             for target, _, _ in TARGETS])),
            "corpus_sha256": sha(canonical(corpus)), "vendor_patch_sha256": sha(canonical(vendor))}


def read_report(path):
    support = module("qualify_cli_archive")
    try:
        raw = support.read_regular(Path(path), 65536)
        def invalid(_):
            raise ValueError()
        return json.loads(raw, object_pairs_hook=VERIFY.unique, parse_constant=invalid)
    except (support.Refusal, VERIFY.Failure, OSError, ValueError, TypeError, UnicodeError, RecursionError):
        raise Failure("fuzz-report-missing-or-invalid") from None


def validate(report, head, tree, version, expected):
    exact(report, ("schema_version", "kind", "success", "source_head", "source_tree", "version", "fuzz", "toolchain",
                   "bounds", "audit", "targets", "seed_check", "source_unchanged", "owned_workspace_cleanup", "limits"))
    require(type(report["schema_version"]) is int and report["schema_version"] == 1
            and report["kind"] == "source-codec-fuzz" and report["success"] is True
            and hexadecimal(head, 40) and hexadecimal(tree, 40)
            and report["source_head"] == head and report["source_tree"] == tree and report["version"] == version
            and report["seed_check"] is True and report["source_unchanged"] is True
            and report["owned_workspace_cleanup"] == "confirmed" and report["limits"] == LIMITS,
            "fuzz-report-binding-invalid")
    exact(report["fuzz"], IDENTITY_FIELDS)
    require(report["fuzz"] == expected and all(hexadecimal(item) for item in report["fuzz"].values()),
            "fuzz-source-identity-invalid")
    exact(report["toolchain"], TOOLCHAIN)
    exact(report["bounds"], BOUNDS)
    require(report["toolchain"] == TOOLCHAIN and all(type(item) is str for item in report["toolchain"].values())
            and report["bounds"] == BOUNDS and all(type(item) is int for item in report["bounds"].values()),
            "fuzz-execution-settings-invalid")
    audit = report["audit"]
    exact(audit, ("database_revision", "actual_lock_sha256", "registry_identity_lock_sha256", "findings", "backports_verified"))
    require(hexadecimal(audit["database_revision"], 40) and audit["actual_lock_sha256"] == expected["lock_sha256"]
            and hexadecimal(audit["registry_identity_lock_sha256"]) and type(audit["findings"]) is int
            and audit["findings"] == 0 and audit["backports_verified"] is True, "fuzz-audit-invalid")
    require(type(report["targets"]) is list and len(report["targets"]) == 2, "fuzz-targets-incomplete")
    for record, (target, accepted, refused) in zip(report["targets"], TARGETS):
        exact(record, ("target", "binary_sha256", "seed_admission", "exit_code", "executions", "coverage",
                       "elapsed_milliseconds", "fuzzer_seconds", "peak_rss_mib", "cancelled", "timed_out"))
        exact(record["seed_admission"], ("accepted", "refused"))
        require(record["target"] == target and hexadecimal(record["binary_sha256"])
                and record["seed_admission"] == {"accepted": accepted, "refused": refused}
                and all(type(item) is int for item in record["seed_admission"].values())
                and type(record["exit_code"]) is int and record["exit_code"] == 0
                and type(record["executions"]) is int and record["executions"] > accepted + refused
                and type(record["coverage"]) is int and record["coverage"] > 0
                and type(record["elapsed_milliseconds"]) is int and 60000 <= record["elapsed_milliseconds"] <= 90000
                and type(record["fuzzer_seconds"]) is int and 60 <= record["fuzzer_seconds"] <= 90
                and abs(record["elapsed_milliseconds"] - record["fuzzer_seconds"] * 1000) <= 3000
                and type(record["peak_rss_mib"]) is int and 0 < record["peak_rss_mib"] <= BOUNDS["rss_mib"]
                and record["cancelled"] is False and record["timed_out"] is False, "fuzz-target-evidence-invalid")
    return report
