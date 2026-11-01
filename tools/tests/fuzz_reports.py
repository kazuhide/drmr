"""Synthetic source-bound fuzz evidence for contract tests, never runtime proof."""
import hashlib
import json


def files(codec_contract):
    result = {
        "fuzz/Cargo.toml": '[package]\nname = "drmr-codec-fuzz"\nversion = "0.0.0"\n',
        "fuzz/Cargo.lock": 'version = 4\n',
        "fuzz/fuzz_targets/hevc.rs": "// Synthetic test harness.\n",
        "fuzz/fuzz_targets/vp9.rs": "// Synthetic test harness.\n",
        "vendor/private-fs/Cargo.toml": '[package]\nname = "private-fs"\n',
        "vendor/process-wrap/Cargo.toml": '[package]\nname = "process-wrap"\n',
    }
    seeds = []
    for target, accepted in (("hevc", 4), ("vp9", 18)):
        for index in range(accepted + 2):
            name = target + "/seed-" + str(index)
            data = "Synthetic public seed " + name
            result["fuzz/corpus/" + name] = data
            seeds.append({"path": name, "sha256": hashlib.sha256(data.encode()).hexdigest(),
                          "bytes": len(data), "accepted": index < accepted, "samples": 0})
    result["fuzz/seed-manifest.json"] = json.dumps({"schema_version": 1, "seeds": seeds,
        "source_fixtures": [{"path": item["path"], "sha256": item["sha256"]} for item in codec_contract.FIXTURES]})
    return result


def complete(contract, identity, head, tree, version):
    return {"schema_version": 1, "kind": "source-codec-fuzz", "success": True,
            "source_head": head, "source_tree": tree, "version": version,
            "fuzz": identity, "toolchain": dict(contract.TOOLCHAIN), "bounds": dict(contract.BOUNDS),
            "audit": {"database_revision": "d" * 40, "actual_lock_sha256": identity["lock_sha256"],
                      "registry_identity_lock_sha256": "e" * 64, "findings": 0, "backports_verified": True},
            "targets": [{"target": target, "binary_sha256": str(index + 1) * 64,
                         "seed_admission": {"accepted": accepted, "refused": 2},
                         "exit_code": 0, "executions": 1000, "coverage": 100,
                         "elapsed_milliseconds": 61000, "fuzzer_seconds": 61, "peak_rss_mib": 300,
                         "cancelled": False, "timed_out": False} for index, (target, accepted)
                        in enumerate((("hevc", 4), ("vp9", 18)))],
            "seed_check": True, "source_unchanged": True, "owned_workspace_cleanup": "confirmed",
            "limits": contract.LIMITS}
