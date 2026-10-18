"""Regression checks for cooperative benchmark termination; no media or keys are fetched."""
import importlib.util
import copy
import json
import os
from pathlib import Path
import signal
import subprocess
import sys
import tempfile
import time
import unittest
from unittest import mock

SPEC = importlib.util.spec_from_file_location(
    "measure_conversion", Path(__file__).resolve().parents[1] / "measure_conversion.py"
)
measure = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(measure)


class Conditions(unittest.TestCase):
    def test_warming_is_an_explicit_condition_and_never_a_cold_cache_claim(self):
        arguments = ["--drmr", "drmr", "--input", "input.mp4", "--to", "clear", "--format", "mp4"]
        self.assertEqual(measure.parse_args(arguments).condition, "uncontrolled")
        self.assertEqual(measure.parse_args(arguments + ["--condition", "warmed-by-equivalent-verified-run"]).condition,
                         "warmed-by-equivalent-verified-run")
        with self.assertRaises(measure.Failure):
            measure.parse_args(arguments + ["--condition", "cold"])


def metrics():
    return {"schema_version": 1, "status": "observed",
            "phase_semantics": "elapsed-scope-sums-overlap-not-command-wall-or-cpu",
            "tool_semantics": "generic-call-duration-sums-include-admission-capture-and-cleanup",
            "coverage": "explicit-generic-scopes-only-not-all-rust-or-filesystem-work",
            "phases": [{"phase": name, "status": "observed-scopes", "scopes": 1,
                        "scope_elapsed_sum_seconds": 0.1} for name in (
                            "input-preparation", "inspection", "normalization", "packaging", "verification", "publication")],
            "tools": [{"tool": name, "status": "observed-calls", "calls": 1,
                       "unsuccessful_calls": 0, "reported_duration_sum_seconds": 0.1}
                      for name in ("ffmpeg", "ffprobe", "packager")],
            "unclassified_tool_calls": 0, "invalid_tool_records": 0,
            "workspace": {"status": "observed-checkpoints", "scope": "selected-quiescent-stage-sets-not-all-live-command-stages",
                          "checkpoints": 1, "incomplete_checkpoints": 0,
                          "checkpoint_regular_file_bytes_max": 123, "reason": None},
            "io": {"status": "unmeasured", "reason": "total-process-io-not-collected"}}


class MeasurementBoundary(unittest.TestCase):
    def test_raw_duplicate_members_are_refused_before_values_can_be_collapsed(self):
        value = {"schema_version": 1, "measurements": metrics(), "verification": {
            "media_verified": True}}
        encoded = json.dumps(value)
        cases = (
            encoded.replace('"calls": 1', '"calls": 0, "calls": 1', 1),
            encoded.replace('"status": "observed"', '"status": "partial", "status": "observed"', 1),
            encoded.replace('"media_verified": true', '"media_verified": false, "media_verified": true', 1),
        )
        for raw in cases:
            with self.subTest(raw=raw), mock.patch.object(measure, "sample_processes", return_value=None):
                result, payload = measure.measured_run(
                    [sys.executable, "-c", "import sys; sys.stdout.write(" + repr(raw) + ")"],
                    dict(os.environ), 2, 0.1)
                self.assertEqual(result["status"], "failed")
                self.assertEqual(result["error"], "measurement-invalid")
                self.assertIsNone(payload)

    def test_only_exact_labels_finite_numbers_and_known_shapes_are_retained(self):
        value = metrics()
        self.assertEqual(measure.validate_measurements(value), value)
        for kind in ("raw", "phase", "large", "nan", "bool", "duplicate"):
            bad = copy.deepcopy(value)
            if kind == "raw":
                bad["argv"] = ["operator-secret"]
            elif kind == "phase":
                bad["phases"][0]["phase"] = "operator-secret"
            elif kind == "large":
                bad["phases"][0]["scope_elapsed_sum_seconds"] = 10**400
            elif kind == "nan":
                bad["tools"][0]["reported_duration_sum_seconds"] = float("nan")
            elif kind == "bool":
                bad["tools"][0]["calls"] = True
            elif kind == "duplicate":
                bad["phases"][1] = copy.deepcopy(bad["phases"][0])
            with self.subTest(kind=kind), self.assertRaises(measure.Failure) as failed:
                measure.validate_measurements(bad)
            self.assertEqual(str(failed.exception), "measurement-invalid")

    def benchmark(self, directory, bad_warmup=False):
        binary, source = Path(directory) / "drmr", Path(directory) / "source.mp4"
        binary.write_bytes(b"public-test-binary")
        source.write_bytes(b"public-test-source")
        args = measure.parse_args(["--drmr", str(binary), "--input", str(source), "--to", "clear", "--format", "mp4",
                                   "--repeats", "1", "--condition", "warmed-by-equivalent-verified-run"])
        proof = {"schema_version": 1, "passed": True, "media_verified": True, "license_playback_verified": False,
                 "checks": {"stream_metadata": "pass", **{name: True for name in measure.CHECKS}}}
        calls, outputs = [], []
        def run(argv, *_):
            self.assertIn("--measure", argv)
            calls.append(argv[1])
            if argv[1] == "convert":
                path = Path(argv[argv.index("--output") + 1])
                path.write_bytes(b"verified-result")
                outputs.append(path)
                payload = {"schema_version": 1, "verification": dict(proof), "measurements": metrics()}
                if bad_warmup:
                    payload["verification"]["media_verified"] = False
            else:
                payload = {**proof, "measurements": metrics()}
            return {"status": "completed", "elapsed_seconds": 0.1, "memory": {"status": "unmeasured"}}, payload
        report = {"runs": []}
        with mock.patch.object(measure, "measured_run", side_effect=run), mock.patch.object(
                measure, "query", side_effect=lambda _binary, arguments, _env:
                "drmr 0.1.0" if arguments == ["--version"] else "--max-workers --measure"):
            if bad_warmup:
                with self.assertRaises(measure.Failure) as failed:
                    measure.benchmark(args, report)
                self.assertEqual(str(failed.exception), "verification-proof-invalid")
            else:
                measure.benchmark(args, report)
        return report, calls, outputs

    def test_every_measured_case_requires_a_fresh_equivalent_verified_warmup(self):
        with tempfile.TemporaryDirectory() as directory:
            report, calls, outputs = self.benchmark(directory)
        self.assertEqual(calls, ["convert", "verify"] * 4)
        self.assertEqual(len(outputs), len(set(outputs)))
        self.assertEqual(len(report["warmups"]), 2)
        self.assertEqual(len(report["runs"]), 2)
        self.assertTrue(all(row["status"] == "verified" for row in report["warmups"]))
        self.assertEqual(report["condition"]["physical_cold"], "not-measured")
        self.assertEqual(measure.summarize(report["runs"])["auto"]["verified_runs"], 1)

    def test_failed_warmup_prevents_all_measured_cases(self):
        with tempfile.TemporaryDirectory() as directory:
            report, calls, _ = self.benchmark(directory, bad_warmup=True)
        self.assertEqual(calls, ["convert"])
        self.assertEqual(report["runs"], [])
        self.assertEqual(report["warmups"][0]["status"], "failed")


@unittest.skipUnless(os.name == "posix", "POSIX termination-event contract")
class TerminationTests(unittest.TestCase):
    def start(self, directory, body):
        ready = Path(directory) / "ready"
        process = subprocess.Popen(
            [sys.executable, "-c", body, str(ready)], start_new_session=True,
            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
        )
        deadline = time.monotonic() + 5
        while not ready.is_file():
            self.assertIsNone(process.poll())
            self.assertLess(time.monotonic(), deadline)
            time.sleep(0.005)
        return process

    def test_cooperative_signal_is_delivered_before_forcing_exit(self):
        with tempfile.TemporaryDirectory() as directory:
            child = self.start(directory,
                "import pathlib,signal,sys,time; "
                "signal.signal(signal.SIGTERM,lambda *_:sys.exit(130)); "
                "pathlib.Path(sys.argv[1]).write_text('ready'); time.sleep(30)")
            try:
                result = measure.terminate(child, grace=1)
                self.assertEqual(result, {"method": "cooperative-signal", "cleanup": "cooperative-exit"})
                self.assertEqual(child.returncode, 130)
            finally:
                if child.poll() is None:
                    os.killpg(child.pid, signal.SIGKILL)
                    child.wait()

    def test_forced_fallback_is_bounded_and_never_claims_cleanup(self):
        with tempfile.TemporaryDirectory() as directory:
            child = self.start(directory,
                "import pathlib,signal,sys,time; signal.signal(signal.SIGTERM,signal.SIG_IGN); "
                "pathlib.Path(sys.argv[1]).write_text('ready'); time.sleep(30)")
            started = time.monotonic()
            try:
                result = measure.terminate(child, grace=0.1)
                self.assertEqual(result, {"method": "force", "cleanup": "unconfirmed"})
                self.assertIsNotNone(child.returncode)
                self.assertLess(time.monotonic() - started, 5)
            finally:
                if child.poll() is None:
                    os.killpg(child.pid, signal.SIGKILL)
                    child.wait()


if __name__ == "__main__":
    unittest.main()
