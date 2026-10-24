#!/usr/bin/env python3
"""Qualify measurement plumbing with the existing attributed two-track BBB presentation."""
import argparse
import hashlib
import importlib.util
import json
import os
from pathlib import Path

from conversion_regression import prepare_source
from regression import private_json, verify_fixture

SPEC = importlib.util.spec_from_file_location(
    "measure_conversion", Path(__file__).resolve().parents[5] / "tools/measure_conversion.py")
measure = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(measure)


def graph(root):
    result = {}
    for path in sorted(root.rglob("*")):
        if path.is_file():
            digest = hashlib.sha256()
            with path.open("rb") as source:
                while True:
                    block = source.read(65536)
                    if not block:
                        break
                    digest.update(block)
            result[path.relative_to(root).as_posix()] = digest.hexdigest()
    return result


def regression(binary, directory, condition="uncontrolled"):
    verify_fixture()
    source = prepare_source(directory / "source")
    before = graph(source.parent)
    keys = private_json(directory / "keys.json", {"schema_version": 1, "output": {
        "kid": "303132333435363738393a3b3c3d3e3f", "key": "40404040404040404040404040404040"}})
    arguments = measure.parse_args(["--drmr", str(binary), "--input", str(source), "--keys-file", str(keys),
                                    "--to", "cbcs", "--format", "dash", "--decode", "--repeats", "1", "--condition", condition])
    report = {"schema_version": 2, "runs": [], "success": False}
    measure.benchmark(arguments, report)
    assert report["success"] is True and len(report["runs"]) == 2
    assert len(report["warmups"]) == (2 if condition != "uncontrolled" else 0)
    assert all(row["status"] == "verified" for row in report["warmups"])
    assert before == graph(source.parent), "the source resource graph changed"
    for row in report["runs"]:
        assert row["status"] == "verified"
        for phase in ("convert", "verify"):
            value = row[phase]
            work = value["work_measurements"]
            memory = value["memory"]

            def observation_failure():
                return "measurement-observation-failed: " + json.dumps({
                    "condition": condition, "mode": row["mode"], "phase": phase,
                    "elapsed_seconds": value["elapsed_seconds"], "memory": memory,
                    "work_measurements": work}, allow_nan=False)

            assert any(tool["calls"] > 0 for tool in work["tools"]), observation_failure()
            assert work["workspace"]["checkpoints"] > 0, observation_failure()
            assert work["workspace"]["checkpoint_regular_file_bytes_max"] > 0, observation_failure()
            assert any(scope["phase"] == "verification" and scope["scopes"] > 0 for scope in work["phases"]), observation_failure()
            # A short real command can finish between observations. The separate
            # held-workload test requires positive samples through the real helper.
            measure.OBSERVER.validate({name: item for name, item in memory.items() if name != "observer_cleanup"})
            assert memory["metric_kind"] == ("windows-working-set" if os.name == "nt" else "unix-rss"), observation_failure()
            if os.name == "nt":
                assert memory["observer_cleanup"] in ("confirmed-exit", "confirmed-termination", "not-started"), observation_failure()
    encoded = json.dumps(report, allow_nan=False)
    assert str(directory) not in encoded and str(binary) not in encoded
    assert "40404040404040404040404040404040" not in encoded
    return report


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("--drmr", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--condition", choices=("uncontrolled", "warmed-by-equivalent-verified-run"), default="uncontrolled")
    arguments = parser.parse_args()
    print(json.dumps(regression(arguments.drmr, arguments.output, arguments.condition), allow_nan=False))
