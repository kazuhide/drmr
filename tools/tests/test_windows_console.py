"""Host tests of the controller's event-isolation and immediate-cleanup oracles.

These do not replace the Windows tests that deliver actual console events.
"""
import importlib.util
from pathlib import Path
from types import SimpleNamespace
import unittest
from unittest.mock import patch


FIXTURE = Path(__file__).resolve().parents[2] / "crates/drmr/tests/fixtures/windows_console.py"
SPEC = importlib.util.spec_from_file_location("drmr_windows_console", FIXTURE)
CONTROLLER = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(CONTROLLER)


class Api:
    def __init__(self, members=(11, 12, 13), states=(0, 0, 0), locked=False):
        self.members = set(members)
        self.states = iter(states)
        self.locked = locked
        self.calls = []

    def console_members(self):
        return self.members

    def event(self, kind, group):
        self.calls.append(("event", kind, group))

    def wait_zero(self, handle):
        self.calls.append(("wait", handle))
        return next(self.states)

    def lock_is_held(self, path):
        self.calls.append(("lock", path))
        return self.locked


class ConsoleOracles(unittest.TestCase):
    def signal(self, api, event):
        CONTROLLER.signal_owned_console(api, event, 11, 12, 13, 14, 15)

    def test_ctrl_c_uses_only_its_isolated_console_broadcast(self):
        api = Api()
        self.signal(api, "ctrl-c")
        self.assertEqual(api.calls, [("event", 0, 0)])

    def test_ctrl_break_targets_the_cli_group(self):
        api = Api()
        self.signal(api, "ctrl-break")
        self.assertEqual(api.calls, [("event", 1, 12)])

    def test_runner_detached_child_or_unrelated_members_prevent_delivery(self):
        for extra in (14, 15, 16):
            api = Api(members=(11, 12, 13, extra))
            with self.assertRaises(CONTROLLER.Failure):
                self.signal(api, "ctrl-c")
            self.assertEqual(api.calls, [])

    def test_all_process_states_are_collected_before_any_lock_io_even_on_failure(self):
        api = Api(states=(258, 0, 0))
        with self.assertRaisesRegex(CONTROLLER.Failure, "descendant-not-terminated"):
            CONTROLLER.confirm_immediate_cleanup(api, (21, 22, 23), "lock")
        self.assertEqual(api.calls, [("wait", 21), ("wait", 22), ("wait", 23)])

    def test_an_exclusive_lock_still_prevents_completion_after_signaled_processes(self):
        api = Api(locked=True)
        with self.assertRaisesRegex(CONTROLLER.Failure, "descendant-lock-still-held"):
            CONTROLLER.confirm_immediate_cleanup(api, (21, 22, 23), "lock")
        self.assertEqual(api.calls, [("wait", 21), ("wait", 22), ("wait", 23), ("lock", "lock")])

    def test_success_returns_the_observed_immediate_states(self):
        api = Api()
        states = CONTROLLER.confirm_immediate_cleanup(api, (21, 22, 23), "lock")
        self.assertEqual(states, [0, 0, 0])
        self.assertEqual(api.calls, [("wait", 21), ("wait", 22), ("wait", 23), ("lock", "lock")])


class SetupApi(Api):
    def __init__(self, fail=None, allocated_members=(11,)):
        super().__init__(members=(11, 99))
        self.fail = fail
        self.allocated_members = set(allocated_members)

    def free_console(self):
        self.calls.append(("detach",))
        if self.fail == "detach":
            raise CONTROLLER.Failure("console-detach-failed")
        self.members = set()

    def alloc_console(self):
        self.calls.append(("allocate",))
        if self.fail == "allocate":
            raise CONTROLLER.Failure("console-allocate-failed")
        self.members = self.allocated_members

    def console_members(self):
        self.calls.append(("census",))
        return self.members

    def ignore_ctrl_c(self, ignore):
        self.calls.append(("ignore-ctrl-c", ignore))


class ConsoleSetup(unittest.TestCase):
    def test_inherited_console_is_detached_before_allocating_and_checking_a_new_one(self):
        api = SetupApi()
        CONTROLLER.isolate_console(api, 11)
        self.assertEqual(api.members, {11})
        self.assertEqual(api.calls, [
            ("detach",), ("allocate",), ("census",), ("ignore-ctrl-c", False)
        ])

    def test_detach_failure_does_not_allocate_or_dispatch_an_event(self):
        api = SetupApi(fail="detach")
        with self.assertRaisesRegex(CONTROLLER.Failure, "console-detach-failed"):
            CONTROLLER.isolate_console(api, 11)
        self.assertEqual(api.calls, [("detach",)])

    def test_allocation_failure_does_not_inspect_or_signal_the_old_console(self):
        api = SetupApi(fail="allocate")
        with self.assertRaisesRegex(CONTROLLER.Failure, "console-allocate-failed"):
            CONTROLLER.isolate_console(api, 11)
        self.assertEqual(api.calls, [("detach",), ("allocate",)])

    def test_unexpected_client_after_allocation_fails_before_enabling_events(self):
        api = SetupApi(allocated_members=(11, 99))
        with self.assertRaisesRegex(CONTROLLER.Failure, "controller-console-not-isolated"):
            CONTROLLER.isolate_console(api, 11)
        self.assertEqual(api.calls, [("detach",), ("allocate",), ("census",)])

    def test_failed_console_allocation_prevents_cli_birth(self):
        api = SetupApi(fail="allocate")
        with patch.object(CONTROLLER, "Windows", return_value=api), \
                patch.object(CONTROLLER.subprocess, "Popen") as launch:
            with self.assertRaisesRegex(CONTROLLER.Failure, "console-allocate-failed"):
                CONTROLLER.run(SimpleNamespace())
        launch.assert_not_called()


if __name__ == "__main__":
    unittest.main()
