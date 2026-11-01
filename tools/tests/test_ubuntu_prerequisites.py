"""Keep Ubuntu package acquisition bounded without weakening dependency checks."""
import json
import os
from pathlib import Path
import shutil
import signal
import subprocess
import sys
import tempfile
import time
import unittest


ROOT = Path(__file__).resolve().parents[2]
SCRIPT = ROOT / ".github/scripts/ubuntu-prerequisites.sh"


def gnu_timeout():
    for name in ("gtimeout", "timeout"):
        command = shutil.which(name)
        if command:
            result = subprocess.run([command, "--version"], capture_output=True, text=True)
            if result.returncode == 0 and "GNU coreutils" in result.stdout:
                return command
    return None


TIMEOUT = gnu_timeout() if os.name == "posix" else None


@unittest.skipUnless(TIMEOUT and shutil.which("bash"), "Ubuntu helper requires Bash and GNU timeout")
class UbuntuPrerequisites(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="ci-apt-")
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.bin = self.root / "bin"
        self.bin.mkdir()
        self.events = self.root / "events.jsonl"
        self.heartbeat = self.root / "heartbeat"
        self.group = self.root / "group"
        self.env = {**os.environ, "PATH": str(self.bin) + os.pathsep + os.environ["PATH"],
                    "CI_TEST_EVENTS": str(self.events), "CI_TEST_HEARTBEAT": str(self.heartbeat),
                    "CI_TEST_GROUP": str(self.group), "CI_TEST_TIMEOUT": TIMEOUT,
                    "CI_TEST_DEADLINE": "5s"}
        self.executable("sudo", """
import os, sys
assert sys.argv[1] == '-n'
os.environ['CI_TEST_PRIVILEGED'] = 'yes'
os.execvp(sys.argv[2], sys.argv[2:])
""")
        self.executable("timeout", """
import os, sys
assert os.environ.get('CI_TEST_PRIVILEGED') == 'yes'
assert sys.argv[1:3] == ['--signal=KILL', '180s']
command = os.environ['CI_TEST_TIMEOUT']
os.execv(command, [command, '--signal=KILL',
                  os.environ['CI_TEST_DEADLINE'], *sys.argv[3:]])
""")
        self.executable("heartbeat", """
import os, signal, time
if os.environ.get('CI_TEST_IGNORE_TERM') or os.environ.get('CI_TEST_CHILD_IGNORE_TERM'):
    signal.signal(signal.SIGTERM, signal.SIG_IGN)
with open(os.environ['CI_TEST_HEARTBEAT'], 'ab', buffering=0) as output:
    while True:
        output.write(b'x')
        time.sleep(0.02)
""")
        self.executable("apt-get", """
import json, os, signal, subprocess, sys, time
from pathlib import Path
phase = 'update' if 'update' in sys.argv[1:] else 'install'
with open(os.environ['CI_TEST_EVENTS'], 'a') as output:
    output.write(json.dumps({'phase': phase, 'args': sys.argv[1:],
                            'frontend': os.environ.get('DEBIAN_FRONTEND')}) + '\\n')
if os.environ.get('CI_TEST_HANG') == phase:
    Path(os.environ['CI_TEST_GROUP']).write_text(str(os.getpgrp()))
    if os.environ.get('CI_TEST_IGNORE_TERM'):
        signal.signal(signal.SIGTERM, signal.SIG_IGN)
    child = str(Path(__file__).with_name('heartbeat'))
    child_process = subprocess.Popen([sys.executable, child], stdout=subprocess.DEVNULL,
                                     stderr=subprocess.DEVNULL)
    while True:
        time.sleep(1)
if phase == 'update' and os.environ.get('CI_TEST_PARTIAL_INDEX'):
    raise SystemExit(100 if '--error-on=any' in sys.argv else 0)
raise SystemExit(int(os.environ.get('CI_TEST_' + phase.upper() + '_EXIT', '0')))
""")

    def executable(self, name, body):
        path = self.bin / name
        path.write_text("#!" + sys.executable + "\n" + body, encoding="utf-8")
        path.chmod(0o700)

    def run_helper(self, profile, **variables):
        env = {**self.env, **variables}
        process = subprocess.Popen(["bash", str(SCRIPT), profile], env=env,
                                   stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                                   start_new_session=True)
        try:
            stdout, stderr = process.communicate(timeout=8)
            if self.group.exists() and self.heartbeat.exists():
                size = self.heartbeat.stat().st_size
                time.sleep(0.1)
                self.assertEqual(self.heartbeat.stat().st_size, size,
                                 "package-manager descendants outlived the deadline")
            return process.returncode, stdout, stderr
        finally:
            groups = {process.pid}
            if self.group.exists():
                groups.add(int(self.group.read_text()))
            for group in groups:
                self.assertNotEqual(group, os.getpgrp())
                try:
                    os.killpg(group, signal.SIGKILL)
                except ProcessLookupError:
                    pass
            if process.poll() is None:
                process.communicate(timeout=2)

    def records(self):
        return [json.loads(line) for line in self.events.read_text().splitlines()]

    def test_both_profiles_install_the_original_dependencies_after_strict_update(self):
        for profile, packages in [("media", ["python3", "nodejs", "unzip", "xz-utils"]),
                                  ("glib", ["libglib2.0-dev", "pkg-config"])]:
            with self.subTest(profile=profile):
                self.events.unlink(missing_ok=True)
                status, _, error = self.run_helper(profile)
                self.assertEqual(status, 0, error)
                update, install = self.records()
                self.assertEqual([update["phase"], install["phase"]], ["update", "install"])
                self.assertIn("--error-on=any", update["args"])
                self.assertEqual(install["args"][install["args"].index("--") + 1:], packages)
                self.assertIn("--no-install-recommends", install["args"])
                for record in (update, install):
                    self.assertEqual(record["frontend"], "noninteractive")
                    for option in ("Acquire::Retries=2", "Acquire::http::Timeout=20",
                                   "Acquire::https::Timeout=20", "DPkg::Lock::Timeout=30"):
                        self.assertIn(option, record["args"])

    def test_failed_index_refresh_cannot_continue_with_stale_indexes(self):
        status, _, _ = self.run_helper("media", CI_TEST_PARTIAL_INDEX="1")
        self.assertEqual(status, 100)
        self.assertEqual([r["phase"] for r in self.records()], ["update"])

    def test_failed_package_installation_is_not_success(self):
        status, _, _ = self.run_helper("glib", CI_TEST_INSTALL_EXIT="100")
        self.assertEqual(status, 100)

    def test_stalled_update_and_install_stop_their_descendants(self):
        for phase in ("update", "install"):
            with self.subTest(phase=phase):
                self.events.unlink(missing_ok=True)
                self.heartbeat.unlink(missing_ok=True)
                self.group.unlink(missing_ok=True)
                status, _, _ = self.run_helper("media", CI_TEST_HANG=phase,
                                               CI_TEST_DEADLINE="1s", CI_TEST_IGNORE_TERM="1")
                self.assertNotEqual(status, 0)
                self.assertTrue(self.heartbeat.exists())
                size = self.heartbeat.stat().st_size
                time.sleep(0.1)
                self.assertEqual(self.heartbeat.stat().st_size, size)
                self.assertEqual(self.records()[-1]["phase"], phase)

    def test_unknown_profile_cannot_start_privileged_commands(self):
        status, _, _ = self.run_helper("unknown")
        self.assertEqual(status, 2)
        self.assertFalse(self.events.exists())

    def test_child_ignoring_term_cannot_outlive_its_terminating_parent(self):
        for phase in ("update", "install"):
            with self.subTest(phase=phase):
                self.events.unlink(missing_ok=True)
                self.heartbeat.unlink(missing_ok=True)
                self.group.unlink(missing_ok=True)
                status, _, _ = self.run_helper("media", CI_TEST_HANG=phase,
                                               CI_TEST_DEADLINE="1s",
                                               CI_TEST_CHILD_IGNORE_TERM="1")
                self.assertNotEqual(status, 0)
                self.assertTrue(self.heartbeat.exists())


class UbuntuPrerequisiteWiring(unittest.TestCase):
    def test_affected_jobs_use_the_bounded_installer(self):
        for file, profile in [(".github/actions/media-tools/action.yml", "media"),
                              (".github/workflows/advisories.yml", "glib")]:
            with self.subTest(file=file):
                text = (ROOT / file).read_text()
                self.assertIn("bash .github/scripts/ubuntu-prerequisites.sh " + profile, text)
                self.assertNotIn("sudo apt-get update", text)


if __name__ == "__main__":
    unittest.main()
