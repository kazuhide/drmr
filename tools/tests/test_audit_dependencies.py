"""Scope advisory findings to the exact target and feature graph without ignores."""
import importlib.util
from pathlib import Path
import unittest
import sys
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

SPEC = importlib.util.spec_from_file_location("audit_dependencies", Path(__file__).resolve().parents[1] / "audit_dependencies.py")
AUDIT = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(AUDIT)


class AdvisoryScope(unittest.TestCase):
    def test_cli_excludes_optional_gui_but_desktop_retains_both_warning_classes(self):
        report = {"vulnerabilities": {"list": []}, "warnings": {
            "unsound": [{"package": {"name": "glib", "version": "0.18.5"}, "advisory": {"id": "RUSTSEC-2024-0429", "title": "Unsound iterator"}}],
            "unmaintained": [{"package": {"name": "proc-macro-error", "version": "1.0.4"}, "advisory": {"id": "RUSTSEC-2024-0370", "title": "Unmaintained"}}]}}
        cli = [{"name": "drmr", "version": "0.1.0"}]
        desktop = cli + [{"name": "glib", "version": "0.18.5"}, {"name": "proc-macro-error", "version": "1.0.4"}]
        self.assertEqual(AUDIT.scoped_findings(report, cli), [])
        rows = AUDIT.scoped_findings(report, desktop)
        self.assertEqual({row["classification"] for row in rows}, {"unsound", "unmaintained"})
        self.assertEqual(len(rows), 2)

    def test_new_vulnerability_is_not_suppressed_by_known_warning_logic(self):
        report = {"vulnerabilities": {"list": [{"package": {"name": "runtime", "version": "1.0.0"}, "advisory": {"id": "RUSTSEC-2099-0001", "title": "Synthetic"}}]}, "warnings": {}}
        self.assertEqual(AUDIT.scoped_findings(report, [{"name": "runtime", "version": "1.0.0"}])[0]["classification"], "vulnerability")
        self.assertEqual(AUDIT.scoped_findings(report, [{"name": "runtime", "version": "2.0.0"}]), [])

    def test_malformed_or_missing_results_cannot_look_clean(self):
        for report in [{}, {"vulnerabilities": {}, "warnings": {}}, {"vulnerabilities": {"list": []}, "warnings": {"unknown": [{}]}}]:
            with self.subTest(report=report), self.assertRaises(AUDIT.Failure):
                AUDIT.scoped_findings(report, [])

    def test_normalization_changes_only_the_pinned_registry_identity(self):
        lock = b'[[package]]\nname = "glib"\nversion = "0.18.5"\ndependencies = []\n[[package]]\nname = "other"\nversion = "1"\n'
        changed = AUDIT.normalized_lock(lock, {"source_sha256": "a" * 64})
        self.assertEqual(changed.count(b'source = "registry+'), 1)
        self.assertEqual(changed.replace(b'\nsource = "registry+https://github.com/rust-lang/crates.io-index"\nchecksum = "' + b'a' * 64 + b'"', b'', 1), lock)
        for invalid in [lock.replace(b'0.18.5', b'0.18.4'), lock + lock, changed]:
            with self.assertRaises(AUDIT.Failure): AUDIT.normalized_lock(invalid, {"source_sha256": "a" * 64})

    def test_dispositions_bind_target_classification_and_full_dependency_ancestry(self):
        row = {"name": "proc-macro-error", "version": "1.0.4", "advisory": "RUSTSEC-2024-0370", "classification": "unmaintained", "advisory_sha256": "c",
               "dependency_ancestry": {"sha256": "a" * 64, "build_time_only": True, "direct_parents": ["glib-macros@0.18.5", "gtk3-macros@0.18.2"]}}
        entry = dict({k: row[k] for k in ("name", "version", "advisory", "classification", "advisory_sha256")}, kind="desktop", target="x86_64-unknown-linux-gnu", ancestry_sha256="a" * 64,
                     direct_parents=row["dependency_ancestry"]["direct_parents"], disposition="acknowledged-maintenance")
        policy = {"dispositions": [entry]}
        def disposition(candidate=row, target="x86_64-unknown-linux-gnu"):
            return AUDIT.apply_policy([candidate], "desktop", target, policy, {"advisory": "RUSTSEC-2024-0429"})[0]["disposition"]
        self.assertEqual(disposition(), "acknowledged-maintenance")
        self.assertEqual(disposition(target="x86_64-pc-windows-msvc"), "unresolved")
        self.assertEqual(disposition(dict(row, classification="vulnerability")), "unresolved")
        self.assertEqual(disposition(dict(row, dependency_ancestry={"sha256": "b" * 64, "build_time_only": True, "direct_parents": entry["direct_parents"]})), "unresolved")

    def test_backport_disposition_never_claims_native_regression_by_itself(self):
        row = {"name": "glib", "version": "0.18.5", "advisory": "RUSTSEC-2024-0429", "classification": "unsound", "advisory_sha256": "c", "dependency_ancestry": {"sha256": "a", "direct_parents": []}}
        policy = {"dispositions": [dict({k: row[k] for k in ("name", "version", "advisory", "classification", "advisory_sha256")}, kind="desktop", target="linux", ancestry_sha256="a", direct_parents=[], disposition="source-mitigated")]}
        self.assertEqual(AUDIT.apply_policy([row], "desktop", "linux", policy, {"advisory": "RUSTSEC-2024-0429"})[0]["disposition"], "source-mitigated-pending-native-regression")

    def test_resolved_feature_changes_invalidate_dependency_ancestry(self):
        packages = [{"id": "parent", "name": "macro", "version": "1", "targets": [{"kind": ["proc-macro"]}]},
                    {"id": "child", "name": "library", "version": "1", "targets": [{"kind": ["lib"]}]}]
        meta = {"resolve": {"nodes": [{"id": "parent", "features": [], "deps": [{"pkg": "child", "dep_kinds": [{"kind": None}]}]},
                                      {"id": "child", "features": [], "deps": []}]}}
        first = AUDIT.ancestry(meta, packages, "library", "1")
        self.assertTrue(first["build_time_only"])
        meta["resolve"]["nodes"][1]["features"] = ["additional-feature"]
        self.assertNotEqual(first["sha256"], AUDIT.ancestry(meta, packages, "library", "1")["sha256"])
