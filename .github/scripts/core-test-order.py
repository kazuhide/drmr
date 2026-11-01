#!/usr/bin/env python3
"""Run critical core regressions first while retaining Cargo's test supervision."""
import argparse
import importlib.util
import json
import os
from pathlib import Path
import re


SPEC = importlib.util.spec_from_file_location("native_test_plan", Path(__file__).with_name("native-test-plan.py"))
PLAN = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(PLAN)


def cargo_order(text):
    targets = {}
    for line in text.splitlines():
        item = json.loads(line)
        if item.get("reason") != "compiler-artifact" or not item.get("profile", {}).get("test") or not item.get("executable"):
            continue
        target = item["target"]
        path = item["executable"]
        if (path in targets or target["kind"] not in (["lib"], ["test"])
                or not re.fullmatch(r"[A-Za-z_][A-Za-z_0-9-]*", target["name"])):
            raise ValueError("Invalid or repeated compiled core test target")
        targets[path] = target
    groups = PLAN.plan(list(targets), split_transport=False)
    early_names = sorted(test for group in groups if group["priority"] for test in group["tests"])
    early_paths = {group["executable"] for group in groups if group["priority"]}
    prefix = ["test", "-p", "drmr-core", "--locked"]
    early = [*prefix]
    remaining = [*prefix]
    for path, target in targets.items():
        args = ["--lib"] if target["kind"] == ["lib"] else ["--test", target["name"]]
        if path in early_paths:
            early.extend(args)
        if PLAN.name(path) not in ("conversion_saved_hls_policy", "source_admission"):
            remaining.extend(args)
    skip = [value for test in PLAN.COMMON_CHECKS for value in ("--skip", test)]
    for group in groups:
        if group["priority"]:
            PLAN.validate_selection(group, PLAN.list_tests(group["executable"], ["--exact", *early_names]))
        else:
            PLAN.validate_selection(group, PLAN.list_tests(group["executable"], skip))
    return {"schema_version": 1, "early": [*early, "--", "--exact", *early_names, "--nocapture"],
            "remaining": [*remaining, "--", *skip, "--nocapture"]}


def command(value, phase):
    if (phase not in ("early", "remaining") or not isinstance(value, dict)
            or set(value) != {"schema_version", "early", "remaining"}
            or type(value["schema_version"]) is not int or value["schema_version"] != 1):
        raise ValueError("Invalid core test order")
    args = value[phase]
    if (not isinstance(args, list) or not all(isinstance(arg, str) for arg in args)
            or args[:4] != ["test", "-p", "drmr-core", "--locked"] or args.count("--") != 1):
        raise ValueError("Invalid core test command")
    index, targets = 4, []
    boundary = args.index("--")
    while index < boundary:
        flag = args[index]
        if flag == "--lib":
            targets.append("lib")
            index += 1
        elif flag == "--test" and index + 1 < boundary and re.fullmatch(r"[A-Za-z_][A-Za-z_0-9-]*", args[index + 1]):
            targets.append(args[index + 1])
            index += 2
        else:
            raise ValueError("Invalid core test target selector")
    if targets.count("lib") != 1 or len(targets) != len(set(targets)):
        raise ValueError("Missing or repeated core test targets")
    filters = args[boundary + 1:]
    if phase == "early":
        if (set(targets) != {"lib", "conversion_saved_hls_policy", "source_admission"}
                or len(filters) < 3 or filters[0] != "--exact" or filters[-1] != "--nocapture"
                or any(not re.fullmatch(r"[A-Za-z_][A-Za-z_0-9:]*", name) for name in filters[1:-1])
                or len(filters[1:-1]) != len(set(filters[1:-1]))
                or not set((*PLAN.COMMON_CHECKS, *PLAN.POLICY_CHECKS)) <= set(filters[1:-1])):
            raise ValueError("Invalid early core test filters")
    elif (set(targets) & {"conversion_saved_hls_policy", "source_admission"}
          or filters != [value for test in PLAN.COMMON_CHECKS for value in ("--skip", test)] + ["--nocapture"]):
        raise ValueError("Invalid remaining core test filters")
    return args


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--build", type=Path)
    parser.add_argument("--plan", type=Path, required=True)
    parser.add_argument("--execute", choices=("early", "remaining"))
    args = parser.parse_args()
    if args.execute:
        if args.build is not None:
            parser.error("Execution uses the existing compiled test plan")
        argv = command(json.loads(args.plan.read_text(encoding="utf-8")), args.execute)
        # Replace this process, rather than creating a second test supervisor.
        os.execvp("cargo", ["cargo", *argv])
    else:
        if args.build is None:
            parser.error("The compiled Cargo inventory is required")
        plan = cargo_order(args.build.read_text(encoding="utf-8"))
        with args.plan.open("x", encoding="utf-8") as output:
            json.dump(plan, output)
            output.write("\n")


if __name__ == "__main__":
    main()
