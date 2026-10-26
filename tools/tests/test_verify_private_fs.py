"""The native handle extension retains every pinned publisher byte."""
import importlib.util
import copy
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location("verify_backports", ROOT / "tools/verify_backports.py")
VERIFY = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(VERIFY)


class PrivateFsSource(unittest.TestCase):
    def copied(self, directory):
        root = Path(directory)
        shutil.copytree(ROOT / "vendor/private-fs", root / "vendor/private-fs")
        (root / "tools/backports").mkdir(parents=True)
        shutil.copyfile(ROOT / "tools/backports/private-fs-0.2.1.json",
                        root / "tools/backports/private-fs-0.2.1.json")
        return root

    def test_exact_publisher_inventory_and_handle_extension(self):
        result = VERIFY.verify_private_fs(ROOT)
        self.assertEqual(result["package"], "private-fs")
        self.assertEqual(result["version"], "0.2.1")
        self.assertEqual(result["files"], 31)
        self.assertEqual(result["patched_files"], 3)

    def test_publisher_bytes_survive_an_autocrlf_checkout(self):
        with tempfile.TemporaryDirectory() as directory:
            home = Path(directory)
            root = self.copied(home / "checkout")
            empty_config = home / "empty-config"
            empty_config.touch()
            empty_template = home / "empty-template"
            empty_template.mkdir()
            env = {key: value for key, value in os.environ.items()
                   if not key.startswith("GIT_")}
            env.update(GIT_CONFIG_NOSYSTEM="1", GIT_CONFIG_GLOBAL=str(empty_config),
                       GIT_ATTR_NOSYSTEM="1")

            def git(*arguments):
                return subprocess.run(
                    ["git", "-c", "core.autocrlf=false", "-c", "core.safecrlf=false",
                     "-c", "core.attributesFile=" + str(empty_config),
                     "-c", "core.hooksPath=" + str(empty_template), *arguments],
                    cwd=root, env=env, check=True, stdout=subprocess.PIPE,
                    stderr=subprocess.PIPE, encoding="utf-8", timeout=10)

            shutil.copyfile(ROOT / ".gitattributes", root / ".gitattributes")
            git("init", "--quiet", "--template=" + str(empty_template))
            git("add", ".gitattributes", "vendor/private-fs", "tools/backports/private-fs-0.2.1.json")
            shutil.rmtree(root / "vendor/private-fs")
            git("-c", "core.autocrlf=true", "checkout", "--", "vendor/private-fs")
            self.assertEqual(VERIFY.verify_private_fs(root), VERIFY.verify_private_fs(ROOT))

    def test_line_ending_changes_remain_source_modifications(self):
        with tempfile.TemporaryDirectory() as directory:
            root = self.copied(directory)
            for name, failure in [("LICENSE-MIT", "native-patch-publisher-source"),
                                  ("src/windows/mod.rs", "native-patch-modified-source")]:
                with self.subTest(file=name):
                    path = root / "vendor/private-fs" / name
                    original = path.read_bytes()
                    changed = original.replace(b"\n", b"\r\n")
                    self.assertNotEqual(changed, original)
                    path.write_bytes(changed)
                    with self.assertRaisesRegex(VERIFY.Failure, failure):
                        VERIFY.verify_private_fs(root)
                    path.write_bytes(original)

    def test_missing_extension_and_unrelated_publisher_changes_are_refused(self):
        with tempfile.TemporaryDirectory() as directory:
            root = self.copied(directory)
            pin = json.loads((root / "tools/backports/private-fs-0.2.1.json").read_text())
            path = root / "vendor/private-fs/src/windows/mod.rs"
            safe = path.read_bytes()
            change = pin["patches"]["src/windows/mod.rs"]["replacements"][0]
            path.write_bytes(safe.replace(change["after"].encode(), change["before"].encode(), 1))
            with self.assertRaisesRegex(VERIFY.Failure, "native-patch-modified-source"):
                VERIFY.verify_private_fs(root)
            path.write_bytes(safe)
            license_file = root / "vendor/private-fs/LICENSE-MIT"
            license_file.write_bytes(license_file.read_bytes() + b"changed")
            with self.assertRaisesRegex(VERIFY.Failure, "native-patch-publisher-source"):
                VERIFY.verify_private_fs(root)

    def test_extra_or_missing_files_are_refused(self):
        with tempfile.TemporaryDirectory() as directory:
            root = self.copied(directory)
            home = root / "vendor/private-fs"
            extra = home / "unreviewed.rs"
            extra.write_text("unreviewed")
            with self.assertRaisesRegex(VERIFY.Failure, "native-patch-file-set"):
                VERIFY.verify_private_fs(root)
            extra.unlink()
            license_file = home / "LICENSE-MIT"
            license_file.unlink()
            with self.assertRaisesRegex(VERIFY.Failure, "native-patch-file-set"):
                VERIFY.verify_private_fs(root)
            if os.name != "nt":
                license_file.symlink_to(ROOT / "vendor/private-fs/LICENSE-MIT")
                with self.assertRaisesRegex(VERIFY.Failure, "native-patch-file-set"):
                    VERIFY.verify_private_fs(root)

    def test_malformed_or_ambiguous_pin_cannot_select_an_unreviewed_patch(self):
        with tempfile.TemporaryDirectory() as directory:
            root = self.copied(directory)
            path = root / "tools/backports/private-fs-0.2.1.json"
            pin = json.loads(path.read_text())
            for changed in [dict(pin, schema_version=True), dict(pin, source_sha256=0),
                            dict(pin, upstream_revision="bad"), dict(pin, future_field=True)]:
                path.write_text(json.dumps(changed))
                with self.assertRaises(VERIFY.Failure):
                    VERIFY.verify_private_fs(root)
            changed = copy.deepcopy(pin)
            changed["patches"]["src/lib.rs"]["replacements"][0]["after"] = "unsafe"
            path.write_text(json.dumps(changed))
            with self.assertRaisesRegex(VERIFY.Failure, "native-patch-shape"):
                VERIFY.verify_private_fs(root)
            path.write_text(path.read_text().replace('"schema_version": 1', '"schema_version": 1, "schema_version": 1'))
            with self.assertRaisesRegex(VERIFY.Failure, "native-patch-identity"):
                VERIFY.verify_private_fs(root)
