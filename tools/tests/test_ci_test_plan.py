"""Require complete, once-only native test coverage when scheduling groups."""
import importlib.util
import collections
import copy
import hashlib
import json
from pathlib import Path
import tempfile
import unittest
from unittest import mock


PATH = Path(__file__).resolve().parents[2] / ".github/scripts/native-test-plan.py"
SPEC = importlib.util.spec_from_file_location("native_test_plan", PATH)
PLAN = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(PLAN)


def binary(name, tests):
    return {"executable": name + "-0123456789abcdef", "tests": tests}


class TestPlan(unittest.TestCase):
    def inventory(self):
        return [
            binary("drmr_core", [*PLAN.COMMON_CHECKS, "other_test"]),
            binary("conversion_saved_hls_policy", [*PLAN.POLICY_CHECKS, "new_policy_test"]),
            binary("source_admission", ["refuses_missing_media"]),
            binary("conversion_ts_output", [
                "video_all_nine_input_output_protection_combinations_preserve_every_access_unit",
                "audio_window::test_a", "audio_window::test_b", "new_module::new_test",
            ]),
            binary("conversion_abr_hls", ["abr"]),
            binary("bbb_media", [
                "the_real_cli_saves_converts_and_independently_verifies_the_bbb_multi_drm_plan",
                "the_real_cli_verifies_all_sixteen_bbb_protection_routes",
            ]),
            binary("empty_target", []),
        ]

    def test_early_regressions_and_remaining_inventory_run_exactly_once(self):
        inventory = self.inventory()
        groups = PLAN.build_plan(inventory)
        self.assertTrue(all(group["priority"] for group in groups[:3]))
        self.assertFalse(any(group["priority"] for group in groups[3:]))
        executed = [(group["executable"], name) for group in groups for name in group["tests"]]
        expected = [(item["executable"], name) for item in inventory for name in item["tests"]]
        self.assertCountEqual(executed, expected)
        self.assertEqual(len(executed), len(set(executed)))
        # A new module is included automatically, rather than silently omitted.
        self.assertIn((inventory[3]["executable"], "new_module::new_test"), executed)
        self.assertEqual(sum(group["executable"] == inventory[-1]["executable"] for group in groups), 1)

    def test_missing_critical_tests_and_ambiguous_binaries_are_refused(self):
        for kind in ("critical", "policy", "duplicate", "ambiguous", "duplicate-test"):
            inventory = self.inventory()
            if kind == "critical":
                inventory[0]["tests"].remove(PLAN.COMMON_CHECKS[0])
            elif kind == "policy":
                inventory[1]["tests"].remove(PLAN.POLICY_CHECKS[0])
            elif kind == "duplicate":
                inventory.append(inventory[0])
            elif kind == "ambiguous":
                inventory.append({**inventory[0], "executable": "drmr_core-fedcba9876543210"})
            else:
                inventory[3]["tests"].append(inventory[3]["tests"][0])
            with self.subTest(kind=kind), self.assertRaises(ValueError):
                PLAN.build_plan(inventory)

    def test_windows_keeps_platform_guards_and_executable_suffixes(self):
        inventory = self.inventory()
        inventory[0]["tests"].extend(PLAN.WINDOWS_CHECKS)
        for item in inventory:
            item["executable"] = "C:\\runner\\" + item["executable"] + ".exe"
        groups = PLAN.build_plan(inventory, windows=True)
        self.assertCountEqual(groups[0]["tests"], [*PLAN.COMMON_CHECKS, *PLAN.WINDOWS_CHECKS])
        for shard in PLAN.shard_plans(groups):
            self.assertCountEqual(shard[0]["tests"], [*PLAN.COMMON_CHECKS, *PLAN.WINDOWS_CHECKS])
        inventory[0]["tests"].remove(PLAN.WINDOWS_CHECKS[0])
        with self.assertRaises(ValueError):
            PLAN.build_plan(inventory, windows=True)

    def test_libtest_listing_is_strict_and_does_not_accept_zero_test_false_positive(self):
        self.assertEqual(PLAN.parse_listing("a::b: test\nother: test\n"), ["a::b", "other"])
        self.assertEqual(PLAN.parse_listing(""), [])
        for text in ("a: test\na: test\n", "a: benchmark\n", "Unexpected error\n", "x: test\x1b[0m"):
            with self.subTest(text=text), self.assertRaises(ValueError):
                PLAN.parse_listing(text)

    def test_selected_command_is_validated_against_real_listing(self):
        groups = PLAN.build_plan(self.inventory())
        group = next(group for group in groups if group["executable"].startswith("drmr_core") and not group["priority"])
        self.assertEqual(group["arguments"], ["--exact", "other_test"])
        PLAN.validate_selection(group, "other_test: test\n")
        with self.assertRaises(ValueError):
            PLAN.validate_selection(group, "other_test: test\nextra_test: test\n")

    def test_short_exclusions_do_not_repeat_early_guards_in_large_unit_binary(self):
        inventory = self.inventory()
        inventory[0]["tests"] += [f"case_{index}" for index in range(10)]
        groups = PLAN.build_plan(inventory)
        remainder = next(group for group in groups if group["executable"].startswith("drmr_core") and not group["priority"])
        self.assertEqual(remainder["arguments"].count("--skip"), len(PLAN.COMMON_CHECKS))
        PLAN.validate_selection(remainder, "".join(name + ": test\n" for name in remainder["tests"]))

    def test_long_route_matrices_are_spread_without_losing_new_tests(self):
        tests = ["a_twenty_seven_routes", "b_eighteen_routes", "c_nine_routes",
                 "d_nine_routes", "unknown::new_test", "short_test"]
        groups = PLAN.split_transport_tests(tests)
        self.assertEqual(len(groups), 3)
        self.assertCountEqual([name for group in groups for name in group], tests)
        self.assertNotEqual(next(index for index, group in enumerate(groups) if tests[0] in group),
                            next(index for index, group in enumerate(groups) if tests[1] in group))

    def test_two_runner_shards_repeat_only_early_guards_and_keep_all_remaining_tests(self):
        groups = PLAN.build_plan(self.inventory())
        shards = PLAN.shard_plans(groups)
        self.assertEqual(len(shards), 2)
        for shard in shards:
            self.assertEqual(shard[:3], groups[:3])
        remaining = [group for shard in shards for group in shard[3:]]
        expected = collections.Counter((group["executable"], test) for group in groups[3:]
                                       for test in group["tests"])
        actual = collections.Counter((group["executable"], test) for group in remaining
                                     for test in group["tests"])
        self.assertEqual(actual, expected)
        self.assertTrue(all(count == 1 for count in actual.values()))
        self.assertEqual(sum(group["executable"].startswith("empty_target") for group in remaining), 1)
        self.assertEqual(PLAN.shard_plans(groups), shards)
        # Both runners receive costly transport work; ABR is not repeated.
        self.assertTrue(all(any(PLAN.name(group["executable"]) == "conversion_ts_output"
                                for group in shard[3:]) for shard in shards))
        self.assertEqual(sum(PLAN.name(group["executable"]) == "conversion_abr_hls"
                             for group in remaining), 1)

    def test_shard_coverage_refuses_missing_repeated_or_changed_groups_and_guards(self):
        groups = PLAN.build_plan(self.inventory())
        shards = PLAN.shard_plans(groups)
        for mutate in [lambda value: value[0].pop(),
                       lambda value: value[1].append(value[0][3]),
                       lambda value: value[0][0]["tests"].pop(),
                       lambda value: value[0][3]["arguments"].append("--skip"),
                       lambda value: value.pop(),
                       lambda value: value.append(value[0])]:
            changed = copy.deepcopy(shards)
            mutate(changed)
            with self.subTest(mutate=mutate), self.assertRaises(ValueError):
                PLAN.validate_shards(groups, changed)

    def identity(self, shard):
        return {"source_sha": "a" * 40, "rust_profile": "stable", "shard_index": shard,
                "shard_count": 2, "run_id": 123, "run_attempt": 1}

    def test_shard_receipts_assign_both_media_reports_once_per_profile(self):
        groups = PLAN.build_plan(self.inventory())
        shards = PLAN.shard_plans(groups)
        receipts = [PLAN.make_receipt(groups, self.identity(index)) for index in range(2)]
        reports = collections.Counter(name for receipt in receipts for name in receipt["expected_reports"])
        self.assertEqual(reports, {"bbb-media-report.json": 1, "conversion-report.json": 1})
        for index, receipt in enumerate(receipts):
            PLAN.validate_receipt(groups, shards[index], receipt, self.identity(index))
            owns_bbb = any(PLAN.name(group["executable"]) == "bbb_media" for group in shards[index])
            self.assertEqual(bool(receipt["expected_reports"]), owns_bbb)

    def test_receipts_cannot_mix_sources_profiles_attempts_shards_or_inventory(self):
        groups = PLAN.build_plan(self.inventory())
        shards = PLAN.shard_plans(groups)
        receipt = PLAN.make_receipt(groups, self.identity(0))
        for key, value in [("source_sha", "b" * 40), ("rust_profile", "minimum"),
                           ("run_id", 124), ("run_attempt", 2), ("shard_index", 1),
                           ("shard_count", 1), ("schema_version", True),
                           ("inventory_sha256", "0" * 64), ("expected_reports", ["unexpected-report.json"])]:
            changed = {**receipt, key: value}
            with self.subTest(key=key), self.assertRaises(ValueError):
                PLAN.validate_receipt(groups, shards[0], changed, self.identity(0))
        changed = copy.deepcopy(groups)
        changed[-1]["tests"].append("new_test")
        with self.assertRaises(ValueError):
            PLAN.validate_receipt(changed, shards[0], receipt, self.identity(0))
        for index in [-1, 2, True]:
            with self.subTest(index=index), self.assertRaises(ValueError):
                PLAN.make_receipt(groups, self.identity(index))
        incomplete = [group for group in groups if PLAN.name(group["executable"]) != "bbb_media"]
        with self.assertRaises(ValueError):
            PLAN.make_receipt(incomplete, self.identity(0))
        selected = copy.deepcopy(shards[0])
        selected[0]["arguments"].append("--skip")
        with self.assertRaises(ValueError):
            PLAN.validate_receipt(groups, selected, receipt, self.identity(0))

    def test_plan_json_cannot_hide_duplicate_fields_or_nonfinite_numbers(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "receipt.json"
            for text in ['{"run_attempt": 2, "run_attempt": 1}', '{"run_id":NaN}', '{"run_id":Infinity}']:
                path.write_text(text, encoding="utf-8")
                with self.subTest(text=text), self.assertRaises(ValueError):
                    PLAN.read_json(path)

    def test_required_gate_reconciles_actual_shards_instead_of_mixing_receipts(self):
        groups = PLAN.build_plan(self.inventory())
        shards = PLAN.shard_plans(groups)
        evidence = []
        for rust in ("minimum", "stable"):
            for index in range(2):
                receipt = PLAN.make_receipt(groups, {**self.identity(index), "rust_profile": rust})
                evidence.append({"artifact": f"windows-native-results-{rust}-shard-{index}-attempt-1",
                                 "receipt": receipt, "plan": shards[index],
                                 "reports": receipt["expected_reports"]})
        PLAN.validate_run_receipts(evidence, "a" * 40, 123, 1)
        swapped = copy.deepcopy(evidence)
        swapped[0]["plan"], swapped[1]["plan"] = swapped[1]["plan"], swapped[0]["plan"]
        for row in swapped[:2]:
            row["receipt"]["plan_sha256"] = hashlib.sha256(PLAN.canonical(row["plan"]).encode()).hexdigest()
        with self.assertRaises(ValueError):
            PLAN.validate_run_receipts(swapped, "a" * 40, 123, 1)
        for mutate in [lambda rows: rows.pop(),
                       lambda rows: rows.append(rows[0]),
                       lambda rows: rows[0]["receipt"].update(source_sha="b" * 40),
                       lambda rows: rows[0]["receipt"].update(run_attempt=2),
                       lambda rows: rows[0]["receipt"].update(rust_profile="stable"),
                       lambda rows: rows[0]["receipt"].update(shard_index=1),
                       lambda rows: rows[0]["plan"].pop(),
                       lambda rows: rows[0]["receipt"]["complete_plan"][-1]["tests"].append("new_test"),
                       lambda rows: rows[0]["reports"].append("unexpected.json")]:
            changed = copy.deepcopy(evidence)
            mutate(changed)
            with self.subTest(mutate=mutate), self.assertRaises(ValueError):
                PLAN.validate_run_receipts(changed, "a" * 40, 123, 1)

    def test_gate_cli_reads_separate_artifacts_with_portable_inventory_binding(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            for rust in ("minimum", "stable"):
                for index in range(2):
                    groups = PLAN.build_plan(self.inventory())
                    for group in groups:
                        group["executable"] = f"D:\\runner-{index}\\" + group["executable"]
                    identity = {**self.identity(index), "rust_profile": rust}
                    receipt = PLAN.make_receipt(groups, identity)
                    path = root / f"windows-native-results-{rust}-shard-{index}-attempt-1" / "drmr-native-user"
                    path.mkdir(parents=True)
                    (path / "shard-receipt.json").write_text(json.dumps(receipt), encoding="utf-8")
                    (path / "test-plan.json").write_text(json.dumps(PLAN.shard_plans(groups)[index]), encoding="utf-8")
                    for report in receipt["expected_reports"]:
                        (path / report).write_text("{}", encoding="utf-8")
            args = ["native-test-plan", "--check-shards", str(root), "--source-sha", "a" * 40,
                    "--run-id", "123", "--run-attempt", "1"]
            with mock.patch("sys.argv", args):
                PLAN.main()
            report = next(root.glob("*/drmr-native-user/bbb-media-report.json"))
            report.unlink()
            with self.assertRaises(ValueError):
                PLAN.check_shard_directory(root, "a" * 40, 123, 1)
            report.write_text("{}", encoding="utf-8")
            (root / "old-attempt").mkdir()
            with self.assertRaises(ValueError):
                PLAN.check_shard_directory(root, "a" * 40, 123, 1)

    def test_cli_receipt_is_verified_before_execution_without_rewriting_evidence(self):
        groups = PLAN.build_plan(self.inventory())
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            executables, output, receipt = root / "executables.json", root / "plan.json", root / "receipt.json"
            executables.write_text(json.dumps([item["executable"] for item in self.inventory()]), encoding="utf-8")
            args = ["native-test-plan", "--windows", "--executables", str(executables),
                    "--output", str(output), "--receipt", str(receipt), "--shard-index", "0",
                    "--source-sha", "a" * 40, "--rust-profile", "stable", "--run-id", "123", "--run-attempt", "1"]
            with mock.patch.object(PLAN, "plan", return_value=groups), mock.patch("sys.argv", args):
                PLAN.main()
            self.assertEqual(json.loads(output.read_text()), PLAN.shard_plans(groups)[0])
            before = output.read_bytes(), receipt.read_bytes()
            with mock.patch.object(PLAN, "plan", return_value=groups), mock.patch("sys.argv", args + ["--validate"]):
                PLAN.main()
            self.assertEqual((output.read_bytes(), receipt.read_bytes()), before)
            changed = json.loads(receipt.read_text())
            changed["run_attempt"] = 2
            receipt.write_text(json.dumps(changed), encoding="utf-8")
            with mock.patch.object(PLAN, "plan", return_value=groups), mock.patch("sys.argv", args + ["--validate"]):
                with self.assertRaises(ValueError):
                    PLAN.main()


if __name__ == "__main__":
    unittest.main()
