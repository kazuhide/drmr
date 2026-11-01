"""Require complete, once-only native test coverage when scheduling groups."""
import importlib.util
from pathlib import Path
import unittest


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


if __name__ == "__main__":
    unittest.main()
