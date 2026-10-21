"""Behavioral checks for the deliberately bounded guide publisher."""
import importlib.util
from html.parser import HTMLParser
import os
from pathlib import Path
import py_compile
import tempfile
import unittest


TOOLS = Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location("guide_builder", TOOLS / "build_guide_site.py")
builder = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(builder)


class GuideBuilderTests(unittest.TestCase):
    def test_real_sources_render_reciprocal_chapters_and_semantic_examples(self):
        pages = builder.render(TOOLS.parent)
        self.assertEqual(set(pages), {"guide/index.html", "ja/guide/index.html"})
        for route, page in pages.items():
            html = page.decode()
            self.assertEqual(html.count('data-guide-chapter='), 20)
            self.assertIn('<figure class="guide-example" aria-labelledby="example-1-title" data-code-language="sh">', html)
            self.assertNotIn('role="group"', html)
            self.assertIn('<caption id="table-1-title">', html)
            self.assertIn('scope="row"', html)
            for chapter in builder.CHAPTER_IDS:
                self.assertIn(f'href="#{chapter}"', html)
                other = "../ja/guide/" if route == "guide/index.html" else "../../guide/"
                self.assertIn(f'href="{other}#{chapter}"', html)
            self.assertNotIn('<script', html)

    def test_rendered_examples_keep_exact_source_text_and_validated_languages(self):
        import re

        class Examples(HTMLParser):
            def __init__(self):
                super().__init__(convert_charrefs=True)
                self.examples = []
                self.in_code = False
                self.in_pre = False

            def handle_starttag(self, tag, attrs):
                attrs = dict(attrs)
                if tag == 'figure':
                    self.examples.append([attrs.get('data-code-language'), ''])
                if tag == 'pre':
                    self.in_pre = True
                if tag == 'code' and self.in_pre:
                    self.in_code = True

            def handle_endtag(self, tag):
                if tag == 'code':
                    self.in_code = False
                if tag == 'pre':
                    self.in_pre = False

            def handle_data(self, text):
                if self.in_code:
                    self.examples[-1][1] += text

        for route, page in builder.render(TOOLS.parent).items():
            locale = 'ja' if route.startswith('ja/') else 'en'
            source = (TOOLS.parent / f'site-content/guide/{locale}.md').read_text(encoding='utf-8')
            source = builder.expand_examples(source, locale, TOOLS.parent)
            expected = [[language, code] for language, code in
                        re.findall(r'^```([^\n]*)\n(.*?)^```', source, re.M | re.S)]
            parser = Examples()
            parser.feed(page.decode())
            self.assertEqual(parser.examples, expected)

    def test_recipe_fragment_links_and_headings_are_bounded(self):
        self.assertEqual(builder.inline('[Setup](#recipe-setup)'), '<a href="#recipe-setup">Setup</a>')
        for target in ('#../escape', '#recipe-setup?key=secret', '#recipe%2dsetup', '#bad"id'):
            with self.subTest(target=target), self.assertRaises(ValueError):
                builder.inline('[Bad](' + target + ')')
        output = builder.body('## MP4 → DASH / cbcs {#recipe-mp4-dash-cbcs}', 'en')
        self.assertEqual(output, '<h3 id="recipe-mp4-dash-cbcs" tabindex="-1">MP4 → DASH / cbcs</h3>')

    def test_example_generation_reads_source_even_when_an_old_bytecode_cache_looks_current(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            (root / 'tools').mkdir()
            helper = root / 'tools/conversion_examples.py'
            helper.write_text("def markdown(locale): return 'old'\n")
            before = helper.stat()
            py_compile.compile(str(helper), doraise=True)
            helper.write_text("def markdown(locale): return 'new'\n")
            os.utime(helper, ns=(before.st_atime_ns, before.st_mtime_ns))
            self.assertEqual(builder.expand_examples('# 8. URL\n\n{{conversion-examples}}', 'en', root),
                             '# 8. URL\n\nnew')

    def test_recipe_marker_cannot_be_missing_duplicated_in_code_or_in_another_chapter(self):
        _, source = builder.metadata((TOOLS.parent / 'site-content/guide/en.md').read_text())
        marker = '{{conversion-examples}}'
        variants = [source.replace(marker, ''), source.replace(marker, marker + '\n' + marker),
                    source.replace(marker, '```text\n' + marker + '\n```'),
                    source.replace(marker, '').replace('# 10. ', marker + '\n\n# 10. ', 1),
                    source.replace(marker, 'Prefix ' + marker)]
        for variant in variants:
            with self.subTest(variant=variants.index(variant)), self.assertRaises(ValueError):
                builder.expand_examples(variant, 'en', TOOLS.parent)

    def test_markup_is_escaped_and_unsafe_links_are_refused(self):
        self.assertEqual(builder.inline('`<key>&value`'), '<code>&lt;key&gt;&amp;value</code>')
        with self.assertRaises(ValueError):
            builder.inline('[secret](javascript:alert)')
        with self.assertRaises(ValueError):
            builder.inline('<script>bad</script>')

    def test_malformed_tables_and_unclosed_fences_are_refused(self):
        for source in ['| a | b |\n| --- | --- |\n| one |\n', '```sh\necho nope\n', '- \n']:
            with self.subTest(source=source), self.assertRaises(ValueError):
                builder.body(source, 'en')

    def test_stale_output_check_never_rewrites_a_file(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            for route, content in builder.render(TOOLS.parent).items():
                target = root / 'site' / route
                target.parent.mkdir(parents=True, exist_ok=True)
                target.write_bytes(content)
            self.assertTrue(builder.outputs_match(root, builder.render(TOOLS.parent)))
            target = root / 'site/guide/index.html'
            target.write_bytes(b'outdated')
            self.assertFalse(builder.outputs_match(root, builder.render(TOOLS.parent)))
            self.assertEqual(target.read_bytes(), b'outdated')


if __name__ == '__main__':
    unittest.main()
