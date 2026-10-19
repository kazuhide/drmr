"""Behavioral checks for the deliberately bounded guide publisher."""
import importlib.util
from pathlib import Path
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
            self.assertIn('role="group" aria-labelledby="example-1-title"', html)
            self.assertIn('<caption id="table-1-title">', html)
            self.assertIn('scope="row"', html)
            for chapter in builder.CHAPTER_IDS:
                self.assertIn(f'href="#{chapter}"', html)
                other = "../ja/guide/" if route == "guide/index.html" else "../../guide/"
                self.assertIn(f'href="{other}#{chapter}"', html)
            self.assertNotIn('<script', html)

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
