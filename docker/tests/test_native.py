import hashlib
import importlib.util
from pathlib import Path
import tempfile
import unittest

spec = importlib.util.spec_from_file_location("native", Path(__file__).parents[1] / "native.py")
native = importlib.util.module_from_spec(spec)
spec.loader.exec_module(native)

class SourceEvidence(unittest.TestCase):
    def test_raw_source_checksum_rejects_line_ending_changes(self):
        with tempfile.TemporaryDirectory() as temp:
            path = Path(temp) / "source"
            path.write_bytes(b"publisher\r\n")
            with self.assertRaisesRegex(ValueError, "source-checksum-mismatch"):
                native.verify_file(path, hashlib.sha256(b"publisher\n").hexdigest())

    def test_notice_filename_variants_preserve_raw_bytes(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp) / "source"
            root.mkdir()
            bodies = {"LICENSE.md": b"terms\r\n", "LICENSE.MIT": b"copyright", "LICENSES/Apache-2.0.txt": b"full terms", "AUTHORS": b"publisher"}
            for name, body in bodies.items():
                path = root / name
                path.parent.mkdir(exist_ok=True)
                path.write_bytes(body)
            output = Path(temp) / "notices"
            records = native.collect_notices(root, output)
            self.assertEqual({r["path"] for r in records}, set(bodies))
            for name, body in bodies.items():
                self.assertEqual((output / name).read_bytes(), body)

    def test_component_without_license_fails_closed(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp) / "source"
            root.mkdir()
            (root / "README").write_text("not license evidence")
            with self.assertRaisesRegex(ValueError, "component-license-missing"):
                native.collect_notices(root, Path(temp) / "notices")

if __name__ == "__main__":
    unittest.main()
