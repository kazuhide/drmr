"""Bounded ownership for the terminal test's CLI and registered native tools."""

import ctypes
import errno
import json
import os
from pathlib import Path
import shutil
import signal
import subprocess
import sys
import time

TOOLS = ("ffmpeg", "ffprobe", "packager")
HERE = Path(__file__).resolve().parent


class CleanupUnconfirmed(RuntimeError):
    pass


class RUsageInfoV0(ctypes.Structure):
    # Darwin's public libproc rusage_info_v0; the birth clock survives exec.
    _fields_ = [("uuid", ctypes.c_uint8 * 16)] + [(name, ctypes.c_uint64) for name in
        ("user", "system", "idle", "interrupt", "pageins", "wired", "resident", "footprint", "birth", "exit")]


def darwin_libraries():
    system = ctypes.CDLL(None)
    system.task_name_for_pid.argtypes = [ctypes.c_uint32, ctypes.c_int, ctypes.POINTER(ctypes.c_uint32)]
    system.task_name_for_pid.restype = ctypes.c_int
    system.task_info.argtypes = [ctypes.c_uint32, ctypes.c_int, ctypes.c_void_p, ctypes.POINTER(ctypes.c_uint32)]
    system.task_info.restype = ctypes.c_int
    system.mach_port_deallocate.argtypes = [ctypes.c_uint32, ctypes.c_uint32]
    system.mach_port_deallocate.restype = ctypes.c_int
    proc = ctypes.CDLL("/usr/lib/libproc.dylib", use_errno=True)
    proc.proc_pid_rusage.argtypes = [ctypes.c_int, ctypes.c_int, ctypes.c_void_p]
    proc.proc_pid_rusage.restype = ctypes.c_int
    proc.proc_signal_with_audittoken.argtypes = [ctypes.c_void_p, ctypes.c_int]
    proc.proc_signal_with_audittoken.restype = ctypes.c_int
    return system, proc


def birth(pid):
    if sys.platform == "linux":
        try:
            fields = Path(f"/proc/{pid}/stat").read_text().rsplit(")", 1)[1].split()
        except FileNotFoundError:
            return None
        return None if fields[0] == "Z" else "linux:" + fields[19]
    if sys.platform == "darwin":
        _, proc = darwin_libraries()
        information = RUsageInfoV0()
        if proc.proc_pid_rusage(pid, 0, ctypes.byref(information)) != 0:
            if ctypes.get_errno() == errno.ESRCH:
                return None
            raise CleanupUnconfirmed("process-birth-unavailable")
        return None if information.exit else "darwin:" + str(information.birth)
    raise CleanupUnconfirmed("process-ownership-platform-unsupported")


def signal_owned(receipt, number):
    pid, expected = receipt["pid"], receipt["birth"]
    if birth(pid) != expected:
        return
    if sys.platform == "linux":
        descriptor = os.pidfd_open(pid)
        try:
            if birth(pid) == expected:
                signal.pidfd_send_signal(descriptor, number)
        finally:
            os.close(descriptor)
    elif sys.platform == "darwin":
        system, proc = darwin_libraries()
        self_port = ctypes.c_uint32.in_dll(system, "mach_task_self_").value
        port = ctypes.c_uint32()
        if system.task_name_for_pid(self_port, pid, ctypes.byref(port)) != 0:
            if birth(pid) != expected:
                return
            raise CleanupUnconfirmed("process-token-unavailable")
        try:
            token, count = (ctypes.c_uint32 * 8)(), ctypes.c_uint32(8)
            if system.task_info(port.value, 15, ctypes.byref(token), ctypes.byref(count)) != 0 or count.value != 8:
                if birth(pid) != expected:
                    return
                raise CleanupUnconfirmed("process-token-invalid")
            # Both the birth recheck and the kernel's audit-token PID version
            # must match; no PID-only fallback can signal a reused process.
            if birth(pid) == expected:
                result = proc.proc_signal_with_audittoken(ctypes.byref(token), number)
                if result not in (0, errno.ESRCH):
                    raise CleanupUnconfirmed("process-signal-unconfirmed")
        finally:
            system.mach_port_deallocate(self_port, port.value)


class ProcessOwner:
    def __init__(self, root, real_tools=None):
        self.root = Path(root).resolve()
        self.original = self.root.stat()
        self.registry = self.root / "processes"
        self.registry.mkdir(mode=0o700)
        self.proxies = self.root / "tools"
        self.proxies.mkdir(mode=0o700)
        self.real_tools = real_tools or {name: shutil.which(name) for name in TOOLS}
        if not all(self.real_tools.values()):
            raise RuntimeError("real-media-tool-unavailable")
        for name in TOOLS:
            path = self.proxies / name
            path.write_bytes((HERE / "packager.py").read_bytes())
            path.chmod(0o700)
        shutil.copyfile(HERE / "process_owner.py", self.proxies / "process_owner.py")
        self.children = []
        self.closing = False

    def environment(self, base, directory, outcome="normal"):
        directory.mkdir(mode=0o700, parents=True, exist_ok=True)
        control = directory / "control.json"
        descriptor = os.open(control, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
        with os.fdopen(descriptor, "w") as file:
            json.dump({"tools": self.real_tools, "registry": str(self.registry), "outcome": outcome}, file)
        return dict(base, TERMINAL_TEST_CONTROL=str(control), PATH=str(self.proxies) + os.pathsep + base["PATH"])

    def spawn(self, arguments, **options):
        if self.closing:
            raise CleanupUnconfirmed("process-owner-closed")
        child = subprocess.Popen([str(value) for value in arguments], start_new_session=True, **options)
        self.children.append(child)
        return child

    def execute(self, arguments, environment=None, timeout=30, input=None):
        child = self.spawn(arguments, env=environment, stdin=subprocess.PIPE if input is not None else subprocess.DEVNULL,
                           stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        try:
            stdout, _ = child.communicate(input, timeout=timeout)
        except subprocess.TimeoutExpired:
            if not self.close():
                raise CleanupUnconfirmed("timeout-cleanup-unconfirmed") from None
            raise TimeoutError("owned-command-deadline") from None
        if child.returncode:
            raise RuntimeError("media-command-failed")
        return stdout

    def receipts(self):
        if list(self.registry.glob("*.pending")):
            raise CleanupUnconfirmed("process-registration-incomplete")
        receipts = [json.loads(path.read_text()) for path in self.registry.glob("*.json")]
        if len(receipts) > 4096 or any(row.get("tool") not in TOOLS or not isinstance(row.get("pid"), int)
            or row["pid"] <= 1 or not isinstance(row.get("birth"), str) for row in receipts):
            raise CleanupUnconfirmed("process-registration-invalid")
        return receipts

    def idle(self):
        return all(child.poll() is not None for child in self.children) and all(
            birth(row["pid"]) != row["birth"] for row in self.receipts())

    def close(self):
        self.closing = True
        try:
            (self.registry / "closing").touch(mode=0o600, exist_ok=True)
            for child in self.children:
                if child.poll() is None:
                    # An unreaped direct child reserves its PID until wait().
                    child.terminate()
            deadline = time.monotonic() + 1
            while time.monotonic() < deadline and not self.idle():
                for row in self.receipts():
                    try:
                        signal_owned(row, signal.SIGTERM)
                    except ProcessLookupError:
                        pass
                time.sleep(0.025)
            for child in self.children:
                if child.poll() is None:
                    child.kill()
                child.wait(timeout=1)
                for stream in (child.stdin, child.stdout, child.stderr):
                    if stream:
                        stream.close()
            deadline = time.monotonic() + 2
            quiet = None
            while time.monotonic() < deadline:
                for row in self.receipts():
                    try:
                        signal_owned(row, signal.SIGKILL)
                    except ProcessLookupError:
                        pass
                if self.idle():
                    quiet = quiet or time.monotonic()
                    if time.monotonic() - quiet >= 0.2:
                        return True
                else:
                    quiet = None
                time.sleep(0.025)
        except (OSError, AttributeError, NotImplementedError, ValueError, KeyError, CleanupUnconfirmed, subprocess.TimeoutExpired):
            return False
        return False

    def remove_temporary(self):
        try:
            if not self.close():
                return False
            current = self.root.stat()
            if (current.st_uid, current.st_dev, current.st_ino) != (
                self.original.st_uid, self.original.st_dev, self.original.st_ino) or self.root.is_symlink():
                return False
            shutil.rmtree(self.root)
            return True
        except OSError:
            return False
