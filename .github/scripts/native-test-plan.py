#!/usr/bin/env python3
"""Plan complete native test coverage from the compiled libtest inventory."""
import argparse
import collections
import hashlib
import json
from pathlib import Path
import re
import subprocess


COMMON_CHECKS = (
    "conversion::layout::stage::tests::re_layout_moves_media_without_copying_and_rewrites_actual_hls_paths",
    "conversion::download::layout_tests::source_download_keeps_nested_paths_and_child_relative_uris",
    "conversion::download::owned::tests::entrypoint_publication_is_prepaid_before_moving_the_pending_original",
)
WINDOWS_CHECKS = (
    "native::workspace::windows::tests::exclusive_media_child_creation_preserves_guards_and_existing_entries",
    "conversion::files::relocation::tests::late_replacements_are_not_moved_or_deleted_by_source_mutations",
)
POLICY_CHECKS = (
    "new_single_plan_saves_the_actual_request_and_replays_the_source_bound_and_layout",
    "single_schema_one_keeps_two_seconds",
    "source_policy::legacy_segment_plans_keep_generated_seven_while_new_plans_inherit_six",
)
# Longest complete binaries in native runner logs. Keep costly work at the front;
# this affects order only, never the set of tests or the concurrency budget.
ORDER = ("conversion_ts_output", "conversion_abr_hls", "drmr_core",
         "conversion_hls_multi_drm", "conversion_finite_outputs", "conversion_bundle_encryption",
         "conversion_multi_drm", "conversion_ts", "conversion_saved_output_options", "toolrun")
# Relative placement weights from complete Windows native groups. Process and
# memory limits are independent; unknown targets still enter the complete plan.
SHARD_WEIGHTS = {"conversion_abr_hls": 70, "conversion_ts_output": 52, "drmr_core": 26,
                 "conversion_finite_outputs": 15, "conversion_bundle_encryption": 15,
                 "conversion_hls_multi_drm": 12}
REPORT_TESTS = {
    "bbb-media-report.json": "the_real_cli_saves_converts_and_independently_verifies_the_bbb_multi_drm_plan",
    "conversion-report.json": "the_real_cli_verifies_all_sixteen_bbb_protection_routes",
}


def parse_listing(text):
    result = []
    for line in text.splitlines():
        if not line:
            continue
        if not re.fullmatch(r"[A-Za-z_][A-Za-z_0-9:]*: test", line):
            raise ValueError("Invalid native test inventory")
        result.append(line[:-6])
    if len(result) != len(set(result)):
        raise ValueError("Duplicate native test inventory")
    return result


def name(executable):
    match = re.fullmatch(r"(.+)-[0-9a-f]+(?:\.exe)?", re.split(r"[/\\]", executable)[-1])
    if not match:
        raise ValueError("Invalid native test binary name")
    return match[1]


def group(item, tests, *, priority=False):
    tests = sorted(tests)
    all_tests = set(item["tests"])
    excluded = sorted(all_tests - set(tests))
    if set(tests) == all_tests:
        arguments = []
    elif len(tests) <= len(excluded):
        arguments = ["--exact", *tests]
    else:
        # libtest skip filters are substrings. The actual filtered listing is
        # checked before execution, including if a future test extends a name.
        arguments = [value for test in excluded for value in ("--skip", test)]
    return {"executable": item["executable"], "arguments": arguments,
            "tests": tests, "priority": priority}


def split_transport_tests(tests):
    # The 57-case TS binary dominates native CI. Spread its route matrices over
    # three groups; unknown/new tests still enter the least-loaded group.
    def cost(test):
        if "twenty_seven" in test:
            return 27
        if "eighteen" in test:
            return 18
        if "nine" in test:
            return 9
        return 1
    buckets, loads = [[], [], []], [0, 0, 0]
    for test in sorted(tests, key=lambda test: (-cost(test), test)):
        index = min(range(3), key=lambda index: (loads[index], index))
        buckets[index].append(test)
        loads[index] += cost(test)
    return [bucket for bucket in buckets if bucket]


def build_plan(inventory, *, windows=False, split_transport=True):
    if not inventory:
        raise ValueError("Native test binaries are missing")
    by_name, paths = {}, set()
    for item in inventory:
        target = name(item["executable"])
        if (item["executable"] in paths
                or len(item["tests"]) != len(set(item["tests"]))):
            raise ValueError("Duplicate native test inventory")
        if target in by_name and target in ("drmr_core", "conversion_saved_hls_policy", "source_admission"):
            raise ValueError("Ambiguous critical native test binary")
        # Noncritical target names can repeat across packages; retain every path.
        by_name.setdefault(target, item)
        paths.add(item["executable"])
    core = by_name.get("drmr_core")
    policy = by_name.get("conversion_saved_hls_policy")
    admission = by_name.get("source_admission")
    checks = [*COMMON_CHECKS, *(WINDOWS_CHECKS if windows else ())]
    if (core is None or policy is None or admission is None
            or not set(checks) <= set(core["tests"])
            or not set(POLICY_CHECKS) <= set(policy["tests"]) or not admission["tests"]):
        raise ValueError("A required early regression is missing")
    early = [group(core, checks, priority=True), group(policy, policy["tests"], priority=True),
             group(admission, admission["tests"], priority=True)]
    remaining = []
    for item in inventory:
        if item is policy or item is admission:
            continue
        selected = set(item["tests"]) - (set(checks) if item is core else set())
        if split_transport and name(item["executable"]) == "conversion_ts_output" and selected:
            remaining.extend(group(item, bucket) for bucket in split_transport_tests(selected))
        else:
            remaining.append(group(item, selected))
    def order(item):
        target = name(item["executable"])
        return (ORDER.index(target) if target in ORDER else len(ORDER), item["executable"])
    result = early + sorted(remaining, key=order)
    expected = collections.Counter((item["executable"], test) for item in inventory for test in item["tests"])
    actual = collections.Counter((item["executable"], test) for item in result for test in item["tests"])
    if actual != expected or any(count != 1 for count in actual.values()):
        raise ValueError("Native test plan coverage is incomplete or repeated")
    return result


def validate_selection(item, text):
    if set(parse_listing(text)) != set(item["tests"]):
        raise ValueError("Native test filters changed the planned coverage")


def canonical(value):
    return json.dumps(value, sort_keys=True, separators=(",", ":"), allow_nan=False)


def portable_plan(groups):
    return [{**item, "executable": re.split(r"[/\\]", item["executable"])[-1]} for item in groups]


def validate_shards(groups, shards):
    early = [item for item in groups if item["priority"]]
    remaining = [item for item in groups if not item["priority"]]
    if len(early) != 3 or len(shards) != 2 or any(
            canonical(shard[:3]) != canonical(early) for shard in shards):
        raise ValueError("Every native shard must retain the three early guards")
    selected = [item for shard in shards for item in shard[3:]]
    expected = collections.Counter((item["executable"], test) for item in remaining for test in item["tests"])
    actual = collections.Counter((item["executable"], test) for item in selected for test in item["tests"])
    if (actual != expected or any(count != 1 for count in actual.values())
            or collections.Counter(map(canonical, selected)) != collections.Counter(map(canonical, remaining))):
        raise ValueError("Native shard coverage is incomplete, repeated or changed")


def shard_plans(groups):
    early = [item for item in groups if item["priority"]]
    remaining = [item for item in groups if not item["priority"]]
    assigned, loads = [[], []], [0, 0]
    def weight(index):
        item = remaining[index]
        return SHARD_WEIGHTS.get(name(item["executable"]), max(1, len(item["tests"])))
    for index in sorted(range(len(remaining)), key=lambda index: (-weight(index), index)):
        shard = min(range(2), key=lambda shard: (loads[shard], shard))
        assigned[shard].append(index)
        loads[shard] += weight(index)
    shards = [early + [remaining[index] for index in sorted(indexes)] for indexes in assigned]
    validate_shards(groups, shards)
    return shards


def make_receipt(groups, identity):
    if (not isinstance(identity, dict) or set(identity) != {
            "source_sha", "rust_profile", "shard_index", "shard_count", "run_id", "run_attempt"}
            or not isinstance(identity["source_sha"], str)
            or not re.fullmatch(r"[0-9a-f]{40}", identity["source_sha"])
            or identity["rust_profile"] not in ("minimum", "stable")
            or type(identity["shard_index"]) is not int or identity["shard_index"] not in (0, 1)
            or type(identity["shard_count"]) is not int or identity["shard_count"] != 2
            or any(type(identity[key]) is not int or identity[key] < 1 for key in ("run_id", "run_attempt"))):
        raise ValueError("Invalid native shard identity")
    shards = shard_plans(groups)
    selected = shards[identity["shard_index"]]
    reports = []
    for report, test in REPORT_TESTS.items():
        owners = [item for item in groups if name(item["executable"]) == "bbb_media" and test in item["tests"]]
        if len(owners) != 1:
            raise ValueError("Native media report coverage is missing or ambiguous")
        if owners[0] in selected:
            reports.append(report)
    complete = portable_plan(groups)
    digest = lambda value: hashlib.sha256(canonical(value).encode("utf-8")).hexdigest()
    return {"schema_version": 1, **identity, "inventory_sha256": digest(complete),
            "plan_sha256": digest(selected), "expected_reports": reports, "complete_plan": complete}


def validate_receipt(groups, selected, receipt, identity):
    expected = make_receipt(groups, identity)
    if (canonical(selected) != canonical(shard_plans(groups)[identity["shard_index"]])
            or canonical(receipt) != canonical(expected)):
        raise ValueError("Native shard receipt does not match this execution")


def validate_run_receipts(evidence, source, run_id, attempt):
    if not isinstance(evidence, list) or len(evidence) != 4:
        raise ValueError("Both Windows profiles require both native shard receipts")
    profiles = {rust: {} for rust in ("minimum", "stable")}
    for row in evidence:
        receipt, selected = row["receipt"], row["plan"]
        identity = {key: receipt[key] for key in
                    ("source_sha", "rust_profile", "shard_index", "shard_count", "run_id", "run_attempt")}
        expected = make_receipt(receipt["complete_plan"], identity)
        index, rust = identity["shard_index"], identity["rust_profile"]
        expected["plan_sha256"] = hashlib.sha256(canonical(selected).encode("utf-8")).hexdigest()
        artifact = f"windows-native-results-{rust}-shard-{index}-attempt-{attempt}"
        if (identity["source_sha"] != source or identity["run_id"] != run_id
                or identity["run_attempt"] != attempt or index in profiles[rust]
                or row["artifact"] != artifact or canonical(receipt) != canonical(expected)
                or canonical(portable_plan(selected)) != canonical(shard_plans(receipt["complete_plan"])[index])
                or sorted(row["reports"]) != sorted(expected["expected_reports"])):
            raise ValueError("Native shard receipts do not belong to the same qualification")
        profiles[rust][index] = row
    for shards in profiles.values():
        if set(shards) != {0, 1}:
            raise ValueError("A required native shard receipt is missing")
        complete = shards[0]["receipt"]["complete_plan"]
        if canonical(complete) != canonical(shards[1]["receipt"]["complete_plan"]):
            raise ValueError("Native shards used different compiled inventories")
        validate_shards(complete, [portable_plan(shards[index]["plan"]) for index in range(2)])
        reports = collections.Counter(report for row in shards.values() for report in row["reports"])
        if reports != collections.Counter({report: 1 for report in REPORT_TESTS}):
            raise ValueError("Each Windows profile must retain each media report once")


def check_shard_directory(directory, source, run_id, attempt):
    artifacts = {f"windows-native-results-{rust}-shard-{index}-attempt-{attempt}"
                 for rust in ("minimum", "stable") for index in range(2)}
    if {path.name for path in directory.iterdir()} != artifacts:
        raise ValueError("Native evidence artifacts are missing or belong to another configuration")
    evidence = []
    for artifact in sorted(artifacts):
        path = directory / artifact / "drmr-native-user"
        evidence.append({"artifact": artifact, "receipt": read_json(path / "shard-receipt.json"),
                         "plan": read_json(path / "test-plan.json"),
                         "reports": [report for report in REPORT_TESTS if (path / report).is_file()]})
    validate_run_receipts(evidence, source, run_id, attempt)


def list_tests(executable, arguments=()):
    result = subprocess.run([executable, *arguments, "--list", "--format", "terse"],
                            capture_output=True, text=True, check=True, timeout=60)
    if result.stderr.strip():
        raise ValueError("Native test inventory emitted diagnostics")
    return result.stdout


def plan(executables, *, windows=False, split_transport=True):
    if len(executables) != len(set(executables)):
        raise ValueError("Duplicate native test binaries")
    inventory = [{"executable": path, "tests": parse_listing(list_tests(path))} for path in executables]
    groups = build_plan(inventory, windows=windows, split_transport=split_transport)
    for item in groups:
        if item["arguments"]:
            validate_selection(item, list_tests(item["executable"], item["arguments"]))
    return groups


def read_json(path):
    def unique(pairs):
        value = {}
        for key, item in pairs:
            if key in value:
                raise ValueError("Ambiguous native plan JSON")
            value[key] = item
        return value
    def constant(_value):
        raise ValueError("Invalid native plan JSON")
    return json.loads(path.read_text(encoding="utf-8-sig"), object_pairs_hook=unique, parse_constant=constant)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--executables", type=Path)
    parser.add_argument("--output", type=Path)
    parser.add_argument("--windows", action="store_true")
    parser.add_argument("--shard-index", type=int, choices=(0, 1))
    parser.add_argument("--source-sha")
    parser.add_argument("--rust-profile", choices=("minimum", "stable"))
    parser.add_argument("--run-id", type=int)
    parser.add_argument("--run-attempt", type=int)
    parser.add_argument("--receipt", type=Path)
    parser.add_argument("--validate", action="store_true")
    parser.add_argument("--check-shards", type=Path)
    args = parser.parse_args()
    if args.check_shards is not None:
        if any((args.executables, args.output, args.windows, args.shard_index is not None,
                args.rust_profile, args.receipt, args.validate)):
            parser.error("Qualification consumes only the current run's shard evidence")
        check_shard_directory(args.check_shards, args.source_sha, args.run_id, args.run_attempt)
        print("Validated complete Windows native profiles from four matching shard receipts.")
        return
    if args.executables is None or args.output is None:
        parser.error("Planning requires compiled executables and an output path")
    identity = None
    if args.shard_index is not None:
        if not args.windows or args.receipt is None:
            parser.error("A Windows shard requires its identity and receipt")
        identity = {"source_sha": args.source_sha, "rust_profile": args.rust_profile,
                    "shard_index": args.shard_index, "shard_count": 2,
                    "run_id": args.run_id, "run_attempt": args.run_attempt}
    elif any(value is not None for value in (args.source_sha, args.rust_profile, args.run_id,
                                              args.run_attempt, args.receipt)) or args.validate:
        parser.error("Shard identity and receipt options require a Windows shard")
    paths = read_json(args.executables)
    if not isinstance(paths, list) or not all(isinstance(path, str) for path in paths):
        raise ValueError("Invalid native test binaries")
    groups = plan(paths, windows=args.windows)
    receipt = None
    if identity is not None:
        receipt = make_receipt(groups, identity)
        selected = shard_plans(groups)[args.shard_index]
        if args.validate:
            validate_receipt(groups, read_json(args.output), read_json(args.receipt), identity)
            print("Validated the native shard identity, complete inventory and selected plan.")
            return
        groups = selected
    with args.output.open("x", encoding="utf-8") as output:
        json.dump(groups, output)
        output.write("\n")
    if receipt is not None:
        with args.receipt.open("x", encoding="utf-8") as output:
            json.dump(receipt, output)
            output.write("\n")
    print(f"Planned {sum(len(item['tests']) for item in groups)} tests in {len(groups)} complete native groups.")


if __name__ == "__main__":
    main()
