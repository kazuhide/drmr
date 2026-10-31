"""Keep Desktop validation connected to its source and distribution inputs."""
import fnmatch
from pathlib import Path
import re
import unittest

ROOT = Path(__file__).resolve().parents[2]


def desktop_paths():
    text = (ROOT / ".github/workflows/desktop.yml").read_text(encoding="utf-8")
    block = text.split("    paths:\n", 1)[1].split("  workflow_dispatch:", 1)[0]
    return re.findall(r"^      - '([^']+)'$", block, re.MULTILINE)


class DesktopInputs(unittest.TestCase):
    def test_each_build_and_distribution_input_triggers_validation(self):
        paths = desktop_paths()
        inputs = [".gitattributes", "README.md", "CHANGELOG.md", "LICENSE", "Cargo.toml", "Cargo.lock",
                  ".github/scripts/minimum-rust.py", ".github/workflows/desktop.yml",
                  ".github/workflows/ci-scope.yml", "tools/ci_contract.py", "tools/tests/test_ci_contract.py",
                  "vendor/process-wrap/src/std/job_object.rs", "crates/drmr-test-support/build.rs",
                  "tools/build_desktop.py", "tools/collect_notices.py", "tools/verify_archive.py",
                  "tools/tests/test_build_desktop.py", "tools/tests/test_verify_archive.py",
                  "tools/tests/test_ci_inputs.py", "tools/desktop-licenses/manifest.json",
                  "docs/a-future-user-guide.md", "crates/drmr-desktop/src/main.rs"]
        inputs.extend(path.relative_to(ROOT).as_posix() for path in (ROOT / "docs").rglob("*") if path.is_file())
        for path in inputs:
            with self.subTest(path=path):
                self.assertTrue(any(fnmatch.fnmatchcase(path, pattern) for pattern in paths), path)

    def test_unrelated_work_does_not_trigger_native_builds(self):
        for path in ["notes/local.txt", "screenshots/example.png", "tools/measure_conversion.py"]:
            with self.subTest(path=path):
                self.assertFalse(any(fnmatch.fnmatchcase(path, pattern) for pattern in desktop_paths()))
