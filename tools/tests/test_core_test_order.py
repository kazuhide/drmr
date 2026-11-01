"""Keep early and remaining Cargo test selections complete and disjoint."""
import importlib.util
import json
from pathlib import Path
import unittest
from unittest import mock


PATH = Path(__file__).resolve().parents[2] / ".github/scripts/core-test-order.py"
SPEC = importlib.util.spec_from_file_location("core_test_order", PATH)
ORDER = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(ORDER)


class CoreTestOrder(unittest.TestCase):
    def fixture(self):
        tests = {
            "drmr_core": [*ORDER.PLAN.COMMON_CHECKS, "unrelated::case"],
            "conversion_saved_hls_policy": [*ORDER.PLAN.POLICY_CHECKS, "new_policy_case"],
            "source_admission": ["source_guard"],
            "new_integration": ["new_integration_case"],
        }
        inventory, rows = [], []
        for target, names in tests.items():
            path = target + "-0123456789abcdef"
            inventory.append({"executable": path, "tests": names})
            rows.append({"reason": "compiler-artifact", "profile": {"test": True}, "executable": path,
                         "target": {"kind": ["lib"] if target == "drmr_core" else ["test"], "name": target}})
        def listing(path, arguments=()):
            selected = tests[ORDER.PLAN.name(path)]
            if "--exact" in arguments:
                selected = [name for name in selected if name in arguments]
            skipped = [arguments[index + 1] for index, arg in enumerate(arguments) if arg == "--skip"]
            selected = [name for name in selected if not any(skip in name for skip in skipped)]
            return "".join(name + ": test\n" for name in selected)
        return tests, inventory, rows, listing

    def test_cargo_runs_every_old_and_new_test_once_including_whole_policy_binary(self):
        tests, inventory, rows, listing = self.fixture()
        groups = ORDER.PLAN.build_plan(inventory, split_transport=False)
        with mock.patch.object(ORDER.PLAN, "plan", return_value=groups), mock.patch.object(ORDER.PLAN, "list_tests", side_effect=listing):
            plan = ORDER.cargo_order("\n".join(json.dumps(row) for row in rows))
        selected = []
        for phase in ("early", "remaining"):
            args = ORDER.command(plan, phase)
            targets = [args[index + 1] for index, arg in enumerate(args) if arg == "--test"] + ["drmr_core"]
            filters = args[args.index("--") + 1:]
            for target in targets:
                selected.extend((target, line[:-6]) for line in listing(target + "-0123456789abcdef", filters).splitlines())
        self.assertCountEqual(selected, [(target, name) for target, names in tests.items() for name in names])
        self.assertEqual(len(selected), len(set(selected)))
        self.assertIn(("new_integration", "new_integration_case"), selected)
        self.assertIn(("conversion_saved_hls_policy", "new_policy_case"), selected)

    def test_duplicate_or_unknown_compiled_target_cannot_be_silently_dropped(self):
        _tests, _inventory, rows, _listing = self.fixture()
        for altered in ([*rows, rows[0]], [{**rows[0], "target": {"kind": ["bin"], "name": "unexpected"}}]):
            with self.subTest(altered=altered), self.assertRaises(ValueError):
                ORDER.cargo_order("\n".join(json.dumps(row) for row in altered))

    def test_command_file_cannot_add_cargo_configuration_or_drop_targets(self):
        for args in (["test", "-p", "drmr-core", "--locked", "--config", "bad", "--", "--nocapture"],
                     ["test", "-p", "drmr-core", "--locked", "--", "--nocapture"]):
            plan = {"schema_version": 1, "early": args, "remaining": args}
            with self.subTest(args=args), self.assertRaises(ValueError):
                ORDER.command(plan, "early")


if __name__ == "__main__":
    unittest.main()
