"""Native coverage and complete decoding remain distinct in maintainer proofs."""
import copy
import importlib.util
from pathlib import Path
import unittest

SPEC = importlib.util.spec_from_file_location("media_evidence", Path(__file__).resolve().parents[1] / "media_evidence.py")
module = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(module)


class CoverageTests(unittest.TestCase):
    def native(self):
        return {"schema_version": 2, "required_depth": "native", "scope": "comparison", "passed": True,
                "checks": {"stream_metadata": "pass", "packet_payloads_equal": True, "packet_timing_equal": True},
                "decode_evidence": {"status": "not-checked", "reason": "not-requested"}, "license_playback_verified": False}

    def test_unrequested_decode_is_retained_and_never_qualifies_as_decoded(self):
        value = self.native()
        self.assertEqual(module.v2_proof(value)["decode_evidence"], value["decode_evidence"])
        self.assertIsNone(module.v2_proof(value, require_decoded=True))
        value["required_depth"] = "decoded"
        self.assertIsNone(module.v2_proof(value))
        value["decode_evidence"] = {"status": "content-compared", "equal": True}
        self.assertIsNotNone(module.v2_proof(value, require_decoded=True))
        value["scope"] = "source-admission"
        self.assertIsNone(module.v2_proof(value, scope="source-admission", require_decoded=True))
        value["decode_evidence"] = {"status": "source-decoded"}
        self.assertIsNotNone(module.v2_proof(value, scope="source-admission", require_decoded=True))

    def test_false_unknown_and_obsolete_claims_fail_closed(self):
        for change in [lambda v: v.update(passed=False), lambda v: v.update(failure="child-check-failed"),
                       lambda v: v.update(media_verified=True), lambda v: v["checks"].update(decoded_content_equal=True),
                       lambda v: v["checks"].update(packet_payloads_equal=1),
                       lambda v: v.update(decode_evidence={"status": "content-compared", "equal": False}),
                       lambda v: v.update(decode_evidence={"status": "source-decoded"}),
                       lambda v: v.update(required_depth="unknown")]:
            value = copy.deepcopy(self.native())
            change(value)
            self.assertIsNone(module.v2_proof(value))


class ConsumerTests(unittest.TestCase):
    native = CoverageTests.native

    def load(self, name):
        spec = importlib.util.spec_from_file_location(name, Path(__file__).resolve().parents[1] / (name + ".py"))
        result = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(result)
        return result

    def decoded(self):
        value = self.native()
        value.update(required_depth="decoded", decode_evidence={"status": "content-compared", "equal": True},
                     native_manifest={"schema_version": 1, "scope": "manifest-only", "profile": "dash-static-manifest",
                                      "errors": 0, "warnings": 0, "truncated": False, "incomplete": False, "document_count": 1})
        return value

    def test_archive_container_and_release_require_actual_decoded_evidence(self):
        for name, function, error in [("qualify_cli_archive", "verification", "Refusal"),
                                      ("qualify_container", "require_verification", "Refusal"),
                                      ("prepare_release", "proof", "Failure")]:
            consumer = self.load(name)
            verifier = getattr(consumer, function)
            verifier(self.decoded())
            with self.assertRaises(getattr(consumer, error)):
                verifier(self.native())
            false = self.decoded()
            false["decode_evidence"]["equal"] = False
            with self.assertRaises(getattr(consumer, error)):
                verifier(false)

    def test_measurement_preserves_unrequested_decode_instead_of_inventing_a_pass(self):
        measure = self.load("measure_conversion")
        proof = measure.validate_proof(self.native())
        self.assertEqual(proof["decode_evidence"], {"status": "not-checked", "reason": "not-requested"})
        self.assertNotIn("media_verified", proof)
        self.assertNotIn("decoded_content_equal", proof["checks"])
        with self.assertRaises(measure.Failure):
            measure.validate_proof(self.native(), require_decoded=True)
        false = self.decoded()
        false["decode_evidence"]["equal"] = False
        with self.assertRaises(measure.Failure):
            measure.validate_proof(false)

    def test_archive_source_and_plan_coverage_remain_distinct_from_comparison(self):
        archive = self.load("qualify_cli_archive")
        source = self.decoded()
        source.update(scope="source-admission", decode_evidence={"status": "source-decoded"})
        self.assertTrue(archive.source_verification({"schema_version": 2, "verification": source}))
        self.assertFalse(archive.source_verification({"schema_version": 2, "verification": self.decoded()}))
        self.assertFalse(archive.plan_depth({"schema_version": 3, "required_depth": "native"}))
        self.assertTrue(archive.plan_depth({"schema_version": 3, "required_depth": "decoded"}))
        self.assertTrue(archive.plan_depth({"schema_version": 1}))
        self.assertFalse(archive.plan_depth({"schema_version": 1, "required_depth": None}))

    def test_public_media_regression_retains_native_and_decode_requirements(self):
        path = Path(__file__).resolve().parents[2] / "crates/drmr/tests/fixtures/bbb/regression.py"
        spec = importlib.util.spec_from_file_location("bbb_regression", path)
        regression = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(regression)
        regression.require_media_verification(self.decoded())
        timing = self.decoded()
        timing["checks"]["packet_timing_equal"] = False
        failed_decode = self.decoded()
        failed_decode["decode_evidence"]["equal"] = False
        for value in [self.native(), timing, failed_decode]:
            with self.assertRaises(AssertionError):
                regression.require_media_verification(value)
        source = self.decoded()
        source.update(scope="source-admission", decode_evidence={"status": "source-decoded"})
        regression.require_media_verification(source, scope="source-admission", manifest=False)
        with self.assertRaises(AssertionError):
            regression.require_media_verification(source)


if __name__ == "__main__":
    unittest.main()
