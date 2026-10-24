"""Owned-console controller for real Windows CLI cancellation integration tests.

The Rust parent owns this process and all descendants in a managed job. This
controller detaches itself and allocates its own console before starting the CLI.
No event is sent until the console census excludes the parent and unrelated
processes. Detached media descendants require the CLI's own process-tree cleanup.
"""
from __future__ import annotations

import argparse
from contextlib import ExitStack
import ctypes
from ctypes import wintypes
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys
import time


WAIT_OBJECT_0 = 0
WAIT_TIMEOUT = 258
CREATE_NEW_PROCESS_GROUP = 0x200


class Failure(Exception):
    """A fixed classification, with no operator path or captured child output."""

    def __init__(self, reason, **evidence):
        super().__init__(reason)
        self.evidence = evidence


def require(condition, reason):
    if not condition:
        raise Failure(reason)


class Windows:
    def __init__(self):
        self.kernel = ctypes.WinDLL("kernel32", use_last_error=True)
        definitions = {
            "FreeConsole": ([], wintypes.BOOL),
            "AllocConsole": ([], wintypes.BOOL),
            "SetConsoleCtrlHandler": ([ctypes.c_void_p, wintypes.BOOL], wintypes.BOOL),
            "GenerateConsoleCtrlEvent": ([wintypes.DWORD, wintypes.DWORD], wintypes.BOOL),
            "GetConsoleProcessList": ([ctypes.POINTER(wintypes.DWORD), wintypes.DWORD], wintypes.DWORD),
            "OpenProcess": ([wintypes.DWORD, wintypes.BOOL, wintypes.DWORD], wintypes.HANDLE),
            "WaitForSingleObject": ([wintypes.HANDLE, wintypes.DWORD], wintypes.DWORD),
            "CloseHandle": ([wintypes.HANDLE], wintypes.BOOL),
            "CreateFileW": ([wintypes.LPCWSTR, wintypes.DWORD, wintypes.DWORD,
                             ctypes.c_void_p, wintypes.DWORD, wintypes.DWORD,
                             wintypes.HANDLE], wintypes.HANDLE),
        }
        for name, (arguments, result) in definitions.items():
            function = getattr(self.kernel, name)
            function.argtypes = arguments
            function.restype = result

    def checked(self, value, reason):
        if not value:
            raise Failure(f"{reason}:{ctypes.get_last_error()}")
        return value

    def ignore_ctrl_c(self, ignore):
        self.checked(self.kernel.SetConsoleCtrlHandler(None, ignore), "console-handler-failed")

    def free_console(self):
        self.checked(self.kernel.FreeConsole(), "console-detach-failed")

    def alloc_console(self):
        self.checked(self.kernel.AllocConsole(), "console-allocate-failed")

    def console_members(self):
        # This test creates only three attached clients. Fail rather than guess
        # or send a broadcast if the console has an unexpected population.
        members = (wintypes.DWORD * 64)()
        count = self.checked(self.kernel.GetConsoleProcessList(members, len(members)),
                             "console-census-failed")
        require(count <= len(members), "console-census-too-large")
        return set(members[:count])

    def event(self, kind, group):
        self.checked(self.kernel.GenerateConsoleCtrlEvent(kind, group), "console-event-failed")

    def retain_process(self, pid, resources):
        handle = self.checked(self.kernel.OpenProcess(0x00100000, False, pid),
                              "process-handle-failed")  # SYNCHRONIZE
        resources.callback(self.close_handle, handle)
        return handle

    def close_handle(self, handle):
        self.checked(self.kernel.CloseHandle(handle), "process-handle-close-failed")

    def wait_zero(self, handle):
        state = self.kernel.WaitForSingleObject(handle, 0)
        require(state in (WAIT_OBJECT_0, WAIT_TIMEOUT), "process-state-failed")
        return state

    def lock_is_held(self, path):
        handle = self.kernel.CreateFileW(str(path), 0xC0000000, 0, None, 3, 0x80, None)
        if handle == ctypes.c_void_p(-1).value:
            require(ctypes.get_last_error() == 32, "lock-open-unexpected-error")
            return True
        self.checked(handle, "lock-open-failed")
        self.checked(self.kernel.CloseHandle(handle), "lock-close-failed")
        return False


def isolate_console(api, controller):
    # Console membership is independent of job membership. Detach only this
    # controller, then allocate and verify its console before any child birth.
    api.free_console()
    api.alloc_console()
    require(api.console_members() == {controller}, "controller-console-not-isolated")
    api.ignore_ctrl_c(False)


def signal_owned_console(api, event, controller, cli, media, descendant, runner):
    members = api.console_members()
    require(controller in members and cli in members and media in members,
            "owned-console-client-missing")
    require(runner not in members and descendant not in members, "console-isolation-failed")
    require(members <= {controller, cli, media}, "unrelated-console-client")
    if event == "ctrl-c":
        api.event(0, 0)  # Ctrl+C cannot target a nonzero process group.
    elif event == "ctrl-break":
        api.event(1, cli)
    else:
        raise Failure("unsupported-console-event")


def confirm_immediate_cleanup(api, handles, lock):
    # Collect every result before logging or file operations can hide a race.
    states = [api.wait_zero(handle) for handle in handles]
    if not all(state == WAIT_OBJECT_0 for state in states):
        raise Failure("descendant-not-terminated", process_states=states)
    require(not api.lock_is_held(lock), "descendant-lock-still-held")
    return states


def digest(path):
    result = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(65536), b""):
            result.update(chunk)
    return result.digest()


def ready_pid(path, child, deadline):
    while True:
        require(child.poll() is None, "cli-exited-before-media-ready")
        try:
            text = path.read_text(encoding="ascii")
        except FileNotFoundError:
            text = ""
        if text.endswith("\n") and text.strip().isdigit():
            return int(text.strip())
        require(time.monotonic() < deadline, "media-ready-timeout")
        time.sleep(0.005)


def run(args):
    api = Windows()
    isolate_console(api, os.getpid())
    case = args.case
    control = case / "control"
    temporary = case / "temporary"
    publications = case / "publications"
    temporary.mkdir()
    publications.mkdir()
    sentinel = publications / "sentinel"
    sentinel.write_bytes(b"unrelated output remains unchanged")
    source_digest = digest(args.clip)
    sentinel_digest = digest(sentinel)
    output = publications / "result"
    environment = dict(os.environ, PATH=str(case / "tools"), TEMP=str(temporary), TMP=str(temporary))
    invocation = [str(args.drmr), args.command]
    if args.command == "convert":
        invocation += [str(args.clip), "--to", "clear", "--format", "dash", "--output", str(output)]
    else:
        invocation += [str(args.clip), "--source", str(args.clip)]
    invocation += ["--decode", "--json", "--max-workers", "1"]
    # The console is already isolated and Ctrl+C processing enabled before child
    # birth. The later ignore attribute cannot affect the already-running CLI.
    flags = 0 if args.event == "ctrl-c" else CREATE_NEW_PROCESS_GROUP
    child = None
    with ExitStack() as resources:
        stdout = resources.enter_context((control / "cli.stdout").open("wb"))
        stderr = resources.enter_context((control / "cli.stderr").open("wb"))
        try:
            child = subprocess.Popen(invocation, stdin=subprocess.DEVNULL, stdout=stdout,
                                     stderr=stderr, env=environment, creationflags=flags)
            api.ignore_ctrl_c(True)
            deadline = time.monotonic() + 15
            media = ready_pid(control / "media.pid", child, deadline)
            descendant = ready_pid(control / "descendant.pid", child, deadline)
            handles = [api.retain_process(pid, resources) for pid in (child.pid, media, descendant)]
            require(all(api.wait_zero(handle) == WAIT_TIMEOUT for handle in handles),
                    "process-not-live-before-event")
            lock = control / "descendant.lock"
            require(api.lock_is_held(lock), "descendant-did-not-hold-lock")
            signal_owned_console(api, args.event, os.getpid(), child.pid, media,
                                 descendant, args.runner_pid)
            try:
                status = child.wait(timeout=10)
            except subprocess.TimeoutExpired as error:
                raise Failure("cli-cancellation-timeout") from error
            states = confirm_immediate_cleanup(api, handles, lock)
            require(status == 130, "cli-cancellation-exit-code")
            require(not output.exists(), "cancelled-output-published")
            require(set(publications.iterdir()) == {sentinel}, "conversion-workspace-remains")
            require(not any(temporary.iterdir()), "temporary-workspace-remains")
            require(digest(args.clip) == source_digest, "source-changed")
            require(digest(sentinel) == sentinel_digest, "sentinel-changed")
        finally:
            # This is failure cleanup only, never an oracle. The Rust owner also
            # finishes its whole job before accepting the report or deleting files.
            if child is not None and child.poll() is None:
                child.kill()
                try:
                    child.wait(timeout=5)
                except subprocess.TimeoutExpired as error:
                    raise Failure("failure-cleanup-unconfirmed") from error
    diagnostic = (control / "cli.stderr").read_bytes()
    require(b"cancelled" in diagnostic and b"cleanup-unconfirmed" not in diagnostic,
            "cli-cancellation-diagnostic")
    return {"passed": True, "command": args.command, "event": args.event,
            "exit_code": 130, "immediate_processes": len(handles),
            "process_states": states, "detached_descendant": True}


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--drmr", type=Path, required=True)
    parser.add_argument("--clip", type=Path, required=True)
    parser.add_argument("--case", type=Path, required=True)
    parser.add_argument("--runner-pid", type=int, required=True)
    parser.add_argument("--command", choices=("convert", "verify"), required=True)
    parser.add_argument("--event", choices=("ctrl-c", "ctrl-break"), required=True)
    args = parser.parse_args()
    try:
        require(sys.platform == "win32", "windows-console-required")
        report = run(args)
    except Failure as error:
        report = {"passed": False, "reason": str(error), **error.evidence}
    except Exception as error:
        report = {"passed": False, "reason": "console-controller-failed",
                  "exception_class": type(error).__name__}
    print(json.dumps(report, sort_keys=True), flush=True)
    return 0 if report["passed"] else 1


if __name__ == "__main__":
    raise SystemExit(main())
