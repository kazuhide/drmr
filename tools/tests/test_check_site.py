"""Check the actual Pages payload, including link and artifact escape refusals."""
import importlib.util
import os
import re
from pathlib import Path
import shutil
import stat
import subprocess
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch

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
        self.assertEqual(result["files"], 9)
        self.assertLessEqual(result["bytes"], 180 * 1024)
        self.assertEqual(result["mounts"], ["/", "/drmr/"])
        self.assertEqual(result["scripts"], 0)
        self.assertEqual(result["languages"], ["en", "ja"])
        self.assertEqual(set(result["sha256"]), CHECK.FILES)
        self.assertEqual(result["pending_docs"], ["docs/docker.md"])

    def test_missing_conversion_matrix_is_refused(self):
        for origin in ("index.html", "ja/index.html"):
            path = self.site / origin
            value = path.read_text(encoding="utf-8")
            value = re.sub(r'<table\b[^>]*class="conversion-matrix"[\s\S]*?</table>', '', value)
            path.write_text(value, encoding="utf-8")
        with self.assertRaisesRegex(CHECK.Failure, "site-conversion-matrix"):
            self.check()

    def test_localized_assets_and_language_root_resolve_without_escape(self):
        files = CHECK.FILES
        self.assertEqual(CHECK.local_reference("../assets/site.css", "ja/index.html", files),
                         ("assets/site.css", ""))
        self.assertEqual(CHECK.local_reference("../", "ja/index.html", files), ("index.html", ""))
        self.assertEqual(CHECK.local_reference("./ja/", "index.html", files), ("ja/index.html", ""))
        for reference in ("../../index.html", "../%2e%2e/index.html", "%2e%2e/assets/site.css",
                          "../assets/../index.html", "../assets%2fsite.css", "../keys.json"):
            with self.subTest(reference=reference), self.assertRaises(CHECK.Failure):
                CHECK.local_reference(reference, "ja/index.html", files)

    def test_canonical_and_reciprocal_alternates_are_required(self):
        path = self.site / "index.html"
        original = path.read_text(encoding="utf-8")
        path.write_text(re.sub(r'\s*<link rel="canonical"[^>]+>', '', original), encoding="utf-8")
        with self.assertRaisesRegex(CHECK.Failure, "site-language-links"):
            self.check()

    def test_japanese_language_links_and_footer_notice_are_checked(self):
        path = self.site / "ja/index.html"
        original = path.read_text(encoding="utf-8")
        page = CHECK.Page()
        page.feed(original)
        reference = next(value for tag, attribute, value in page.references
                         if tag == "a" and value.endswith("LICENSE-ibm-plex.txt"))
        self.assertEqual(reference, "../assets/fonts/LICENSE-ibm-plex.txt")
        self.assertEqual(CHECK.local_reference(reference, "ja/index.html", CHECK.FILES),
                         ("assets/fonts/LICENSE-ibm-plex.txt", ""))
        for old, new, token in (( 'lang="ja"', 'lang="en"', "site-document-semantics"),
                                ('hreflang="en"', 'hreflang="ja"', "site-language-links"),
                                ('href="../" lang="en"', 'href="../" lang="ja"', "site-language-links"),
                                ('href="../" lang="en"', 'href="#main" lang="en"', "site-language-links"),
                                ('../assets/fonts/LICENSE-ibm-plex.txt', '../assets/fonts/README.txt', "site-font-license-link"),
                                ('../assets/fonts/LICENSE-ibm-plex.txt', '../../outside.txt', "site-reference")):
            with self.subTest(token=token):
                self.assertIn(old, original)
                path.write_text(original.replace(old, new, 1), encoding="utf-8")
                with self.assertRaisesRegex(CHECK.Failure, token):
                    self.check()
        path.write_text(original, encoding="utf-8")

    def test_wrong_or_duplicate_alternates_and_language_switch_are_refused(self):
        path = self.site / "index.html"
        original = path.read_text(encoding="utf-8")
        for value in (original.replace('hreflang="ja"', 'hreflang="en"', 1),
                      original.replace('https://kazuhide.github.io/drmr/ja/', 'https://example.com/ja/', 1),
                      original.replace('href="./ja/"', 'href="#main"', 1)):
            path.write_text(value, encoding="utf-8")
            with self.assertRaisesRegex(CHECK.Failure, "site-language-links"):
                self.check()

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

    def test_hardlinks_are_refused_before_upload(self):
        path = self.site / "assets/drmr.svg"
        original = path.read_bytes()
        external = self.root / "external.svg"
        external.write_bytes(original)
        path.unlink()
        os.link(external, path)
        with self.assertRaisesRegex(CHECK.Failure, "site-link-or-special-file"):
            self.check()

    def directory_link(self, link, target):
        if os.name == "nt":
            # Junctions require no symbolic-link privilege. Pass paths only as
            # environment data; a fixed command string avoids MS C argv quoting
            # rules being applied to cmd's own command-language quotes.
            environment = dict(os.environ, DRMR_SITE_TEST_LINK=str(link.absolute()),
                               DRMR_SITE_TEST_TARGET=str(target.absolute()))
            command = 'cmd.exe /D /V:OFF /Q /C mklink /J "%DRMR_SITE_TEST_LINK%" "%DRMR_SITE_TEST_TARGET%"'
            result = subprocess.run(command, executable=os.environ["COMSPEC"], env=environment,
                                    stdin=subprocess.DEVNULL, stdout=subprocess.PIPE,
                                    stderr=subprocess.PIPE, timeout=10)
            self.assertEqual(result.returncode, 0, "directory junction fixture failed")
            self.addCleanup(link.rmdir)
            self.assertTrue(link.lstat().st_file_attributes & stat.FILE_ATTRIBUTE_REPARSE_POINT)
        else:
            link.symlink_to(target, target_is_directory=True)
            self.addCleanup(link.unlink)
            self.assertTrue(link.is_symlink())

    def test_actual_directory_links_are_refused_at_root_and_child(self):
        alias = self.root / "site alias & !data"
        self.directory_link(alias, self.site)
        with self.assertRaises(CHECK.Failure):
            CHECK.validate(alias, self.repo, pending_docs=("docs/docker.md",))
        outside = self.root / "outside fonts \u65e5\u672c\u8a9e"
        shutil.copytree(self.site / "assets/fonts", outside)
        expected = {p.name: p.read_bytes() for p in outside.iterdir()}
        shutil.rmtree(self.site / "assets/fonts")
        self.directory_link(self.site / "assets/fonts", outside)
        with self.assertRaisesRegex(CHECK.Failure, "site-link-or-special-file"):
            self.check()
        self.assertEqual({p.name: p.read_bytes() for p in outside.iterdir()}, expected)

    def test_reparse_attributes_are_refused_even_with_directory_mode(self):
        real_lstat = Path.lstat
        for selected in (self.site, self.site / "assets/fonts"):
            def reparse_lstat(path, *args, **kwargs):
                actual = real_lstat(path, *args, **kwargs)
                if path == selected:
                    return SimpleNamespace(st_mode=actual.st_mode, st_nlink=actual.st_nlink,
                                           st_size=actual.st_size,
                                           st_file_attributes=stat.FILE_ATTRIBUTE_REPARSE_POINT)
                return actual
            with self.subTest(path=selected.name), patch.object(Path, "lstat", reparse_lstat):
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
                                ('<h2 id="support-title">', '<h4 id="support-title">', "site-heading-semantics"),
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


class ConversionMatrix(unittest.TestCase):
    def fixture(self, language="en"):
        labels = {
            "en": ("Input and output support", "Conversion support matrix", "Supported conversion profiles", "Input ↓ / Output →", "Outside profile", "(qualified)",
                   ["Standard profile", "Audio start/end edits", "Fragmented MP4 tracks", "Fragmented MP4 (fMP4)", "Video with optional audio", "Audio only"]),
            "ja": ("入力と出力の対応表", "変換対応表", "対応する変換プロファイル", "入力 ↓ / 出力 →", "対象外", "（限定条件）",
                   ["音声の開始・終了の編集情報なし", "音声の開始・終了の編集情報あり", "フラグメント化MP4トラック", "フラグメント化MP4（fMP4）", "映像（音声は省略可）", "音声のみ"]),
        }
        heading, region, caption, corner, outside, qualified, profiles = labels[language]
        standard = ["Clear/cenc/cbcs", "Clear/cenc/cbcs", "Clear/cbcs/AES-128", ""]
        rows = [
            ("mp4-standard", "MP4", "Clear/cenc/cbcs", standard),
            ("mp4-audio-edit", "MP4", "Clear/cenc", ["Clear", "", "", ""]),
            ("dash", "DASH", "Clear/cenc/cbcs", standard),
            ("hls-fmp4", "HLS", "Clear/cbcs/AES-128", standard),
            ("hls-ts-video", "HLS / MPEG-TS", "Clear/AES-128/SAMPLE-AES", ["", "", "Clear/cbcs/AES-128", "Clear/AES-128/SAMPLE-AES"]),
            ("hls-ts-audio", "HLS / MPEG-TS", "Clear/AES-128/SAMPLE-AES", ["", "", "", "Clear/AES-128/SAMPLE-AES"]),
        ]
        def protection(value):
            return " / ".join('<span>Clear</span>' if token == "Clear" else '<code>' + token + '</code>' for token in value.split("/"))
        body = []
        for index, (name, form, inputs, outputs) in enumerate(rows):
            note = "<sup>2</sup>" if index == 1 else "<sup>3</sup>" if index >= 4 else ""
            header = ('<th scope="row"><strong class="input-format">' + form + '</strong>'
                      '<span class="input-profile">' + profiles[index] + note + '</span>'
                      '<span class="input-protections">' + protection(inputs) + (qualified if index == 1 else "") + '</span></th>')
            cells = ['<td>' + (protection(value) if value else outside) + ('<sup>1</sup>' if column == 0 and index in (0, 2, 3) else "") + '</td>' for column, value in enumerate(outputs)]
            body.append('<tr id="' + name + '">' + header + "".join(cells) + '</tr>')
        return ('<main><section class="hero"><h1>drmr</h1></section><section id="support"><h2>' + heading + '</h2>'
                '<p id="matrix-guide">Qualified profiles only.</p><p id="matrix-scroll-hint">Scroll horizontally.</p>'
                '<div class="matrix-scroll" role="region" tabindex="0" aria-label="' + region + '" aria-describedby="matrix-guide matrix-scroll-hint">'
                '<table class="conversion-matrix" aria-describedby="matrix-limits matrix-definitions"><caption>' + caption + '</caption><thead><tr>'
                + '<th scope="col" aria-label="' + corner + '"><span class="axis-input">' + corner.split(' / ')[0] + '</span> <span class="axis-output">' + corner.split(' / ')[1] + '</span></th>'
                + ''.join('<th scope="col">' + label + '</th>' for label in ["MP4", "DASH", "HLS fMP4", "HLS MPEG-TS"])
                + '</tr></thead><tbody>' + ''.join(body) + '</tbody></table></div>'
                '<ol id="matrix-limits"><li>Single track.</li><li>Audio edit profile.</li><li>Transport stream profile.</li></ol>'
                '<p id="matrix-definitions">Protection definitions.</p></section><section id="workflow"></section></main>')

    def test_both_locale_matrices_retain_all_input_and_output_profiles(self):
        for language in ("en", "ja"):
            with self.subTest(language=language):
                result = CHECK.validate_matrix(self.fixture(language), language)
                self.assertEqual((result["rows"], result["output_columns"], result["cells"]), (6, 4, 24))

    def test_missing_duplicate_transposed_and_changed_cells_are_refused(self):
        original = self.fixture()
        mutations = [
            original.replace('<td>Outside profile</td>', '', 1),
            original.replace('id="mp4-audio-edit"', 'id="mp4-standard"', 1),
            original.replace('<th scope="col">MP4</th>', '<th scope="col">DASH</th>', 1),
            original.replace('<td>Outside profile</td>', '<td><span>Clear</span></td>', 1),
            original.replace('<code>cbcs</code>', '<code>SAMPLE-AES</code>', 1),
            original.replace('<code>cenc</code>', '<span>cenc</span>', 1),
        ]
        for value in mutations:
            with self.subTest(value=value[:80]), self.assertRaises(CHECK.Failure):
                CHECK.validate_matrix(value, "en")

    def test_header_scopes_qualification_notes_and_priority_are_required(self):
        original = self.fixture()
        for old, new in [
            ('scope="row"', 'scope="col"'), ('scope="col"', ''),
            ('<td>', '<td colspan="2">'), ('<th scope="row">', '<th scope="row" rowspan="2">'),
            ('(qualified)', ''), ('<sup>2</sup>', ''), ('<sup>1</sup>', '<sup>3</sup>'),
            ('tabindex="0"', ''), ('role="region"', 'role="group"'),
            ('aria-describedby="matrix-guide matrix-scroll-hint"', 'aria-describedby="matrix-guide"'),
            ('<p id="matrix-scroll-hint">Scroll horizontally.</p>', ''),
            ('<section id="support">', '<section id="support-old">'),
            ('<li>Single track.</li>', ''), ('<span>Clear</span>', 'Clear'),
        ]:
            with self.subTest(old=old), self.assertRaises(CHECK.Failure):
                CHECK.validate_matrix(original.replace(old, new, 1), "en")

    def test_the_audio_edit_row_never_implies_general_mp4_or_ts_support(self):
        value = self.fixture()
        value = value.replace('<td>Outside profile</td>', '<td><span>Clear</span></td>')
        with self.assertRaisesRegex(CHECK.Failure, "site-matrix-protections"):
            CHECK.validate_matrix(value, "en")


if __name__ == "__main__":
    unittest.main()
