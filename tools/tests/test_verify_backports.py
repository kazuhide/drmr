"""A backport is accepted only with every original publisher byte accounted for."""
import importlib.util
from pathlib import Path
import shutil
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location("verify_backports", ROOT / "tools/verify_backports.py")
VERIFY = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(VERIFY)


class BackportSource(unittest.TestCase):
    def test_original_inventory_and_two_line_patch(self):
        result = VERIFY.verify(ROOT)
        self.assertEqual(result["files"], 121)
        self.assertEqual(result["version"], "0.18.5")

    def test_restoring_the_unsound_pointer_or_changing_unrelated_source_is_refused(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            shutil.copytree(ROOT / "vendor/glib", root / "vendor/glib")
            (root / "tools/backports").mkdir(parents=True)
            shutil.copyfile(ROOT / "tools/backports/glib-0.18.5.json", root / "tools/backports/glib-0.18.5.json")
            changed = root / "vendor/glib/src/variant_iter.rs"
            safe = changed.read_bytes()
            changed.write_bytes(safe.replace(b"let mut p:", b"let p:").replace(b"&mut p,", b"&p,"))
            with self.assertRaisesRegex(VERIFY.Failure, "backport-patched-source"):
                VERIFY.verify(root)
            changed.write_bytes(safe)
            untouched = root / "vendor/glib/LICENSE"
            untouched.write_bytes(untouched.read_bytes() + b"changed")
            with self.assertRaisesRegex(VERIFY.Failure, "backport-publisher-source"):
                VERIFY.verify(root)

    def test_extra_or_missing_source_cannot_be_covered_by_the_pin(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            shutil.copytree(ROOT / "vendor/glib", root / "vendor/glib")
            (root / "tools/backports").mkdir(parents=True)
            shutil.copyfile(ROOT / "tools/backports/glib-0.18.5.json", root / "tools/backports/glib-0.18.5.json")
            (root / "vendor/glib/unreviewed.rs").write_text("unreviewed")
            with self.assertRaisesRegex(VERIFY.Failure, "backport-file-set"):
                VERIFY.verify(root)
