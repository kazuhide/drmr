"""Fuzz evidence comes from matching real CLI counters, never raw diagnostics."""
import contextlib
import copy
import importlib.util
import io
import json
import os
from pathlib import Path
import signal
import subprocess
import sys
import tempfile
import threading
import time
import unittest
from unittest.mock import Mock, patch

sys.path.insert(0, str(Path(__file__).resolve().parent))
import fuzz_reports

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "tools"))
SPEC = importlib.util.spec_from_file_location("fuzz_qualification", ROOT / "tools/fuzz_qualification.py")
FUZZ = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(FUZZ)

# Counter shapes captured from real single-process 60-second campaigns.
def transcript(target="hevc"):
    accepted, executions, coverage = (4, 276213, 1810) if target == "hevc" else (18, 355787, 999)
    return (f"DRMR_FUZZ_CORPUS target={target} accepted={accepted} refused=2\n"
            f"#{executions}\tDONE   cov: {coverage} ft: 4896 corp: 335/2020Kb lim: 25421 exec/s: 4528 rss: 277Mb\n"
            f"Done {executions} runs in 61 second(s)\n"
            f"stat::number_of_executed_units: {executions}\n"
            "stat::peak_rss_mb: 297\n"
            "stat::average_exec_per_sec: 4528\n").encode()


class FuzzExecution(unittest.TestCase):
    def test_genuine_counter_shapes_match_wall_time_and_seed_admission(self):
        for target, _, _ in FUZZ.CONTRACT.TARGETS:
            record = FUZZ.statistics(transcript(target), target, 61360)
            self.assertGreater(record["executions"], 100000)
            self.assertGreater(record["coverage"], 0)
            self.assertEqual(record["fuzzer_seconds"], 61)
            self.assertNotIn("corp:", json.dumps(record))

    def test_missing_duplicate_contradictory_or_fake_time_stats_are_refused(self):
        raw = transcript()
        changes = [raw.replace(b"accepted=4", b"accepted=3"), raw.replace(b"refused=2", b"refused=0"),
                   raw.replace(b"cov: 1810", b"cov: 0"), raw.replace(b"#276213", b"#276214"),
                   raw.replace(b"Done 276213", b"Done 0"),
                   raw.replace(b"executed_units: 276213", b"executed_units: 1"),
                   raw.replace(b"in 61 second(s)", b"in 1 second(s)"),
                   raw.replace(b"peak_rss_mb: 297", b"peak_rss_mb: 513"),
                   raw + b"stat::number_of_executed_units: 276213\n", raw + raw,
                   raw.split(b"Done ")[0], b"PRIVATE VALUE"]
        for index, data in enumerate(changes):
            with self.subTest(index=index), self.assertRaises(FUZZ.CONTRACT.Failure):
                FUZZ.statistics(data, "hevc", 61360)
        for elapsed in (100, 59000, 90001, 80000, True):
            with self.subTest(elapsed=elapsed), self.assertRaises(FUZZ.CONTRACT.Failure):
                FUZZ.statistics(raw, "hevc", elapsed)

    def test_raw_private_diagnostics_are_not_projected_into_public_stats(self):
        record = FUZZ.statistics(b"PRIVATE_VALUE /private/path https://invalid.test/key\n" + transcript(), "hevc", 61360)
        self.assertNotIn("PRIVATE_VALUE", json.dumps(record))
        self.assertNotIn("/private/path", json.dumps(record))
        self.assertNotIn("invalid.test", json.dumps(record))

    def test_single_process_arguments_keep_mutations_and_crashes_outside_source(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            binary = root / "hevc"
            binary.write_bytes(b"Synthetic executable bytes")
            corpus = root / "corpus"
            corpus.mkdir()
            runner = Mock()
            runner.workspace = root
            runner.run.return_value = transcript()
            with patch.object(FUZZ.time, "monotonic", side_effect=[0, 61.36]):
                record = FUZZ.run_target(runner, binary, "hevc", corpus)
            arguments = runner.run.call_args.args[0]
            self.assertEqual(arguments[:4], ["/bin/sh", "-c", 'exec "$@" 2>&1', "drmr-fuzz"])
            self.assertIn("-max_total_time=60", arguments)
            self.assertIn("-max_len=65536", arguments)
            self.assertIn("-rss_limit_mb=512", arguments)
            self.assertFalse(any(item.startswith(("-jobs=", "-workers=")) for item in arguments))
            self.assertEqual(record["exit_code"], 0)

    def test_reusable_workflow_keeps_fuzz_outside_normal_matrix_and_gates_assembly(self):
        workflow = (ROOT / ".github/workflows/codec-fuzz.yml").read_text()
        release = (ROOT / ".github/workflows/release.yml").read_text()
        self.assertIn("workflow_call:", workflow)
        self.assertIn("workflow_dispatch:", workflow)
        self.assertIn("schedule:", workflow)
        self.assertNotIn("pull_request:", workflow)
        self.assertIn("nightly-2026-11-01", workflow)
        self.assertIn("cargo-fuzz --version 0.13.2 --locked", workflow)
        self.assertIn("cargo-audit --version 0.22.2 --locked", workflow)
        self.assertIn("CARGO_NET_OFFLINE: 'true'", workflow)
        self.assertIn("--sanitizer address", workflow)
        self.assertIn("--debug-assertions", workflow)
        self.assertIn("CARGO_PROFILE_RELEASE_DEBUG_ASSERTIONS: 'true'", workflow)
        self.assertIn("CARGO_PROFILE_RELEASE_OVERFLOW_CHECKS: 'true'", workflow)
        self.assertNotIn("fuzz build -O", workflow)
        self.assertIn("uses: ./.github/workflows/codec-fuzz.yml", release)
        self.assertIn("needs: [roundtrip, fuzz]", release)
        self.assertIn('--fuzz-report "$RUNNER_TEMP/fuzz/cli-codec-fuzz.json"', release)

    def test_changed_binary_blocks_result(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            binary = root / "hevc"
            binary.write_bytes(b"Original executable")
            runner = Mock(workspace=root)
            def changed(*_, **__):
                binary.write_bytes(b"Replaced executable")
                return transcript()
            runner.run.side_effect = changed
            with patch.object(FUZZ.time, "monotonic", side_effect=[0, 61.36]), self.assertRaises(FUZZ.CONTRACT.Failure):
                FUZZ.run_target(runner, binary, "hevc", root)

    def test_metadata_admits_only_the_locked_core_feature_and_same_vendor_paths(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            files = {"Cargo.toml": '[workspace.package]\nversion = "1.1.0"\n',
                     "fuzz/Cargo.toml": "fuzz", "crates/drmr-core/Cargo.toml": "core",
                     "vendor/private-fs/Cargo.toml": "private"}
            for name, content in files.items():
                path = root / name
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_text(content)
            packages = [{"id": name, "name": name, "version": version, "source": source,
                         "manifest_path": str(root / path)} for name, version, source, path in (
                            ("drmr-codec-fuzz", "0.0.0", None, "fuzz/Cargo.toml"),
                            ("drmr-core", "1.1.0", None, "crates/drmr-core/Cargo.toml"),
                            ("private-fs", "0.2.1", None, "vendor/private-fs/Cargo.toml"),
                            ("libfuzzer-sys", "0.4.13", "registry+https://github.com/rust-lang/crates.io-index", "registry/Cargo.toml"))]
            nodes = [{"id": p["id"], "features": ["fuzzing"] if p["id"] == "drmr-core" else [],
                      "deps": [{"pkg": key, "dep_kinds": [{"kind": None}]} for key in
                               ({"drmr-codec-fuzz": ["drmr-core", "libfuzzer-sys"], "drmr-core": ["private-fs"]}.get(p["id"], []))]}
                     for p in packages]
            metadata = {"packages": packages, "resolve": {"nodes": nodes}, "workspace_root": str(root / "fuzz")}
            source = Mock(path=root, paths=set(files))
            source.read.side_effect = lambda name: files[name].encode()
            changes = [lambda value: None, lambda value: value["packages"][1].update(version="1.0.3"),
                       lambda value: value["packages"][1].update(source="registry+wrong"),
                       lambda value: value["packages"][2].update(source="registry+wrong"),
                       lambda value: value["packages"][3].update(version="0.4.12"),
                       lambda value: value["resolve"]["nodes"][1].update(features=[]),
                       lambda value: value.update(workspace_root="/outside")]
            for index, change in enumerate(changes):
                value = copy.deepcopy(metadata)
                change(value)
                runner = Mock()
                runner.run.side_effect = [b"rustc 1.101.0-nightly (test)\n", b"cargo-fuzz 0.13.2\n", b"cargo-audit 0.22.2\n", b"Verified 26 public codec seeds\n"]
                runner.json.return_value = value
                with self.subTest(index=index), patch.object(FUZZ, "audit", return_value={"checked": True}):
                    if index == 0:
                        self.assertEqual(FUZZ.preflight(runner, source, {}, root, root), {"checked": True})
                    else:
                        with self.assertRaises(FUZZ.CONTRACT.Failure):
                            FUZZ.preflight(runner, source, {}, root, root)

    def test_audit_checks_both_lock_identities_and_refuses_any_fuzz_graph_finding(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = Mock(path=root)
            source.read.side_effect = lambda name: b"original lock" if name.endswith("Cargo.lock") else b"{}"
            for finding in ([], [{"advisory": "unresolved"}]):
                auditor = Mock()
                auditor.normalized_private_fs_lock.return_value = b"registry identity lock"
                auditor.scoped_findings.return_value = finding
                runner = Mock(workspace=root, cancel_event=threading.Event())
                runner.run.side_effect = [b"https://github.com/RustSec/advisory-db.git\n", b"a" * 40 + b"\n", b"", b"a" * 40 + b"\n", b""]
                with patch.object(FUZZ.CONTRACT, "module", return_value=auditor):
                    if finding:
                        with self.assertRaises(FUZZ.CONTRACT.Failure):
                            FUZZ.audit(runner, source, {"lock_sha256": "b" * 64}, [], root, root)
                    else:
                        result = FUZZ.audit(runner, source, {"lock_sha256": "b" * 64}, [], root, root)
                        self.assertEqual(result["findings"], 0)
                        self.assertEqual(auditor.audit_input.call_count, 2)
                        self.assertEqual([call.args[3] for call in auditor.audit_input.call_args_list], [b"original lock", b"registry identity lock"])
                        auditor.verify_private_fs.assert_called_once_with(root)

    @unittest.skipUnless(os.name == "posix", "Linux release cancellation handler")
    def test_repeated_signals_remain_cooperative_until_cleanup_finishes(self):
        previous = {kind: signal.getsignal(kind) for kind in (signal.SIGINT, signal.SIGTERM)}
        with FUZZ.cancellation() as cancelled:
            for kind in (signal.SIGTERM, signal.SIGINT, signal.SIGTERM):
                signal.raise_signal(kind)
            self.assertTrue(cancelled.is_set())
        self.assertEqual({kind: signal.getsignal(kind) for kind in previous}, previous)

    @unittest.skipUnless(os.name == "posix", "Linux fuzz child cancellation")
    def test_signal_cancels_and_reaps_the_actual_single_process_child(self):
        process = []
        actual_popen = FUZZ.SUPPORT.subprocess.Popen
        with FUZZ.cancellation() as cancelled:
            workspace = FUZZ.SUPPORT.Workspace()
            runner = FUZZ.SUPPORT.Runner(workspace.path, cancel_event=cancelled)
            ready = workspace.path / "ready"
            def spawned(*args, **kwargs):
                child = actual_popen(*args, **kwargs)
                process.append(child)
                return child
            def stop():
                deadline = time.monotonic() + 5
                while not ready.exists() and time.monotonic() < deadline:
                    time.sleep(0.01)
                if ready.exists():
                    os.kill(os.getpid(), signal.SIGTERM)
                    time.sleep(0.02)
                    os.kill(os.getpid(), signal.SIGTERM)
            thread = threading.Thread(target=stop)
            thread.start()
            try:
                with patch.object(FUZZ.SUPPORT.subprocess, "Popen", side_effect=spawned), \
                     self.assertRaisesRegex(FUZZ.SUPPORT.Refusal, "^command-cancelled$"):
                    runner.run([sys.executable, "-c", "from pathlib import Path; import time; Path('ready').write_text('ready'); time.sleep(30)"],
                               "synthetic-fuzz-child", timeout=10)
            finally:
                thread.join(timeout=6)
                workspace.close()
            self.assertFalse(thread.is_alive())
            self.assertEqual(len(process), 1)
            self.assertIsNotNone(process[0].returncode)
            self.assertFalse(workspace.path.exists())

    def test_failure_and_interrupt_close_the_real_owned_workspace(self):
        for error in (RuntimeError("PRIVATE_VALUE"), KeyboardInterrupt()):
            created = []
            factory = FUZZ.SUPPORT.Workspace
            def workspace():
                result = factory()
                created.append(result.path)
                return result
            source = Mock(path=ROOT, head="a" * 40, tree="b" * 40)
            source.read.return_value = b'[workspace.package]\nversion = "1.1.0"\n'
            with patch.object(FUZZ.SUPPORT, "require_native"), patch.object(FUZZ.CONTRACT.VERIFY, "Source", return_value=source), \
                 patch.object(FUZZ.CONTRACT, "identity", return_value={}), patch.object(FUZZ, "check_source"), \
                 patch.object(FUZZ.SUPPORT, "Workspace", side_effect=workspace), \
                 patch.object(FUZZ, "preflight", side_effect=error), self.assertRaises(type(error)):
                FUZZ.qualify(ROOT, ROOT, ROOT, ROOT)
            self.assertEqual(len(created), 1)
            self.assertFalse(created[0].exists())

    def test_full_orchestration_binds_actual_source_copies_seeds_and_cleans_before_success(self):
        with tempfile.TemporaryDirectory() as directory:
            home = Path(directory).resolve()
            root, binaries = home / "source", home / "bin"
            root.mkdir()
            binaries.mkdir()
            files = fuzz_reports.files(FUZZ.CONTRACT.CODECS)
            files["Cargo.toml"] = '[workspace.package]\nversion = "1.1.0"\n'
            for name, data in files.items():
                path = root / name
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_text(data)
            def git(*args):
                return subprocess.check_output(["git", *args], cwd=root, stderr=subprocess.PIPE)
            git("init", "--quiet")
            git("add", ".")
            git("-c", "user.name=Fuzz Test", "-c", "user.email=fuzz@example.invalid", "commit", "--quiet", "-m", "Synthetic fixture")
            for target, _, _ in FUZZ.CONTRACT.TARGETS:
                (binaries / target).write_bytes(target.encode())
            source = FUZZ.CONTRACT.VERIFY.Source(root)
            identity = FUZZ.CONTRACT.identity(source)
            proof = fuzz_reports.complete(FUZZ.CONTRACT, identity, source.head, source.tree, "1.1.0")["audit"]
            corpora = []
            def run(arguments, *_, **__):
                corpus = Path(arguments[5])
                corpora.append(corpus)
                target = Path(arguments[4]).name
                self.assertEqual(len(list(corpus.iterdir())), 6 if target == "hevc" else 20)
                for seed in FUZZ.CONTRACT.seeds(source):
                    if seed["path"].startswith(target + "/"):
                        self.assertEqual((corpus / seed["path"].split("/")[1]).read_bytes(), source.read("fuzz/corpus/" + seed["path"]))
                return transcript(target)
            with patch.object(FUZZ.SUPPORT, "require_native"), patch.object(FUZZ, "preflight", return_value=proof), \
                 patch.object(FUZZ.SUPPORT.Runner, "run", side_effect=run), \
                 patch.object(FUZZ.time, "monotonic", side_effect=[0, 61.36, 61.36, 122.72]):
                report = FUZZ.qualify(root, binaries, home, home)
            self.assertIs(FUZZ.CONTRACT.validate(report, source.head, source.tree, "1.1.0", identity), report)
            self.assertTrue(all(not path.exists() for path in corpora))
            self.assertEqual(git("status", "--porcelain"), b"")
            self.assertNotIn(str(home), json.dumps(report))
            (root / "fuzz/Cargo.lock").write_text("Changed source lock")
            with self.assertRaises(FUZZ.CONTRACT.Failure):
                FUZZ.check_source(source, identity)

    def test_main_never_prints_exception_or_operator_values(self):
        for error in (RuntimeError("PRIVATE_VALUE"), FUZZ.CONTRACT.Failure("PRIVATE_VALUE")):
            output = io.StringIO()
            with patch.object(FUZZ, "qualify", side_effect=error), contextlib.redirect_stdout(output):
                code = FUZZ.main(["--binary-dir", "/private/bin", "--database", "/private/db", "--cargo-audit", "/private/audit"])
            self.assertEqual(code, 1)
            self.assertEqual(json.loads(output.getvalue()), {"schema_version": 1, "kind": "source-codec-fuzz",
                "success": False, "error": "fuzz-qualification-failed"})

    def test_cancellation_deadline_and_unconfirmed_cleanup_cannot_publish_success(self):
        for failure, expected in (("command-cancelled", "fuzz-cancelled"), ("command-deadline", "fuzz-timeout"),
                                  ("command-cleanup-unconfirmed", "fuzz-cleanup-unconfirmed"),
                                  ("owned-cleanup-unconfirmed", "fuzz-cleanup-unconfirmed")):
            output = io.StringIO()
            with patch.object(FUZZ, "qualify", side_effect=FUZZ.SUPPORT.Refusal(failure)), contextlib.redirect_stdout(output):
                code = FUZZ.main(["--binary-dir", "binary", "--database", "database", "--cargo-audit", "audit"])
            self.assertEqual(code, 1)
            self.assertEqual(json.loads(output.getvalue())["error"], expected)


if __name__ == "__main__":
    unittest.main()
