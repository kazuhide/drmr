import fcntl
import json
import os
from pathlib import Path
import signal
import subprocess
import sys
import tempfile
import unittest
from unittest import mock

import capture
from process_owner import CleanupUnconfirmed, ProcessOwner, TOOLS, birth, signal_owned


class ProcessOwnerTests(unittest.TestCase):
    def setUp(self):
        self.root = Path(tempfile.mkdtemp(prefix="drmr-terminal-control-"))
        self.fake = self.root / "fake-tool"
        self.fake.write_text("#!/usr/bin/env python3\nimport fcntl, os, pathlib, time\n"
            "name=os.environ['TERMINAL_TEST_TOOL']\n"
            "lock=open(pathlib.Path(os.environ['TERMINAL_TEST_LOCKS'])/(name+'.lock'),'w')\n"
            "fcntl.flock(lock,fcntl.LOCK_EX)\ntime.sleep(60)\n")
        self.fake.chmod(0o700)
        self.owner = ProcessOwner(self.root, {name: str(self.fake) for name in TOOLS})

    def tearDown(self):
        self.assertTrue(self.owner.remove_temporary(), "owned control processes must be confirmed gone")

    def test_timeout_reaps_cli_and_all_detached_tools_and_releases_locks(self):
        command = self.root / "fake-cli.py"
        command.write_text("import fcntl, os, pathlib, subprocess, time\n"
            "root=pathlib.Path(os.environ['TERMINAL_TEST_LOCKS'])\n"
            "lock=open(root/'cli.lock','w'); fcntl.flock(lock,fcntl.LOCK_EX)\n"
            "for tool in ('ffmpeg','ffprobe','packager'):\n"
            " subprocess.Popen([tool,'--fake'],start_new_session=True,stdin=subprocess.DEVNULL,stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL)\n"
            "while len(list((root/'processes').glob('*.json')))<3 or not all((root/(name+'.lock')).exists() for name in ('ffmpeg','ffprobe','packager')): time.sleep(.01)\n"
            "(root/'ready').touch()\ntime.sleep(60)\n")
        environment = self.owner.environment(dict(os.environ, TERMINAL_TEST_LOCKS=str(self.root)), self.root / "control")
        with self.assertRaises((TimeoutError, subprocess.TimeoutExpired)):
            capture.execute([sys.executable, command], environment, timeout=2, owner=self.owner)
        self.assertTrue((self.root / "ready").exists(), "the negative must actually launch all three tool groups")
        self.assertEqual({row["tool"] for row in self.owner.receipts()}, set(TOOLS))
        self.assertTrue(self.owner.idle(), "timeout must leave no live CLI or native tool")
        for name in ("cli", *TOOLS):
            with open(self.root / (name + ".lock"), "a") as lock:
                fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)

    def test_a_reused_pid_identity_is_not_signalled(self):
        child = subprocess.Popen([sys.executable, "-c", "import time; time.sleep(60)"])
        try:
            identity = birth(child.pid)
            signal_owned({"pid": child.pid, "birth": identity + "-different"}, signal.SIGKILL)
            self.assertIsNone(child.poll())
        finally:
            child.kill()
            child.wait(timeout=2)

    def test_unknown_identity_retains_the_owned_temporary(self):
        path = self.owner.registry / "unknown.json"
        path.write_text(json.dumps({"pid": os.getpid(), "birth": birth(os.getpid()), "tool": "ffmpeg"}))
        try:
            with mock.patch("process_owner.birth", side_effect=CleanupUnconfirmed("unavailable")):
                self.assertFalse(self.owner.remove_temporary())
            self.assertTrue(self.root.exists())
        finally:
            path.unlink()


if __name__ == "__main__":
    unittest.main()
