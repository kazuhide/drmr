#!/usr/bin/env python3
"""Plan complete native test coverage from the compiled libtest inventory."""
import argparse
import collections
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


def build_plan(inventory, *, windows=False):
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
        if name(item["executable"]) == "conversion_ts_output" and selected:
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


def list_tests(executable, arguments=()):
    result = subprocess.run([executable, *arguments, "--list", "--format", "terse"],
                            capture_output=True, text=True, check=True, timeout=60)
    if result.stderr.strip():
        raise ValueError("Native test inventory emitted diagnostics")
    return result.stdout


def plan(executables, *, windows=False):
    if len(executables) != len(set(executables)):
        raise ValueError("Duplicate native test binaries")
    inventory = [{"executable": path, "tests": parse_listing(list_tests(path))} for path in executables]
    groups = build_plan(inventory, windows=windows)
    for item in groups:
        if item["arguments"]:
            validate_selection(item, list_tests(item["executable"], item["arguments"]))
    return groups


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--executables", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--windows", action="store_true")
    args = parser.parse_args()
    paths = json.loads(args.executables.read_text(encoding="utf-8-sig"))
    if not isinstance(paths, list) or not all(isinstance(path, str) for path in paths):
        raise ValueError("Invalid native test binaries")
    groups = plan(paths, windows=args.windows)
    with args.output.open("x", encoding="utf-8") as output:
        json.dump(groups, output)
        output.write("\n")
    print(f"Planned {sum(len(item['tests']) for item in groups)} tests in {len(groups)} complete native groups.")


if __name__ == "__main__":
    main()
