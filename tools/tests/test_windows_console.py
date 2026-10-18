"""Host tests of the controller's event-isolation and immediate-cleanup oracles.

These do not replace the Windows tests that deliver actual console events.
"""
import importlib.util
from pathlib import Path
import unittest


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


if __name__ == "__main__":
    unittest.main()
