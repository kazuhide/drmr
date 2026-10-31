"""Behavioral checks for the deliberately bounded guide publisher."""
import importlib.util
from html.parser import HTMLParser
import os
from pathlib import Path
import py_compile
import tempfile
import unittest
from unittest.mock import patch


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
            self.assertEqual(html.count('data-guide-chapter='), 18)
            self.assertIn('<figure class=guide-example aria-labelledby=e1 data-code-language=json>', html)
            self.assertNotIn('role="group"', html)
            self.assertIn('<caption id=t1>', html)
            self.assertIn('scope=row', html)
            for chapter in builder.CHAPTER_IDS:
                self.assertIn(f'href="#{chapter}"', html)
                other = "../ja/guide/" if route == "guide/index.html" else "../../guide/"
                self.assertIn(f'href={other}#{chapter}', html)
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
        self.assertEqual(output, '<h3 id=recipe-mp4-dash-cbcs tabindex=-1>MP4 → DASH / cbcs</h3>')

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

    def test_fenced_code_keeps_shell_quotes_literal_and_escapes_markup(self):
        command = 'drmr analyze "https://example.com/input.mp4?a=1&b=2"'
        hostile = "'</code><script>bad</script>&quot;"
        output = builder.body('```sh\n' + command + '\n' + hostile + '\n```', 'en')
        self.assertIn('drmr analyze "https://example.com/input.mp4?a=1&amp;b=2"', output)
        self.assertIn("'&lt;/code&gt;&lt;script&gt;bad&lt;/script&gt;&amp;quot;", output)
        self.assertNotIn('<script>', output)
        self.assertEqual(output.count('</code>'), 1)

    def test_compact_code_retains_literal_conjunctions_and_hostile_entity_text(self):
        code = 'drmr --version &&\nprintf \'&&quot; &notit; <script>bad</script>\'\n'
        output = builder.body('```sh\n' + code + '```', 'en')

        class CodeText(HTMLParser):
            def __init__(self):
                super().__init__(convert_charrefs=True)
                self.inside = False
                self.code = ''
                self.tags = []

            def handle_starttag(self, tag, attrs):
                self.tags.append(tag)
                if tag == 'code':
                    self.inside = True

            def handle_endtag(self, tag):
                if tag == 'code':
                    self.inside = False

            def handle_data(self, text):
                if self.inside:
                    self.code += text

        parser = CodeText()
        parser.feed(output)
        self.assertEqual(parser.code, code)
        self.assertNotIn('script', parser.tags)
        self.assertIn(' &&\n', output)
        self.assertIn('&amp;&amp;quot;', output)

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

    def test_compact_body_preserves_the_complete_element_tree_text_and_attributes(self):
        spec = importlib.util.spec_from_file_location("guide_semantics", TOOLS / "check_site.py")
        checker = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(checker)
        markup = ('<section id="sample"><p>One <a href="#sample">link</a>.</p><p>Two.</p>'
                  '<ul><li>First</li><li>Second</li></ul>'
                  '<table><thead><tr><th scope="col">A</th><th scope="col">B</th></tr></thead>'
                  '<tbody><tr><th scope="row">Key</th><td>Value</td></tr>'
                  '<tr><th scope="row">Other</th><td>Last</td></tr></tbody></table>'
                  '<p>Before code.</p><figure class="guide-example" aria-labelledby="e1" data-code-language="sh">'
                  '<figcaption id="e1">Example 1: sh</figcaption><pre tabindex="0"><code>'
                  'printf \'&lt;/td&gt; &lt;/p&gt;\' &amp;&amp;\n  drmr --version\n'
                  '</code></pre></figure><p>Last.</p></section>')
        compact = builder.compact_body(markup)

        def tree(node):
            if isinstance(node, str):
                return node
            return node.tag, node.attrs, [tree(child) for child in node.children]

        self.assertEqual(tree(checker.MatrixMarkup(compact).root), tree(checker.MatrixMarkup(markup).root))
        self.assertLess(len(compact), len(markup))
        self.assertNotIn('</td>', compact)
        self.assertIn('printf \'&lt;/td&gt; &lt;/p&gt;\' &amp;&amp;\n  drmr --version\n', compact)

    def test_both_complete_guides_have_identical_semantics_before_and_after_compaction(self):
        spec = importlib.util.spec_from_file_location("guide_serialization", TOOLS / "check_site.py")
        checker = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(checker)
        compact = builder.render(TOOLS.parent)
        with patch.object(builder, "compact_body", side_effect=lambda markup: markup):
            explicit = builder.render(TOOLS.parent)

        def tree(node):
            if isinstance(node, str):
                return node
            return node.tag, node.attrs, [tree(child) for child in node.children]

        for route in compact:
            with self.subTest(route=route):
                self.assertEqual(tree(checker.MatrixMarkup(compact[route].decode()).root),
                                 tree(checker.MatrixMarkup(explicit[route].decode()).root))
                self.assertLess(len(compact[route]), len(explicit[route]))

    def test_compaction_does_not_omit_endings_before_inline_text_or_other_elements(self):
        markup = '<section><p>First.</p>tail<a href="#x">link</a><p>Second.</p><code>x</code></section>'
        self.assertEqual(builder.compact_body(markup), markup.replace('href="#x"', 'href=#x'))


if __name__ == '__main__':
    unittest.main()
