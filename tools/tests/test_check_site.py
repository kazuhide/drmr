"""Check the actual Pages payload, including link and artifact escape refusals."""
import importlib.util
import os
from pathlib import Path
import shutil
import tempfile
import unittest

SPEC = importlib.util.spec_from_file_location("check_site", Path(__file__).resolve().parents[1] / "check_site.py")
CHECK = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(CHECK)
SOURCE = Path(__file__).resolve().parents[2]


class Site(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.site = self.root / "site"
        shutil.copytree(SOURCE / "site", self.site)
        self.repo = self.root / "repository"
        for relative in ("README.md", "LICENSE", "docs/usage.md", "docs/conversion-support.md", "docs/media-processing.md", "docs/requirements.md", "docs/windows.md", "docs/desktop.md", "docs/resources.md", "crates/drmr-desktop/icons/icon.svg"):
            dest = self.repo / relative
            dest.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(SOURCE / relative, dest)
        shutil.copytree(SOURCE / "crates/drmr-server/static/vendor/fonts", self.repo / "crates/drmr-server/static/vendor/fonts")

    def check(self):
        return CHECK.validate(self.site, self.repo, pending_docs=("docs/docker.md",))

    def edit_html(self, original, replacement):
        path = self.site / "index.html"
        value = path.read_text(encoding="utf-8")
        self.assertIn(original, value)
        path.write_text(value.replace(original, replacement, 1), encoding="utf-8")

    def test_actual_site_is_small_self_contained_and_mount_independent(self):
        result = self.check()
        self.assertEqual(result["files"], 8)
        self.assertLessEqual(result["bytes"], 180 * 1024)
        self.assertEqual(result["mounts"], ["/", "/drmr/"])
        self.assertEqual(result["scripts"], 0)
        self.assertEqual(result["pending_docs"], ["docs/docker.md"])

    def test_pending_document_is_an_explicit_failure_by_default(self):
        with self.assertRaisesRegex(CHECK.Failure, "missing-repository-document"):
            CHECK.validate(self.site, self.repo)

    def test_missing_asset_and_unknown_artifact_are_refused(self):
        (self.site / "assets/drmr.svg").unlink()
        with self.assertRaisesRegex(CHECK.Failure, "site-file-set"):
            self.check()
        shutil.copyfile(SOURCE / "site/assets/drmr.svg", self.site / "assets/drmr.svg")
        (self.site / "keys.json").write_text("{}", encoding="utf-8")
        with self.assertRaisesRegex(CHECK.Failure, "site-file-set"):
            self.check()

    def test_symlinks_and_hardlinks_are_refused_before_upload(self):
        path = self.site / "assets/drmr.svg"
        original = path.read_bytes()
        external = self.root / "external.svg"
        external.write_bytes(original)
        path.unlink()
        path.symlink_to(external)
        with self.assertRaisesRegex(CHECK.Failure, "site-link-or-special-file"):
            self.check()
        path.unlink()
        os.link(external, path)
        with self.assertRaisesRegex(CHECK.Failure, "site-link-or-special-file"):
            self.check()

    def test_relative_and_encoded_asset_escapes_are_refused(self):
        path = self.site / "index.html"
        original = path.read_text(encoding="utf-8")
        for reference in ("../README.md", "./%2e%2e/README.md", "/assets/site.css", "//example.com/a.css"):
            with self.subTest(reference=reference):
                path.write_text(original.replace("./assets/site.css", reference), encoding="utf-8")
                with self.assertRaisesRegex(CHECK.Failure, "site-reference"):
                    self.check()

    def test_css_font_escape_and_external_import_are_refused(self):
        path = self.site / "assets/site.css"
        original = path.read_text(encoding="utf-8")
        path.write_text(original.replace("./fonts/ibm-plex-sans-latin-400-normal.woff2", "../../outside.woff2"), encoding="utf-8")
        with self.assertRaisesRegex(CHECK.Failure, "site-reference"):
            self.check()
        path.write_text(original + '\n@import "https://example.com/style.css";', encoding="utf-8")
        with self.assertRaisesRegex(CHECK.Failure, "site-active-content"):
            self.check()

    def test_scripts_handlers_forms_and_duplicate_ids_are_refused(self):
        path = self.site / "index.html"
        original = path.read_text(encoding="utf-8")
        for injected in ('<script src="https://example.com/a.js"></script>', '<img src="./assets/drmr.svg" onload="fetch(1)">', '<form></form>', '<div id="main"></div>'):
            with self.subTest(injected=injected):
                path.write_text(original.replace("</body>", injected + "</body>"), encoding="utf-8")
                with self.assertRaises(CHECK.Failure):
                    self.check()

    def test_missing_internal_anchor_is_refused(self):
        self.edit_html('href="#workflow"', 'href="#missing"')
        with self.assertRaisesRegex(CHECK.Failure, "site-anchor"):
            self.check()

    def test_language_heading_order_and_image_alternative_are_required(self):
        path = self.site / "index.html"
        original = path.read_text(encoding="utf-8")
        for old, new, token in (( 'lang="en"', 'lang=""', "site-document-semantics"),
                                ('<h2 id="workflow-title">', '<h4 id="workflow-title">', "site-heading-semantics"),
                                ('height="32" alt=""', 'height="32"', "site-image-semantics")):
            with self.subTest(token=token):
                self.assertIn(old, original)
                path.write_text(original.replace(old, new, 1), encoding="utf-8")
                with self.assertRaisesRegex(CHECK.Failure, token):
                    self.check()

    def test_transfer_budget_is_enforced_on_actual_payload_bytes(self):
        path = self.site / "assets/site.css"
        path.write_bytes(path.read_bytes() + b" " * (180 * 1024))
        with self.assertRaisesRegex(CHECK.Failure, "site-byte-budget"):
            self.check()

    def test_font_and_publisher_notice_bytes_must_match_originals(self):
        for relative in ("assets/fonts/LICENSE-ibm-plex.txt", "assets/fonts/ibm-plex-sans-latin-400-normal.woff2"):
            with self.subTest(relative=relative):
                path = self.site / relative
                original = path.read_bytes()
                path.write_bytes(original + b"\n")
                with self.assertRaisesRegex(CHECK.Failure, "site-vendored-bytes"):
                    self.check()
                path.write_bytes(original)


if __name__ == "__main__":
    unittest.main()
