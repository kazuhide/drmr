"""Test-only held workloads and bounded ownership; no media reports or metrics are fabricated."""
import ctypes
import errno
import os
from pathlib import Path
import signal
import subprocess
import sys
import time


def wait_for(predicate, seconds=5):
    deadline = time.monotonic() + seconds
    while not predicate():
        if time.monotonic() >= deadline:
            raise RuntimeError("fixture-readiness-timeout")
        time.sleep(0.01)


def workload(role, directory):
    if role in ("--child", "--orphan-child"):
        memory = bytearray(64 * 1024 * 1024)
        for offset in range(0, len(memory), 4096):
            memory[offset] = 1
        (directory / "allocated").write_text("ready", encoding="ascii")
        if role == "--orphan-child":
            wait_for(lambda: (directory / "child-release").exists(), 15)
        else:
            sys.stdin.buffer.readline()
        return
    wait_for(lambda: (directory / "start").exists() or (directory / "release").exists(), 15)
    if (directory / "release").exists():
        print("{}")
        return
    child_role = "--orphan-child" if role == "--root-crash" else "--child"
    child = subprocess.Popen([sys.executable, __file__, child_role, str(directory)],
                             stdin=subprocess.PIPE, stdout=subprocess.DEVNULL,
                             stderr=subprocess.DEVNULL, close_fds=True)
    try:
        (directory / "child-pid").write_text(str(child.pid), encoding="ascii")
        wait_for(lambda: (directory / "allocated").exists())
        (directory / "ready").write_text("ready", encoding="ascii")
        if role == "--root-crash":
            # A deliberate owner failure leaves a live file-barrier child in its Job.
            os._exit(7)
        wait_for(lambda: (directory / "release").exists(), 15)
    finally:
        child.stdin.close()
        try:
            child.wait(timeout=5)
        except subprocess.TimeoutExpired:
            child.kill()
            child.wait(timeout=5)
            raise RuntimeError("fixture-child-timeout") from None
    if child.returncode != 0:
        raise RuntimeError("fixture-child-failed")
    print("{}")


class Job:
    """Assign the paused-at-barrier root before its only child can be born."""
    def __init__(self):
        from ctypes import wintypes as w
        self.kernel = ctypes.WinDLL("kernel32", use_last_error=True)

        class Basic(ctypes.Structure):
            _fields_ = [("process_time", ctypes.c_int64), ("job_time", ctypes.c_int64),
                        ("flags", w.DWORD), ("minimum", ctypes.c_size_t), ("maximum", ctypes.c_size_t),
                        ("active_limit", w.DWORD), ("affinity", ctypes.c_size_t),
                        ("priority", w.DWORD), ("scheduling", w.DWORD)]

        class Extended(ctypes.Structure):
            _fields_ = [("basic", Basic), ("io", ctypes.c_uint64 * 6),
                        ("process_memory", ctypes.c_size_t), ("job_memory", ctypes.c_size_t),
                        ("peak_process_memory", ctypes.c_size_t), ("peak_job_memory", ctypes.c_size_t)]

        class Accounting(ctypes.Structure):
            _fields_ = [("times", ctypes.c_int64 * 4), ("faults", w.DWORD),
                        ("total", w.DWORD), ("active", w.DWORD), ("terminated", w.DWORD)]

        self.Accounting = Accounting
        signatures = {
            "CreateJobObjectW": ([ctypes.c_void_p, w.LPCWSTR], w.HANDLE),
            "SetInformationJobObject": ([w.HANDLE, ctypes.c_int, ctypes.c_void_p, w.DWORD], w.BOOL),
            "AssignProcessToJobObject": ([w.HANDLE, w.HANDLE], w.BOOL),
            "TerminateJobObject": ([w.HANDLE, w.UINT], w.BOOL),
            "QueryInformationJobObject": ([w.HANDLE, ctypes.c_int, ctypes.c_void_p, w.DWORD, ctypes.c_void_p], w.BOOL),
            "WaitForSingleObject": ([w.HANDLE, w.DWORD], w.DWORD),
            "CloseHandle": ([w.HANDLE], w.BOOL),
        }
        for name, (arguments, result) in signatures.items():
            function = getattr(self.kernel, name)
            function.argtypes, function.restype = arguments, result
        self.handle = self.kernel.CreateJobObjectW(None, None)
        if not self.handle:
            raise RuntimeError("fixture-job-unavailable")
        info = Extended()
        info.basic.flags = 0x2000  # JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE
        if not self.kernel.SetInformationJobObject(self.handle, 9, ctypes.byref(info), ctypes.sizeof(info)):
            self.close()
            raise RuntimeError("fixture-job-unavailable")

    def assign(self, process):
        if not self.kernel.AssignProcessToJobObject(self.handle, int(process._handle)):
            raise RuntimeError("fixture-job-assignment-failed")

    def terminate(self):
        if not self.kernel.TerminateJobObject(self.handle, 1):
            raise RuntimeError("fixture-job-termination-failed")

    def drained(self, processes):
        info = self.Accounting()
        if not self.kernel.QueryInformationJobObject(self.handle, 1, ctypes.byref(info), ctypes.sizeof(info), None):
            raise RuntimeError("fixture-job-query-failed")
        states = [self.kernel.WaitForSingleObject(handle, 0) for handle in processes]
        if any(state not in (0, 258) for state in states):
            raise RuntimeError("fixture-process-query-failed")
        return info.active == 0 and all(state == 0 for state in states)

    def close(self):
        if self.handle is not None:
            handle, self.handle = self.handle, None
            if not self.kernel.CloseHandle(handle):
                raise RuntimeError("fixture-job-close-failed")


class OwnedWorkload:
    def __init__(self, directory, observer):
        self.directory, self.observer_api = directory, observer
        self.root = self.observer = self.child = None
        self.job = Job() if os.name == "nt" else None
        self._termination_attempted = self._cleanup_confirmed = False

    def start(self, process):
        self.root = process
        if self.job:
            self.job.assign(process)
        (self.directory / "start").write_text("start", encoding="ascii")

    def ready(self):
        wait_for(lambda: (self.directory / "ready").exists())
        pid = int((self.directory / "child-pid").read_text(encoding="ascii"))
        if self.job and self.child is None:
            self.child = self.observer_api.WindowsApi().open(pid)
        return pid

    def release(self):
        (self.directory / "release").write_text("release", encoding="ascii")

    def stop(self):
        if self._cleanup_confirmed:
            return
        release_failed = False
        try:
            self.release()
        except (OSError, RuntimeError):
            release_failed = True
        if self.root is None:
            if release_failed:
                raise RuntimeError("fixture-release-failed")
            return
        cleanup_failed = False

        def terminate_once():
            nonlocal cleanup_failed
            if self._termination_attempted:
                return
            self._termination_attempted = True
            try:
                if self.job:
                    self.job.terminate()
                elif self.root.returncode is None:
                    # The failed-release fixture remains at its owned barrier.
                    # Never signal a numeric group after its root was reaped.
                    os.killpg(self.root.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
            except (OSError, RuntimeError):
                cleanup_failed = True

        if release_failed or (self.job and self.root.returncode not in (None, 0)):
            terminate_once()
        try:
            self.root.wait(timeout=5)
        except subprocess.TimeoutExpired:
            terminate_once()
            try:
                self.root.wait(timeout=5)
            except subprocess.TimeoutExpired:
                cleanup_failed = True
        # Another waiter can observe an abnormal exit while this waiter runs.
        if self.job and self.root.returncode not in (None, 0):
            terminate_once()
        if self.job:
            handles = [int(self.root._handle)] + ([self.child.handle] if self.child else [])
            try:
                wait_for(lambda: self.job.drained(handles))
            except RuntimeError:
                cleanup_failed = True
            if self.child is None and self.root.returncode != 0:
                cleanup_failed = True
        else:
            def absent():
                try:
                    os.killpg(self.root.pid, 0)
                    return False
                except OSError as error:
                    if error.errno == errno.ESRCH:
                        return True
                    if error.errno == errno.EPERM:
                        return False  # Not absence; only ESRCH can confirm completion.
                    raise RuntimeError("fixture-group-query-failed") from None
            try:
                wait_for(absent)
            except RuntimeError:
                cleanup_failed = True
        if cleanup_failed:
            raise RuntimeError("fixture-owned-cleanup-unconfirmed")
        self._cleanup_confirmed = True
        if release_failed:
            raise RuntimeError("fixture-release-failed")

    def close(self):
        try:
            if self.child:
                self.child.close()
        finally:
            if self.job:
                self.job.close()


if __name__ == "__main__":
    workload(sys.argv[1], Path(sys.argv[2]))
