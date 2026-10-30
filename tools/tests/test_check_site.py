"""Check the actual Pages payload, including link and artifact escape refusals."""
import importlib.util
from html import escape
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


def recipe_fixture(language="en", first_example=1):
    """Independent complete command examples for the documented support cells."""
    input_labels = {
        "en": ("MP4", "DASH", "HLS / fMP4", "HLS / MPEG-TS (video)", "HLS / MPEG-TS (audio only)"),
        "ja": ("MP4", "DASH", "HLS / fMP4", "HLS / MPEG-TS（映像）", "HLS / MPEG-TS（音声のみ）"),
    }
    formats = (("mp4", "MP4", ("clear", "cenc", "cbcs")),
               ("dash", "DASH", ("clear", "cenc", "cbcs")),
               ("hls", "HLS / fMP4", ("clear", "cbcs", "hls-aes128")),
               ("hls-ts", "HLS / MPEG-TS", ("clear", "hls-aes128", "hls-sample-aes")))
    rows = ("mp4", "dash", "hls-fmp4", "hls-ts-video", "hls-ts-audio")
    protection_labels = {"clear": "Clear", "cenc": "cenc", "cbcs": "cbcs",
                         "hls-aes128": "AES-128", "hls-sample-aes": "SAMPLE-AES"}
    input_profiles = (("clear", "cenc", "cbcs"), ("clear", "cenc", "cbcs"),
                      ("clear", "cenc", "cbcs", "hls-aes128"),
                      ("clear", "hls-aes128", "hls-sample-aes"),
                      ("clear", "hls-aes128", "hls-sample-aes"))
    content = ['<h3 id="recipe-setup">Prepare a private key file</h3><p>Use separate input and output keys.</p>']
    example = first_example
    for index, row in enumerate(rows):
        for output_format, output_label, targets in formats:
            for target in targets:
                identifier = "recipe-" + row + "-" + output_format + "-" + target
                heading = input_labels[language][index] + " → " + output_label + " / " + protection_labels[target]
                suffix = "mp4" if row == "mp4" else "mpd" if row == "dash" else "m3u8"
                source = "src/input." + suffix
                output = "result.mp4" if output_format == "mp4" else "result"
                groups = []
                for group in ((input_profiles[index][0],), input_profiles[index][1:]):
                    protection = group[0]
                    label = " / ".join(protection_labels[item] for item in group)
                    input_keys = " --keys-file keys.json" if protection != "clear" else ""
                    runtime_keys = " --keys-file keys.json" if protection != "clear" or target != "clear" else ""
                    name = "clear" if protection == "clear" else "encrypted"
                    groups.append("# " + label + " input\n"
                                  'drmr analyze "https://example.com/' + name + "." + suffix + '" \\\n'
                                  "  --download-dir src" + input_keys + " &&\n"
                                  "drmr plan " + source + " --to " + target + " --format " + output_format + " \\\n"
                                  "  --output " + output + " --save plan.json" + input_keys + " &&\n"
                                  "drmr convert --plan plan.json" + runtime_keys + " &&\n"
                                  "drmr verify --plan plan.json" + runtime_keys)
                code = "\n\n".join(groups)
                content.append('<h3 id="' + identifier + '" tabindex="-1">' + heading + '</h3>'
                               '<p><a href="#recipe-setup">Prepare the key configuration</a></p>'
                               '<figure class="guide-example" data-code-language="sh" aria-labelledby="ex' + str(example) + '">'
                               '<figcaption id="ex' + str(example) + '">Complete URL conversion example</figcaption>'
                               '<pre tabindex="0"><code>' + escape(code) + '</code></pre></figure>')
                example += 1
    return "".join(content)


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
        for original in (SOURCE / "docs").rglob("*.md"):
            if original.name != "docker.md":
                dest = self.repo / original.relative_to(SOURCE)
                dest.parent.mkdir(parents=True, exist_ok=True)
                shutil.copyfile(original, dest)
        for relative in ("crates/drmr/tests/fixtures/bbb/CENC.md",
                         "crates/drmr/tests/fixtures/bbb/README.md") + CHECK.BUILDER_INPUTS:
            dest = self.repo / relative
            dest.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(SOURCE / relative, dest)

    def check(self):
        return CHECK.validate(self.site, self.repo, pending_docs=("docs/docker.md",))

    def edit_html(self, original, replacement):
        path = self.site / "index.html"
        value = path.read_text(encoding="utf-8")
        self.assertIn(original, value)
        path.write_bytes(value.replace(original, replacement, 1).encode("utf-8"))

    def test_actual_site_is_small_self_contained_and_mount_independent(self):
        result = self.check()
        self.assertEqual(result["files"], 12)
        self.assertLessEqual(result["bytes"], 640 * 1024)
        self.assertEqual(result["byte_budget"], 640 * 1024)
        self.assertEqual(set(result["builder_inputs"]), set(CHECK.BUILDER_INPUTS))
        self.assertEqual({v["chapters"] for v in result["guides"].values()}, {18})
        self.assertEqual(result["mounts"], ["/", "/drmr/"])
        self.assertEqual(result["scripts"], 0)
        self.assertEqual(result["languages"], ["en", "ja"])
        self.assertEqual(set(result["sha256"]), CHECK.FILES)
        self.assertEqual(result["pending_docs"], ["docs/docker.md"])


    def test_stale_generated_guide_is_refused_without_rewriting_inputs_or_output(self):
        path = self.site / "guide/index.html"
        path.write_bytes(path.read_bytes().replace(b"User guide", b"Stale guide", 1))
        changed = path.read_bytes()
        with self.assertRaisesRegex(CHECK.Failure, "site-guide-generated"):
            self.check()
        self.assertEqual(path.read_bytes(), changed)


    def test_crlf_generated_output_is_not_accepted_as_byte_identical(self):
        path = self.site / "ja/guide/index.html"
        original = path.read_bytes()
        self.assertIn(b"\n", original)
        # Isolate byte identity from the separate transfer-budget invariant.
        changed = original.replace(b"\n", b"\r\n", 1)
        self.assertEqual(len(changed), len(original) + 1)
        path.write_bytes(changed)
        with self.assertRaisesRegex(CHECK.Failure, "site-guide-generated"):
            self.check()

    def test_generation_input_directories_cannot_follow_external_links(self):
        directory = self.repo / "site-content/guide"
        external = self.root / "external guide sources"
        directory.rename(external)
        self.directory_link(directory, external)
        before = {p.name: p.read_bytes() for p in external.iterdir()}
        with self.assertRaisesRegex(CHECK.Failure, "site-guide-input"):
            self.check()
        self.assertEqual({p.name: p.read_bytes() for p in external.iterdir()}, before)

    def rewrite_guides_from_template(self, old, new):
        template = self.repo / "tools/templates/guide.html"
        original = template.read_text(encoding="utf-8")
        self.assertIn(old, original)
        template.write_bytes(original.replace(old, new).encode("utf-8"))
        spec = importlib.util.spec_from_file_location("guide_builder_fixture", self.repo / "tools/build_guide_site.py")
        builder = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(builder)
        for relative, content in builder.render(self.repo).items():
            (self.site / relative).write_bytes(content)

    def test_regeneration_does_not_waive_guide_canonical_or_locale_contract(self):
        self.rewrite_guides_from_template('{{canonical}}', 'https://example.com/guide/')
        with self.assertRaisesRegex(CHECK.Failure, "site-language-links"):
            self.check()

    def test_regeneration_does_not_waive_the_guide_font_notice(self):
        self.rewrite_guides_from_template('assets/fonts/LICENSE-ibm-plex.txt', 'assets/fonts/README.txt')
        with self.assertRaisesRegex(CHECK.Failure, "site-font-license-link"):
            self.check()

    def test_generation_inputs_are_regular_single_link_files(self):
        source = self.repo / "site-content/guide/en.md"
        external = self.root / "linked-source.md"
        shutil.copyfile(source, external)
        source.unlink()
        os.link(external, source)
        with self.assertRaisesRegex(CHECK.Failure, "site-guide-input"):
            self.check()

    def test_conversion_example_generator_is_provenance_bound(self):
        helper = self.repo / "tools/conversion_examples.py"
        original = helper.read_text(encoding="utf-8")
        helper.write_bytes((original + '\n_original_markdown = markdown\n'
                          'def markdown(locale):\n'
                          '    return _original_markdown(locale).replace("drmr analyze", "drmr version")\n').encode("utf-8"))
        with self.assertRaisesRegex(CHECK.Failure, "site-guide-generated"):
            self.check()

    def test_imported_conversion_example_generator_cannot_be_hardlinked(self):
        helper = self.repo / "tools/conversion_examples.py"
        external = self.root / "linked-conversion-examples.py"
        helper.rename(external)
        os.link(external, helper)
        with self.assertRaisesRegex(CHECK.Failure, "site-guide-input"):
            self.check()

    def test_missing_conversion_matrix_is_refused(self):
        for origin in ("index.html", "ja/index.html"):
            path = self.site / origin
            value = path.read_text(encoding="utf-8")
            value = re.sub(r'<table\b[^>]*class="conversion-matrix"[\s\S]*?</table>', '', value)
            path.write_bytes(value.encode("utf-8"))
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
        path.write_bytes(re.sub(r'\s*<link rel="canonical"[^>]+>', '', original).encode("utf-8"))
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
                path.write_bytes(original.replace(old, new, 1).encode("utf-8"))
                with self.assertRaisesRegex(CHECK.Failure, token):
                    self.check()
        path.write_bytes(original.encode("utf-8"))

    def test_wrong_or_duplicate_alternates_and_language_switch_are_refused(self):
        path = self.site / "index.html"
        original = path.read_text(encoding="utf-8")
        for value in (original.replace('hreflang="ja"', 'hreflang="en"', 1),
                      original.replace('https://kazuhide.github.io/drmr/ja/', 'https://example.com/ja/', 1),
                      original.replace('href="./ja/"', 'href="#main"', 1)):
            path.write_bytes(value.encode("utf-8"))
            with self.assertRaisesRegex(CHECK.Failure, "site-language-links"):
                self.check()

    def test_fixed_revision_and_ci_job_links_use_only_this_repository(self):
        revision = "0123456789abcdef0123456789abcdef01234567"
        for path in ("commit/" + revision, "actions/runs/123/job/456"):
            with self.subTest(path=path):
                self.assertIsNone(CHECK.document("https://github.com/kazuhide/drmr/" + path,
                                                 self.repo, ()))

    def test_release_download_link_accepts_only_the_canonical_repository(self):
        reference = "https://github.com/kazuhide/drmr/releases"
        self.assertIsNone(CHECK.document(reference, self.repo, ()))
        invalid = (reference + "/", reference + "?download=1", reference + "#assets",
                   reference.replace("/drmr/", "/other/"),
                   reference.replace("https://", "http://"),
                   reference.replace("github.com", "github.com.example.test"))
        for candidate in invalid:
            with self.subTest(candidate=candidate), self.assertRaises(CHECK.Failure):
                CHECK.document(candidate, self.repo, ())

    def test_evidence_link_variants_and_other_repositories_are_refused(self):
        revision = "0123456789abcdef0123456789abcdef01234567"
        paths = ("commit/main", "commit/" + revision[:-1], "commit/" + revision.upper(),
                 "commit/" + revision + "/", "commit/%30" + revision[1:],
                 "actions/runs/0/job/456", "actions/runs/123/job/0",
                 "actions/runs/123", "actions/runs/123/job/456/", "issues/123")
        references = ["https://github.com/kazuhide/drmr/" + path for path in paths]
        references += ["https://github.com/kazuhide/other/commit/" + revision,
                       "https://github.com/other/drmr/actions/runs/123/job/456",
                       "http://github.com/kazuhide/drmr/commit/" + revision,
                       "https://github.com/kazuhide/drmr/commit/" + revision + "?raw=1",
                       "https://github.com/kazuhide/drmr/commit/" + revision + "#diff",
                       "https://github.com/kazuhide/drmr/actions/runs/123/job/456#step:1:1"]
        for reference in references:
            with self.subTest(reference=reference), self.assertRaises(CHECK.Failure):
                CHECK.document(reference, self.repo, ())

    def test_pending_document_is_an_explicit_failure_by_default(self):
        with self.assertRaisesRegex(CHECK.Failure, "missing-repository-document"):
            CHECK.validate(self.site, self.repo)

    def test_missing_asset_and_unknown_artifact_are_refused(self):
        (self.site / "assets/drmr.svg").unlink()
        with self.assertRaisesRegex(CHECK.Failure, "site-file-set"):
            self.check()
        shutil.copyfile(SOURCE / "site/assets/drmr.svg", self.site / "assets/drmr.svg")
        (self.site / "keys.json").write_bytes("{}".encode("utf-8"))
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
                path.write_bytes(original.replace("./assets/site.css", reference).encode("utf-8"))
                with self.assertRaisesRegex(CHECK.Failure, "site-reference"):
                    self.check()

    def test_css_font_escape_and_external_import_are_refused(self):
        path = self.site / "assets/site.css"
        original = path.read_text(encoding="utf-8")
        path.write_bytes(original.replace("./fonts/ibm-plex-sans-latin-400-normal.woff2", "../../outside.woff2").encode("utf-8"))
        with self.assertRaisesRegex(CHECK.Failure, "site-reference"):
            self.check()
        path.write_bytes((original + '\n@import "https://example.com/style.css";').encode("utf-8"))
        with self.assertRaisesRegex(CHECK.Failure, "site-active-content"):
            self.check()

    def test_scripts_handlers_forms_and_duplicate_ids_are_refused(self):
        path = self.site / "index.html"
        original = path.read_text(encoding="utf-8")
        for injected in ('<script src="https://example.com/a.js"></script>', '<img src="./assets/drmr.svg" onload="fetch(1)">', '<form></form>', '<div id="main"></div>'):
            with self.subTest(injected=injected):
                path.write_bytes(original.replace("</body>", injected + "</body>").encode("utf-8"))
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
                path.write_bytes(original.replace(old, new, 1).encode("utf-8"))
                with self.assertRaisesRegex(CHECK.Failure, token):
                    self.check()

    def test_transfer_budget_is_enforced_on_actual_payload_bytes(self):
        path = self.site / "assets/site.css"
        path.write_bytes(path.read_bytes() + b" " * CHECK.MAX_BYTES)
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


    def regenerate_guide_fixture(self):
        spec = importlib.util.spec_from_file_location("chapter_link_builder_fixture", self.repo / "tools/build_guide_site.py")
        builder = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(builder)
        for relative, content in builder.render(self.repo).items():
            (self.site / relative).write_bytes(content)

    def test_portable_same_language_chapter_links_pass_after_source_and_output_regeneration(self):
        for language, canonical, output in (
                ("en", "https://kazuhide.github.io/drmr/guide/", "guide/index.html"),
                ("ja", "https://kazuhide.github.io/drmr/ja/guide/", "ja/guide/index.html")):
            with self.subTest(language=language):
                source = self.repo / ("site-content/guide/" + language + ".md")
                reference = canonical + "#cli-reference"
                source.write_bytes((source.read_text(encoding="utf-8") +
                                  "\n[CLI chapter details](" + reference + ")\n").encode("utf-8"))
                self.regenerate_guide_fixture()
                self.assertIn('href="' + reference + '"', (self.site / output).read_text(encoding="utf-8"))
                self.assertEqual(self.check()["guides"][output]["chapters"], 18)

    def test_regeneration_does_not_waive_portable_chapter_url_boundaries(self):
        cases = (
            "{canonical}#unknown", "{canonical}", "{canonical}#", "{canonical}?#cli-reference",
            "{canonical}#%63li-reference", "{canonical}#CLI-reference", "{other}#cli-reference",
            "https://example.com/drmr/guide/#cli-reference",
            "https://kazuhide.github.io/drmr/guide/index.html#cli-reference",
            "https://kazuhide.github.io:443/drmr/guide/#cli-reference",
        )
        for language, canonical, other, output in (
                ("en", "https://kazuhide.github.io/drmr/guide/", "https://kazuhide.github.io/drmr/ja/guide/", "guide/index.html"),
                ("ja", "https://kazuhide.github.io/drmr/ja/guide/", "https://kazuhide.github.io/drmr/guide/", "ja/guide/index.html")):
            source = self.repo / ("site-content/guide/" + language + ".md")
            original = source.read_text(encoding="utf-8")
            for pattern in cases:
                reference = pattern.format(canonical=canonical, other=other)
                with self.subTest(language=language, reference=reference):
                    source.write_bytes((original + "\n[Chapter details](" + reference + ")\n").encode("utf-8"))
                    self.regenerate_guide_fixture()
                    self.assertIn('href="' + reference + '"', (self.site / output).read_text(encoding="utf-8"))
                    with self.assertRaisesRegex(CHECK.Failure, "site-guide-reference"):
                        self.check()
            source.write_bytes(original.encode("utf-8"))
            self.regenerate_guide_fixture()

    def test_regeneration_keeps_primary_reference_count_set_and_location_strict(self):
        primary = "https://www.w3.org/TR/eme-stream-mp4/"
        source = self.repo / "site-content/guide/en.md"
        original = source.read_text(encoding="utf-8")
        primary_line = next(line for line in original.splitlines(keepends=True) if primary in line)
        moved = original.replace(primary_line, "", 1).replace(
            "## Choose an operation", "[Moved reference](" + primary + ")\n\n## Choose an operation", 1)
        mutations = (
            original.replace(primary, "https://kazuhide.github.io/drmr/guide/#references", 1),
            original + "\n[Duplicate reference](" + primary + ")\n",
            original.replace(primary, "https://example.com/normative-reference", 1),
            moved,
        )
        for changed in mutations:
            with self.subTest(changed=changed[-100:]):
                source.write_bytes(changed.encode("utf-8"))
                self.regenerate_guide_fixture()
                with self.assertRaisesRegex(CHECK.Failure, "site-guide-reference"):
                    self.check()


class ConversionMatrix(unittest.TestCase):
    def fixture(self, language="en"):
        labels = {
            "en": ("Input and output support", "Conversion support matrix", "Supported conversion profiles", "Input ↓ / Output →", "Outside profile",
                   ["Progressive or fragmented", "Fragmented MP4 tracks", "Fragmented MP4 (fMP4)", "Video with optional audio", "Audio only"]),
            "ja": ("入力と出力の対応表", "変換対応表", "対応する変換プロファイル", "入力 ↓ / 出力 →", "対象外",
                   ["プログレッシブ / フラグメント化", "フラグメント化MP4トラック", "フラグメント化MP4（fMP4）", "映像（音声は省略可）", "音声のみ"]),
        }
        heading, region, caption, corner, outside, profiles = labels[language]
        standard = ["Clear/cenc/cbcs", "Clear/cenc/cbcs", "Clear/cbcs/AES-128", "Clear/AES-128/SAMPLE-AES"]
        rows = [
            ("mp4", "MP4", "Clear/cenc/cbcs", standard),
            ("dash", "DASH", "Clear/cenc/cbcs", standard),
            ("hls-fmp4", "HLS", "Clear/cenc/cbcs/AES-128", standard),
            ("hls-ts-video", "HLS / MPEG-TS", "Clear/AES-128/SAMPLE-AES", standard),
            ("hls-ts-audio", "HLS / MPEG-TS", "Clear/AES-128/SAMPLE-AES", standard),
        ]
        def protection(value, row=None, column=None):
            parts = []
            for token in value.split("/"):
                part = '<span>Clear</span>' if token == "Clear" else '<code>' + token + '</code>'
                if row is not None:
                    target = {"Clear": "clear", "AES-128": "hls-aes128", "SAMPLE-AES": "hls-sample-aes"}.get(token, token)
                    output = ("mp4", "dash", "hls", "hls-ts")[column]
                    input_label = (("MP4", "DASH", "HLS / fMP4", "HLS / MPEG-TS (video)", "HLS / MPEG-TS (audio only)") if language == "en" else
                                   ("MP4", "DASH", "HLS / fMP4", "HLS / MPEG-TS（映像）", "HLS / MPEG-TS（音声のみ）"))[index]
                    heading = input_label + " → " + ("MP4", "DASH", "HLS / fMP4", "HLS / MPEG-TS")[column] + " / " + token
                    label = heading
                    part = '<a href="guide/#recipe-' + row + '-' + output + '-' + target + '" aria-label="' + label + '">' + part + '</a>'
                parts.append(part)
            return " / ".join(parts)
        body = []
        for index, (name, form, inputs, outputs) in enumerate(rows):
            note = "<sup>2</sup>" if index == 0 else "<sup>3</sup>" if index >= 3 else ""
            header = ('<th scope="row"><strong class="input-format">' + form + '</strong>'
                      '<span class="input-profile">' + profiles[index] + note + '</span>'
                      '<span class="input-protections">' + protection(inputs) + '</span></th>')
            cells = ['<td>' + (protection(value, name, column) if value else outside) + ('<sup>1</sup>' if column == 0 else '<sup>4</sup>' if column == 3 and index < 3 else "") + '</td>' for column, value in enumerate(outputs)]
            body.append('<tr id="' + name + '">' + header + "".join(cells) + '</tr>')
        return ('<main><section class="hero"><h1>drmr</h1></section><section id="support"><h2>' + heading + '</h2>'
                '<p id="matrix-guide">Qualified profiles only.</p><p id="matrix-scroll-hint">Scroll horizontally.</p>'
                '<div class="matrix-scroll" role="region" tabindex="0" aria-label="' + region + '" aria-describedby="matrix-guide matrix-scroll-hint">'
                '<table class="conversion-matrix" aria-describedby="matrix-limits matrix-definitions"><caption>' + caption + '</caption><thead><tr>'
                + '<th scope="col" aria-label="' + corner + '"><span class="axis-input">' + corner.split(' / ')[0] + '</span> <span class="axis-output">' + corner.split(' / ')[1] + '</span></th>'
                + ''.join('<th scope="col">' + label + '</th>' for label in ["MP4", "DASH", "HLS fMP4", "HLS MPEG-TS"])
                + '</tr></thead><tbody>' + ''.join(body) + '</tbody></table></div>'
                '<ol id="matrix-limits"><li>Single track.</li><li>Audio edit profile.</li><li>Transport stream profile.</li><li>Fragmented MP4 to TS profile.</li></ol>'
                '<p id="matrix-definitions">Protection definitions.</p></section><section id="workflow"></section></main>')

    def test_both_locale_matrices_retain_all_input_and_output_profiles(self):
        for language in ("en", "ja"):
            with self.subTest(language=language):
                result = CHECK.validate_matrix(self.fixture(language), language)
                self.assertEqual((result["rows"], result["output_columns"], result["cells"]), (5, 4, 20))
                self.assertEqual(result["recipe_links"], 60)

    def test_each_output_protection_links_only_to_its_own_conversion_example(self):
        for language in ("en", "ja"):
            original = self.fixture(language)
            for old, new in (
                    ('href="guide/#recipe-mp4-dash-cbcs"', 'href="guide/#recipe-dash-dash-cbcs"'),
                    ('href="guide/#recipe-mp4-dash-cbcs"', 'href="guide/#recipe-mp4-hls-cbcs"'),
                    ('href="guide/#recipe-mp4-dash-cbcs"', 'href="guide/#recipe-mp4-dash-cenc"'),
                    ('href="guide/#recipe-mp4-dash-cbcs"', 'href="guide/#recipe-mp4-dash-cbcs-extra"'),
                    ('href="guide/#recipe-mp4-dash-cbcs"', 'href="../guide/#recipe-mp4-dash-cbcs"'),
                    ('href="guide/#recipe-mp4-dash-cbcs"', ''),
                    ('aria-label="MP4 → DASH / cbcs', 'aria-label="DASH → MP4 / cbcs')):
                with self.subTest(language=language, changed=new), self.assertRaisesRegex(
                        CHECK.Failure, "site-matrix-recipe-link"):
                    CHECK.validate_matrix(original.replace(old, new, 1), language)

    def test_missing_extra_nested_and_unavailable_cell_links_are_refused(self):
        original = self.fixture()
        link = '<a href="guide/#recipe-mp4-mp4-clear" aria-label="MP4 → MP4 / Clear"><span>Clear</span></a>'
        self.assertIn(link, original)
        for old, new in (
                (link, '<span>Clear</span>'),
                (link, link + '<a href="guide/#recipe-mp4-mp4-clear">Extra</a>'),
                (link, link.replace('<span>Clear</span>', '<a href="#other"><span>Clear</span></a>')),
                (link, link.replace('recipe-mp4-mp4-clear', 'recipe-mp4-hls-ts-clear'))):
            with self.subTest(changed=new), self.assertRaisesRegex(CHECK.Failure, "site-matrix-recipe-link"):
                CHECK.validate_matrix(original.replace(old, new, 1), "en")

    def test_cenc_input_cannot_be_omitted_from_either_hls_fmp4_row(self):
        for language in ("en", "ja"):
            original = self.fixture(language)
            start = original.index('<tr id="hls-fmp4">')
            end = original.index('</th>', start)
            header = original[start:end]
            self.assertIn(' / <code>cenc</code>', header)
            changed = (original[:start] + header.replace(' / <code>cenc</code>', '', 1)
                       + original[end:])
            with self.subTest(language=language), self.assertRaisesRegex(
                    CHECK.Failure, "site-matrix-protections"):
                CHECK.validate_matrix(changed, language)

    def test_missing_duplicate_transposed_and_changed_cells_are_refused(self):
        original = self.fixture()
        mutations = [
            original.replace('<td>', '', 1),
            original.replace('id="dash"', 'id="mp4"', 1),
            original.replace('<th scope="col">MP4</th>', '<th scope="col">DASH</th>', 1),
            original.replace('<td>', '<td><span>Clear</span>', 1),
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
            ('Progressive or fragmented', 'Standard profile'), ('<sup>2</sup>', ''), ('<sup>1</sup>', '<sup>3</sup>'),
            ('tabindex="0"', ''), ('role="region"', 'role="group"'),
            ('aria-describedby="matrix-guide matrix-scroll-hint"', 'aria-describedby="matrix-guide"'),
            ('<p id="matrix-scroll-hint">Scroll horizontally.</p>', ''),
            ('<section id="support">', '<section id="support-old">'),
            ('<li>Single track.</li>', ''), ('<span>Clear</span>', 'Clear'),
        ]:
            with self.subTest(old=old), self.assertRaises(CHECK.Failure):
                CHECK.validate_matrix(original.replace(old, new, 1), "en")

    def test_fragmented_mp4_ts_qualification_note_cannot_be_omitted(self):
        value = self.fixture().replace('<sup>4</sup>', '', 1)
        with self.assertRaisesRegex(CHECK.Failure, "site-matrix-notes"):
            CHECK.validate_matrix(value, "en")


class GuideReferences(unittest.TestCase):
    def test_nested_guides_reach_only_allowlisted_pages_and_shared_assets(self):
        files = CHECK.FILES | {"guide/index.html", "ja/guide/index.html", "assets/guide.css"}
        cases = [("../assets/guide.css", "guide/index.html", "assets/guide.css"),
                 ("../../assets/guide.css", "ja/guide/index.html", "assets/guide.css"),
                 ("../ja/guide/#verification", "guide/index.html", "ja/guide/index.html"),
                 ("../../guide/#verification", "ja/guide/index.html", "guide/index.html"),
                 ("../", "ja/guide/index.html", "ja/index.html"),
                 ("./guide/", "index.html", "guide/index.html")]
        for reference, origin, expected in cases:
            with self.subTest(reference=reference, origin=origin):
                self.assertEqual(CHECK.local_reference(reference, origin, files),
                                 (expected, "verification" if "#" in reference else ""))
        for reference in ("../../../README.md", "../../%2e%2e/README.md",
                          "%2e%2e/../assets/guide.css", "../../assets/../index.html",
                          "../../assets%2fguide.css", "../../keys.json"):
            with self.subTest(reference=reference), self.assertRaises(CHECK.Failure):
                CHECK.local_reference(reference, "ja/guide/index.html", files)


class GuideMarkup(unittest.TestCase):
    def fixture(self, language="en"):
        chapters = []
        for name in CHECK.GUIDE_CHAPTERS:
            body = "<p>Complete chapter content with a bounded, meaningful explanation.</p>"
            if name == "introduction":
                body += ('<figure class="guide-example" data-code-language="sh" aria-labelledby="ex1">'
                         '<figcaption id="ex1">Read media metadata</figcaption>'
                         '<pre tabindex="0"><code>drmr analyze source.mp4 --metadata-only</code></pre></figure>'
                         '<div class="guide-table-scroll" role="region" aria-labelledby="table-1-title" tabindex="0">'
                         '<table><caption id="table-1-title">Known profiles</caption><thead><tr><th scope="col">Input</th></tr></thead>'
                         '<tbody><tr><th scope="row">MP4</th></tr></tbody></table></div>')
            if name == "url-input":
                body += recipe_fixture(language, first_example=2)
            if name == "references":
                for url in ("https://www.w3.org/TR/eme-stream-mp4/", "https://www.rfc-editor.org/rfc/rfc8216.html",
                            "https://developer.apple.com/library/archive/documentation/AudioVideo/Conceptual/HLS_Sample_Encryption/",
                            "https://www.w3.org/TR/mse-byte-stream-format-isobmff/"):
                    body += '<a href="' + url + '">Primary format reference</a>'
            chapters.append('<section id="' + name + '" data-guide-chapter="' + name + '"><h2 id="' + name + '-title">'
                            + name + '</h2>' + body + '</section>')
        toc = ''.join('<a href="#' + name + '">' + name + '</a>' for name in CHECK.GUIDE_CHAPTERS)
        return '<nav class="guide-toc-nav" aria-label="Guide chapters">' + toc + '</nav><article class="guide-content" id="guide-content">' + ''.join(chapters) + '</article>'

    def test_external_primary_references_are_exact_and_guide_chapter_scoped(self):
        original = self.fixture()
        CHECK.validate_guide(original, "en")
        for value in (original.replace('https://www.w3.org/TR/eme-stream-mp4/', 'https://example.com/reference'),
                      original.replace('https://www.w3.org/TR/eme-stream-mp4/', 'https://www.w3.org/TR/eme-stream-mp4/?token=hidden'),
                      original.replace('<h2 id="introduction-title">', '<a href="https://www.w3.org/TR/eme-stream-mp4/">Extra</a><h2 id="introduction-title">')):
            with self.subTest(), self.assertRaises(CHECK.Failure):
                CHECK.validate_guide(value, "en")

    def test_native_figure_cannot_be_overridden_with_a_group_role(self):
        invalid = self.fixture().replace('class="guide-example"', 'class="guide-example" role="group"')
        with self.assertRaises(CHECK.Failure):
            CHECK.validate_guide(invalid, "en")

    def test_missing_chapters_code_labels_and_table_scopes_remain_refused(self):
        original = self.fixture()
        self.assertEqual(CHECK.validate_guide(original, "en"),
                         {"chapters": 18, "code_examples": 61, "tables": 1, "conversion_examples": 60})
        for old, new in [('data-guide-chapter="verification"', 'data-guide-chapter="missing"'),
                         ('href="#verification"', 'href="#missing"'), ('tabindex="0"', ''),
                         ('aria-labelledby="ex1"', ''), ('data-code-language="sh"', 'data-code-language="unknown"'), ('scope="row"', '')]:
            with self.subTest(old=old), self.assertRaises(CHECK.Failure):
                CHECK.validate_guide(original.replace(old, new, 1), "en")


    def test_each_portable_chapter_reference_is_internal_only_for_its_language(self):
        for language, canonical in (("en", "https://kazuhide.github.io/drmr/guide/"),
                                    ("ja", "https://kazuhide.github.io/drmr/ja/guide/")):
            original = self.fixture(language)
            if language == "ja":
                original = original.replace('aria-label="Guide chapters"', 'aria-label="目次"')
            for chapter in CHECK.GUIDE_CHAPTERS:
                with self.subTest(language=language, chapter=chapter):
                    value = original.replace('<h2 id="introduction-title">',
                        '<a href="' + canonical + '#' + chapter + '">Chapter details</a><h2 id="introduction-title">', 1)
                    self.assertEqual(CHECK.validate_guide(value, language)["chapters"], 18)

    def test_chapter_reference_variants_cannot_bypass_the_primary_reference_contract(self):
        for language, canonical, other in (
                ("en", "https://kazuhide.github.io/drmr/guide/", "https://kazuhide.github.io/drmr/ja/guide/"),
                ("ja", "https://kazuhide.github.io/drmr/ja/guide/", "https://kazuhide.github.io/drmr/guide/")):
            original = self.fixture(language)
            if language == "ja":
                original = original.replace('aria-label="Guide chapters"', 'aria-label="目次"')
            references = (
                canonical, canonical + '#', canonical + '#missing', canonical + '#%63li-reference',
                canonical + '#CLI-reference', canonical + '?raw=1#cli-reference',
                canonical + '?#cli-reference', other + '#cli-reference',
                canonical.replace('https://', 'http://') + '#cli-reference',
                canonical.replace('https://', 'https://user@') + '#cli-reference',
                canonical.replace('https://', 'https://user:password@') + '#cli-reference',
                canonical.replace('kazuhide.github.io', 'KAZUHIDE.github.io') + '#cli-reference',
                canonical.replace('kazuhide.github.io', 'kazuhide.github.io:443') + '#cli-reference',
                canonical.replace('kazuhide.github.io', 'example.com') + '#cli-reference',
                canonical.replace('/drmr/', '/other/') + '#cli-reference',
                canonical + 'index.html#cli-reference', canonical + './#cli-reference',
                canonical.rstrip('/') + '#cli-reference',
            )
            for reference in references:
                with self.subTest(language=language, reference=reference):
                    value = original.replace('<h2 id="introduction-title">',
                        '<a href="' + reference + '">Chapter details</a><h2 id="introduction-title">', 1)
                    with self.assertRaisesRegex(CHECK.Failure, "site-guide-reference"):
                        CHECK.validate_guide(value, language)


class ConversionExamples(unittest.TestCase):
    def check(self, text, language="en"):
        return CHECK.validate_conversion_examples(CHECK.MatrixMarkup(text).root, language)

    def test_all_supported_examples_have_complete_url_to_independent_verification_flow(self):
        for language in ("en", "ja"):
            with self.subTest(language=language):
                self.assertEqual(self.check(recipe_fixture(language), language), 60)

    def test_missing_extra_duplicate_or_unfocusable_recipe_heading_is_refused(self):
        original = recipe_fixture()
        for old, new in (
                ('id="recipe-mp4-dash-cbcs"', 'id="recipe-mp4-dash-unknown"'),
                ('id="recipe-mp4-dash-cbcs"', 'id="recipe-mp4-dash-cenc"'),
                ('id="recipe-mp4-dash-cbcs" tabindex="-1"', 'id="recipe-mp4-dash-cbcs"'),
                ('<h3 id="recipe-mp4-dash-cbcs"', '<h4 id="recipe-mp4-dash-cbcs"'),
                ('MP4 → DASH / cbcs</h3>', 'DASH → MP4 / cbcs</h3>'),
                ('MP4 → DASH / cbcs</h3>', 'MP4 → HLS / fMP4 / cbcs</h3>'),
                ('MP4 → DASH / cbcs</h3>', 'MP4 → DASH / cenc</h3>')):
            value = original.replace(old, new, 1)
            if '<h4 id="recipe-' in value:
                start = value.index('<h4 id="recipe-')
                end = value.index('</h3>', start)
                value = value[:end] + '</h4>' + value[end + 5:]
            with self.subTest(changed=new), self.assertRaisesRegex(CHECK.Failure, "site-guide-recipe"):
                self.check(value)
        with self.assertRaisesRegex(CHECK.Failure, "site-guide-recipe"):
            self.check(original + '<h3 id="recipe-mp4-hls-ts-clear" tabindex="-1">MP4 → HLS / MPEG-TS / Clear</h3>')

    def test_setup_link_and_direct_shell_figure_are_required(self):
        original = recipe_fixture()
        for old, new in (
                ('href="#recipe-setup"', 'href="#key-configuration"'),
                ('<p><a href="#recipe-setup">Prepare the key configuration</a></p>', ''),
                ('<p><a href="#recipe-setup">Prepare the key configuration</a></p>', '<div><p><a href="#recipe-setup">Prepare the key configuration</a></p></div>'),
                ('data-code-language="sh"', 'data-code-language="text"'),
                ('<h3 id="recipe-setup">', '<h3 id="missing-setup">')):
            with self.subTest(changed=new), self.assertRaisesRegex(CHECK.Failure, "site-guide-recipe"):
                self.check(original.replace(old, new, 1))

    def test_swapped_plan_source_target_format_output_and_missing_flow_steps_are_refused(self):
        original = recipe_fixture()
        for old, new in (
                ('drmr plan src/input.mp4', 'drmr plan src/input.mpd'),
                ('--to clear --format mp4', '--to cbcs --format mp4'),
                ('--to clear --format mp4', '--to clear --format dash'),
                ('--output result.mp4', '--output result'),
                ('--save plan.json', '--save other.plan.json'),
                ('drmr convert --plan plan.json', 'drmr convert --plan other.plan.json'),
                ('drmr verify --plan plan.json', 'drmr analyze result.mp4 --metadata-only'),
                ('drmr analyze &quot;https://example.com/clear.mp4&quot;', 'drmr analyze result.mp4'),
                ('--keys-file keys.json', '--keys-file other.json'),
                ('drmr verify --plan', '# drmr verify --plan')):
            with self.subTest(changed=new), self.assertRaisesRegex(CHECK.Failure, "site-guide-recipe-command"):
                self.check(original.replace(old, new, 1))

    def test_every_remote_format_requires_native_acquisition_and_matching_local_entrypoint(self):
        original = recipe_fixture()
        for old, new in (
                ('https://example.com/clear.mp4', 'file:///private/video.mp4'),
                ('https://example.com/clear.mp4', 'https://example.com/manifest.mpd'),
                ('https://example.com/clear.mpd', './remote.mpd'),
                ('https://example.com/clear.mpd', 'http://example.com/clear.mpd'),
                ('https://example.com/clear.mp4', 'https://example.com/cenc.mp4'),
                ('&quot;https://example.com/clear.mp4&quot;', 'https://example.com/clear.mp4'),
                ('drmr plan src/input.mpd', 'drmr plan src/manifest.mpd'),
                ('drmr plan src/input.mp4', 'drmr plan source.mp4'),
                ('--download-dir src', '--download-dir different-source'),
                ('--download-dir src', ''),
                ('drmr analyze', 'curl --fail --location https://example.com/source.mp4\ndrmr analyze'),
                ('https://example.com/clear.m3u8', './remote.m3u8')):
            with self.subTest(changed=new), self.assertRaisesRegex(CHECK.Failure, "site-guide-recipe-command"):
                self.check(original.replace(old, new, 1))

    def test_input_groups_must_be_complete_unique_and_supported(self):
        code = next(CHECK.MatrixMarkup(recipe_fixture()).root.descendants("code")).text()
        first, second = code.split("\n\n")
        for changed in (first, second,
                        "\n\n".join((first, second, second)),
                        "\n\n".join((second, first)),
                        code.replace("# cenc / cbcs input", "# AES-128 input", 1),
                        code.replace("# cenc / cbcs input", "# cenc input", 1),
                        code + "\n\n" + second, code + "\nexit 0"):
            with self.subTest(changed=changed[:90]), self.assertRaisesRegex(CHECK.Failure, "site-guide-recipe-command"):
                CHECK.validate_recipe_commands(changed, "mp4", "mp4", "clear")

    def test_keys_follow_each_input_and_output_role(self):
        code = next(CHECK.MatrixMarkup(recipe_fixture()).root.descendants("code")).text()
        for old, new in (("--download-dir src &&", "--download-dir src --keys-file keys.json &&"),
                         ("--save plan.json &&", "--save plan.json --keys-file keys.json &&"),
                         ("drmr convert --plan plan.json &&", "drmr convert --plan plan.json --keys-file keys.json &&"),
                         ("drmr verify --plan plan.json\n", "drmr verify --plan plan.json --keys-file keys.json\n"),
                         ("--download-dir src --keys-file keys.json", "--download-dir src"),
                         ("--save plan.json --keys-file keys.json", "--save plan.json"),
                         ("drmr convert --plan plan.json --keys-file keys.json", "drmr convert --plan plan.json"),
                         ("drmr verify --plan plan.json --keys-file keys.json", "drmr verify --plan plan.json")):
            self.assertIn(old, code)
            with self.subTest(changed=new), self.assertRaisesRegex(CHECK.Failure, "site-guide-recipe-command"):
                CHECK.validate_recipe_commands(code.replace(old, new, 1), "mp4", "mp4", "clear")
        # A clear source still needs output keys when encrypting at execution time.
        blocks = list(CHECK.MatrixMarkup(recipe_fixture()).root.descendants("code"))
        for operation in ("convert", "verify"):
            encrypted_code = blocks[2].text()
            old = f"drmr {operation} --plan plan.json --keys-file keys.json"
            with self.subTest(output="cbcs", operation=operation), self.assertRaisesRegex(CHECK.Failure, "site-guide-recipe-command"):
                CHECK.validate_recipe_commands(encrypted_code.replace(old, f"drmr {operation} --plan plan.json", 1),
                                               "mp4", "mp4", "cbcs")

    def test_each_four_command_group_stops_on_failure_without_shell_boilerplate(self):
        code = next(CHECK.MatrixMarkup(recipe_fixture()).root.descendants("code")).text()
        for changed in (code.replace(" \\\n", "\n", 1), code.replace(" \\\n", " \\ \n", 1),
                        code.replace(" \\\n", " \\\u2028", 1),
                        code.replace(" \\\n", " \\\n\n", 1), code.replace(" \\\n", " \\\n \n", 1),
                        code.replace("--format mp4 \\\n", "--format mp4 \\\n\n", 1),
                        code.replace("--format mp4 \\\n", "--format mp4 \\\n \n", 1),
                        code.replace(" &&\n", "\n", 1), code.replace(" &&\n", " ;\n", 1),
                        code.replace(" &&\n", " ||\n", 1), code + " &&", "set -eu\n" + code,
                        "umask 077\n" + code, "mkdir work\n" + code, "(\n" + code + "\n)"):
            with self.subTest(changed=changed[:90]), self.assertRaisesRegex(CHECK.Failure, "site-guide-recipe-command"):
                CHECK.validate_recipe_commands(changed, "mp4", "mp4", "clear")


if __name__ == "__main__":
    unittest.main()
