"""Keep documentation routing separate from native qualification evidence."""
import copy
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location("ci_contract", ROOT / "tools/ci_contract.py")
CI = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(CI)


def entry(value="a", mode="100644"):
    return (mode, "blob", value * 40)


class DocumentationBoundary(unittest.TestCase):
    def setUp(self):
        self.before = {"Cargo.toml": entry(), "crates/tool/src/main.rs": entry(),
                       "README.md": entry(), "docs/usage.md": entry(),
                       "site/guide/index.html": entry()}

    def test_only_reviewed_publication_inputs_can_use_the_lightweight_path(self):
        for path in ["README.md", "docs/usage.md", "docs/examples/README.md",
                     "site-content/guide/en.md", "site-content/guide/ja.md",
                     "site-content/guide/README.md", "site/assets/guide.css"]:
            with self.subTest(path=path):
                after = {**self.before, path: entry("b")}
                self.assertTrue(CI.documentation_only(self.before, after))

    def test_runtime_build_test_configuration_and_unknown_inputs_require_native_checks(self):
        for path in ["Cargo.toml", "Cargo.lock", ".cargo/config.toml", ".gitattributes",
                     "crates/drmr-manifest/README.md", "crates/tool/src/main.rs",
                     "crates/drmr-server/static/index.html", "vendor/library/README.md",
                     "LICENSE", "CHANGELOG.md", ".github/workflows/ci.yml",
                     "tools/conversion_examples.py", "tools/tests/test_conversion_examples.py",
                     "docs/examples/keys/a.json", "docs/benchmarks/a.json",
                     "site/new.js", "site/assets/unknown.svg", "unknown.md"]:
            with self.subTest(path=path):
                self.assertFalse(CI.documentation_only(self.before, {**self.before, path: entry("b")}))

    def test_mixed_changes_deletions_and_renames_cannot_hide_runtime_changes(self):
        after = {**self.before, "README.md": entry("b")}
        del after["crates/tool/src/main.rs"]
        self.assertFalse(CI.documentation_only(self.before, after))
        after["docs/main.md"] = self.before["crates/tool/src/main.rs"]
        self.assertFalse(CI.documentation_only(self.before, after))

    def test_document_deletion_is_permitted_but_empty_changes_are_not(self):
        after = dict(self.before)
        del after["docs/usage.md"]
        self.assertTrue(CI.documentation_only(self.before, after))
        self.assertFalse(CI.documentation_only(self.before, self.before))

    def test_symlinks_executable_modes_and_gitlinks_are_not_documents(self):
        for mode, kind in [("120000", "blob"), ("100755", "blob"), ("160000", "commit")]:
            with self.subTest(mode=mode):
                after = {**self.before, "README.md": (mode, kind, "b" * 40)}
                self.assertFalse(CI.documentation_only(self.before, after))
                self.assertFalse(CI.documentation_only(after, self.before))

    def test_site_allowlist_matches_the_validated_published_file_set(self):
        spec = importlib.util.spec_from_file_location("site_contract", ROOT / "tools/check_site.py")
        site = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(site)
        self.assertEqual(CI.SITE_FILES, {"site/" + name for name in site.FILES})


class SourceClassification(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="ci-contract-")
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.git("init", "-q")
        self.git("config", "user.name", "CI fixture")
        self.git("config", "user.email", "ci@example.invalid")
        (self.root / "Cargo.toml").write_text("runtime input\n", encoding="utf-8")
        (self.root / "README.md").write_text("initial guide\n", encoding="utf-8")
        self.base = self.commit()

    def git(self, *args):
        return subprocess.run(["git", "-c", "core.autocrlf=false", *args], cwd=self.root,
                              capture_output=True, check=True, text=True).stdout.strip()

    def commit(self):
        self.git("add", ".")
        self.git("commit", "-qm", "fixture")
        return self.git("rev-parse", "HEAD")

    def merge(self, path="README.md"):
        (self.root / path).write_text("changed input\n", encoding="utf-8")
        head = self.commit()
        self.git("checkout", "-q", "--detach", self.base)
        self.git("merge", "--no-ff", "-qm", "merge fixture", head)
        return {"pull_request": {"base": {"sha": self.base}, "head": {"sha": head}}}, self.git("rev-parse", "HEAD")

    def test_verified_merge_checkout_proves_equal_runtime_inventories(self):
        event, source = self.merge()
        report = CI.classify(self.root, "pull_request", event, source)
        self.assertEqual(report["profile"], "documentation")
        self.assertEqual(report["protected_base_sha256"], report["protected_head_sha256"])
        self.assertEqual(report["source_sha"], source)
        self.assertEqual(report["changed_files"], 1)

    def test_runtime_change_stays_full(self):
        event, source = self.merge("Cargo.toml")
        self.assertEqual(CI.classify(self.root, "pull_request", event, source)["profile"], "full")

    def test_main_manual_scheduled_and_unknown_events_remain_full(self):
        event, source = self.merge()
        for name in ["push", "workflow_dispatch", "schedule", "unexpected"]:
            with self.subTest(event=name):
                self.assertEqual(CI.classify(self.root, name, event, source)["profile"], "full")

    def test_invalid_source_graph_or_dirty_checkout_cannot_authorize_a_skip(self):
        event, source = self.merge()
        cases = [(event, "z" * 40), (event, event["pull_request"]["head"]["sha"]),
                 ({"pull_request": {"base": {"sha": "a" * 40}, "head": event["pull_request"]["head"]}}, source),
                 ({"pull_request": {"base": {"sha": self.base}}}, source)]
        for payload, expected in cases:
            with self.subTest(payload=payload):
                with self.assertRaises(CI.Refusal):
                    CI.classify(self.root, "pull_request", payload, expected)
        (self.root / "unexpected.txt").write_text("untracked input", encoding="utf-8")
        with self.assertRaises(CI.Refusal):
            CI.classify(self.root, "pull_request", event, source)

    def test_cli_writes_only_fixed_profile_and_valid_source_outputs(self):
        event, source = self.merge()
        with tempfile.TemporaryDirectory() as directory:
            controls = Path(directory)
            event_path, output, report = controls / "event.json", controls / "outputs", controls / "report.json"
            event_path.write_text(json.dumps(event), encoding="utf-8")
            env = {**os.environ, "GITHUB_EVENT_PATH": str(event_path), "GITHUB_EVENT_NAME": "pull_request",
                   "GITHUB_SHA": source, "GITHUB_OUTPUT": str(output), "GITHUB_STEP_SUMMARY": str(controls / "summary")}
            result = subprocess.run([sys.executable, str(ROOT / "tools/ci_contract.py"), "classify", "--report", str(report)],
                                    cwd=self.root, env=env, capture_output=True, text=True)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(output.read_text(), "profile=documentation\nsource_sha=" + source + "\n")
            self.assertTrue(json.loads(report.read_text())["success"])
            self.assertNotIn(str(self.root), result.stdout + result.stderr)
            report.unlink()
            output.unlink()
            event_path.write_text('{"pull_request":{"head":{"sha":"private-marker"}}}', encoding="utf-8")
            result = subprocess.run([sys.executable, str(ROOT / "tools/ci_contract.py"), "classify", "--report", str(report)],
                                    cwd=self.root, env=env, capture_output=True, text=True)
            self.assertEqual(result.returncode, 1)
            self.assertFalse(output.exists())
            self.assertNotIn("private-marker", result.stdout + result.stderr + report.read_text())


class NativeQualification(unittest.TestCase):
    def setUp(self):
        self.source, self.run, self.attempt = "a" * 40, 12, 2

    def jobs(self, workflow="ci.yml"):
        names = list(CI.NATIVE_JOBS[workflow])
        if workflow in CI.GATE_NAMES:
            names += [CI.SCOPE_NAME, CI.GATE_NAMES[workflow]]
        return [{"id": index, "name": name, "run_id": self.run, "run_attempt": self.attempt,
                 "head_sha": self.source, "status": "completed", "conclusion": "success"}
                for index, name in enumerate(names, 1)]

    def test_full_qualification_requires_every_actual_native_variant(self):
        for workflow in CI.NATIVE_JOBS:
            with self.subTest(workflow=workflow):
                jobs = self.jobs(workflow)
                CI.validate_jobs(workflow, jobs, self.source, self.run, self.attempt, release=True)
                with self.assertRaises(CI.Refusal):
                    CI.validate_jobs(workflow, jobs[1:], self.source, self.run, self.attempt, release=True)

    def test_skipped_failed_pending_duplicate_and_wrong_source_jobs_are_refused(self):
        for update in [{"conclusion": "skipped"}, {"conclusion": "failure"},
                       {"conclusion": "cancelled"}, {"conclusion": "neutral"},
                       {"status": "in_progress"}, {"head_sha": "b" * 40},
                       {"run_id": 13}, {"run_attempt": 1}]:
            with self.subTest(update=update):
                jobs = self.jobs()
                jobs[0].update(update)
                with self.assertRaises(CI.Refusal):
                    CI.validate_jobs("ci.yml", jobs, self.source, self.run, self.attempt)
        jobs = self.jobs()
        jobs.append({**jobs[0], "id": 99})
        with self.assertRaises(CI.Refusal):
            CI.validate_jobs("ci.yml", jobs, self.source, self.run, self.attempt)

    def test_active_aggregate_is_allowed_but_cannot_qualify_a_release(self):
        jobs = self.jobs()
        jobs[-1].update(status="in_progress", conclusion=None)
        CI.validate_jobs("ci.yml", jobs, self.source, self.run, self.attempt)
        with self.assertRaises(CI.Refusal):
            CI.validate_jobs("ci.yml", jobs, self.source, self.run, self.attempt, release=True)

    def test_failed_aggregate_and_unlisted_native_jobs_are_not_accepted(self):
        jobs = self.jobs()
        jobs[-1]["conclusion"] = "failure"
        with self.assertRaises(CI.Refusal):
            CI.validate_jobs("ci.yml", jobs, self.source, self.run, self.attempt)
        jobs = self.jobs()
        jobs.append({**jobs[0], "id": 99, "name": "New native job"})
        with self.assertRaises(CI.Refusal):
            CI.validate_jobs("ci.yml", jobs, self.source, self.run, self.attempt)

    def test_incomplete_or_inconsistent_job_pagination_is_refused(self):
        cases = [{"total_count": True, "jobs": []}, {"total_count": 1, "jobs": []},
                 {"total_count": 257, "jobs": []}, {"total_count": 1, "jobs": [{}, {}]}]
        for value in cases:
            with self.subTest(response=value), self.assertRaises(CI.Refusal):
                CI.job_rows(lambda endpoint: value, "example/jobs")
        self.assertEqual(CI.job_rows(lambda endpoint: {"total_count": 1, "jobs": [{"id": 1}]}, "jobs"), [{"id": 1}])

    def test_documentation_needs_and_actual_jobs_must_both_show_native_checks_skipped(self):
        report = {"profile": "documentation", "source_sha": "b" * 40, "head_sha": self.source}
        needs = {name: {"result": "skipped"} for name in CI.NATIVE_NEEDS["ci.yml"]}
        needs["scope"] = {"result": "success", "outputs": {"profile": "documentation", "source_sha": report["source_sha"]}}
        jobs = self.jobs()
        for job in jobs:
            if job["name"] in CI.NATIVE_JOBS["ci.yml"]:
                job["conclusion"] = "skipped"
        CI.validate_gate("ci.yml", report, needs, jobs, self.run, self.attempt)
        for mutation in [lambda n, j: n["verify"].update(result="success"),
                         lambda n, j: n["scope"].update(result="failure"),
                         lambda n, j: n["scope"]["outputs"].update(profile="full"),
                         lambda n, j: n.pop("core-tests"),
                         lambda n, j: j[0].update(conclusion="success")]:
            n, j = copy.deepcopy(needs), copy.deepcopy(jobs)
            mutation(n, j)
            with self.assertRaises(CI.Refusal):
                CI.validate_gate("ci.yml", report, n, j, self.run, self.attempt)


if __name__ == "__main__":
    unittest.main()
