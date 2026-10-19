import importlib.util
import os
from pathlib import Path
import tempfile
import unittest

spec = importlib.util.spec_from_file_location("cargo_cache", Path(__file__).parents[1] / "cargo_cache.py")
cache = importlib.util.module_from_spec(spec)
spec.loader.exec_module(cache)


class CargoCacheFreshness(unittest.TestCase):
    def source(self, root):
        source = root / "source"
        for name in ("Cargo.toml", "Cargo.lock", "crates/app/main.rs", "crates/server/static/app.js", "vendor/library/lib.rs"):
            path = source / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text("first")
        (source / "target").mkdir()
        return source

    def test_equal_or_older_mtimes_cannot_reuse_a_different_program_or_asset(self):
        with tempfile.TemporaryDirectory() as temp:
            source = self.source(Path(temp))
            target = source / "target"
            first = cache.prepare(source, target, "rustc fixture")
            self.assertTrue(first["target_reset"])
            sentinel = target / "cached-binary"
            sentinel.write_text("first")
            self.assertFalse(cache.prepare(source, target, "rustc fixture")["target_reset"])
            for name in ("Cargo.toml", "Cargo.lock", "crates/app/main.rs", "crates/server/static/app.js", "vendor/library/lib.rs"):
                with self.subTest(name=name):
                    sentinel.write_text("previous")
                    path = source / name
                    path.write_text("other")
                    os.utime(path, (1, 1))
                    result = cache.prepare(source, target, "rustc fixture")
                    self.assertTrue(result["target_reset"])
                    self.assertFalse(sentinel.exists())
                    self.assertNotEqual(result["inputs_sha256"], first["inputs_sha256"])
            sentinel.write_text("current")
            os.utime(source / "crates/app/main.rs", None)
            self.assertFalse(cache.prepare(source, target, "rustc fixture")["target_reset"])
            self.assertEqual(sentinel.read_text(), "current")
            self.assertTrue(cache.prepare(source, target, "different rustc")["target_reset"])

    def test_target_containment_preserves_every_outside_file(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            source = self.source(root)
            outside = root / "outside"
            outside.mkdir()
            sentinel = outside / "sentinel"
            sentinel.write_text("keep")
            with self.assertRaisesRegex(ValueError, "owned-target-cache-required"):
                cache.prepare(source, outside, "rustc fixture")
            (source / "target").rmdir()
            (source / "target").symlink_to(outside, target_is_directory=True)
            with self.assertRaisesRegex(ValueError, "owned-target-cache-required"):
                cache.prepare(source, source / "target", "rustc fixture")
            (source / "target").unlink()
            (source / "target").mkdir()
            (source / "target/foreign-link").symlink_to(outside, target_is_directory=True)
            self.assertTrue(cache.prepare(source, source / "target", "rustc fixture")["target_reset"])
            self.assertEqual(sentinel.read_text(), "keep")
            self.assertFalse((source / "target/foreign-link").exists())
            (source / "crates/foreign-link").symlink_to(outside, target_is_directory=True)
            with self.assertRaisesRegex(ValueError, "compile-input-link-refused"):
                cache.prepare(source, source / "target", "rustc fixture")


if __name__ == "__main__":
    unittest.main()
