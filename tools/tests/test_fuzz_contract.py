"""Only complete, bounded, exact-source codec fuzz results may qualify a release."""
import copy
import importlib.util
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parent))
import fuzz_reports

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "tools"))
SPEC = importlib.util.spec_from_file_location("fuzz_contract", ROOT / "tools/fuzz_contract.py")
CONTRACT = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(CONTRACT)


class FuzzContract(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        for name, data in fuzz_reports.files(CONTRACT.CODECS).items():
            path = self.root / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(data)
        self.git("init", "--quiet")
        self.git("add", ".")
        self.git("-c", "user.name=Fuzz Test", "-c", "user.email=fuzz@example.invalid", "commit", "--quiet", "-m", "Fixture")
        self.source = CONTRACT.VERIFY.Source(self.root)
        self.identity = CONTRACT.identity(self.source)
        self.report = fuzz_reports.complete(CONTRACT, self.identity, self.source.head, self.source.tree, "1.1.0")

    def git(self, *args):
        return subprocess.check_output(["git", *args], cwd=self.root, stderr=subprocess.PIPE)

    def validate(self, value):
        return CONTRACT.validate(value, self.source.head, self.source.tree, "1.1.0", self.identity)

    def test_complete_report_is_accepted(self):
        self.assertIs(self.validate(self.report), self.report)

    def test_all_public_objects_reject_unknown_and_missing_members(self):
        paths = [(), ("fuzz",), ("toolchain",), ("bounds",), ("audit",), ("targets", 0), ("targets", 0, "seed_admission")]
        for path in paths:
            for mutation in ("unknown", "missing"):
                value = copy.deepcopy(self.report)
                part = value
                for key in path:
                    part = part[key]
                if mutation == "unknown":
                    part["private-value"] = "must-never-pass"
                else:
                    part.pop(next(iter(part)))
                with self.subTest(path=path, mutation=mutation), self.assertRaises(CONTRACT.Failure):
                    self.validate(value)

    def test_stale_identity_relaxed_bounds_and_incomplete_execution_fail_closed(self):
        changes = [lambda r: r.update(source_head="f" * 40), lambda r: r.update(source_tree="f" * 40),
                   lambda r: r.update(version="1.0.3"), lambda r: r.update(success=False),
                   lambda r: r["fuzz"].update(lock_sha256="f" * 64),
                   lambda r: r["fuzz"].update(vendor_patch_sha256="f" * 64),
                   lambda r: r["bounds"].update(seconds=1), lambda r: r["bounds"].update(rss_mib=2048),
                   lambda r: r["bounds"].update(workers=2), lambda r: r["bounds"].update(seed=True),
                   lambda r: r["toolchain"].update(nightly="nightly"), lambda r: r["targets"].pop(),
                   lambda r: r["targets"].reverse(), lambda r: r["targets"][0].update(executions=0),
                   lambda r: r["targets"][0].update(executions=True), lambda r: r["targets"][0].update(coverage=0),
                   lambda r: r["targets"][0].update(exit_code=1), lambda r: r["targets"][0].update(cancelled=True),
                   lambda r: r["targets"][0].update(timed_out=True),
                   lambda r: r["targets"][0].update(elapsed_milliseconds=500),
                   lambda r: r["targets"][0].update(fuzzer_seconds=0),
                   lambda r: r["targets"][0].update(fuzzer_seconds=80),
                   lambda r: r["targets"][0].update(peak_rss_mib=513),
                   lambda r: r["targets"][0]["seed_admission"].update(accepted=3),
                   lambda r: r["targets"][0]["seed_admission"].update(refused=0),
                   lambda r: r["audit"].update(findings=1), lambda r: r["audit"].update(backports_verified=False),
                   lambda r: r["audit"].update(actual_lock_sha256="f" * 64),
                   lambda r: r.update(seed_check=False), lambda r: r.update(source_unchanged=False),
                   lambda r: r.update(owned_workspace_cleanup="unconfirmed")]
        for index, change in enumerate(changes):
            value = copy.deepcopy(self.report)
            change(value)
            with self.subTest(index=index), self.assertRaises(CONTRACT.Failure):
                self.validate(value)

    def test_source_lock_harness_seed_and_vendor_changes_cannot_reuse_identity(self):
        for name in ("fuzz/Cargo.lock", "fuzz/fuzz_targets/hevc.rs", "fuzz/seed-manifest.json", "vendor/private-fs/Cargo.toml"):
            path = self.root / name
            original = path.read_text()
            path.write_text(original + "\n")
            self.git("add", ".")
            self.git("-c", "user.name=Fuzz Test", "-c", "user.email=fuzz@example.invalid", "commit", "--quiet", "-m", "Changed fixture")
            changed = CONTRACT.identity(CONTRACT.VERIFY.Source(self.root))
            with self.subTest(name=name), self.assertRaises(CONTRACT.Failure):
                CONTRACT.validate(self.report, self.source.head, self.source.tree, "1.1.0", changed)
            path.write_text(original)


if __name__ == "__main__":
    unittest.main()
