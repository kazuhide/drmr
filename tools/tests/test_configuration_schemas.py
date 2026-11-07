"""Check editor schemas against the shared strict-parser corpus and public examples."""

import importlib.util
import json
from pathlib import Path
import unittest

from jsonschema import Draft7Validator


ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location("conversion_examples", ROOT / "tools/conversion_examples.py")
examples = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(examples)
SCHEMAS = {"outputs": "output-config-v4.schema.json", "keys": "named-keys-v2.schema.json"}
CORPUS = ROOT / "crates/drmr-core/tests/fixtures/configuration/corpus.json"


def schema_nodes(value):
    if isinstance(value, dict):
        yield value
        for child in value.values():
            yield from schema_nodes(child)
    elif isinstance(value, list):
        for child in value:
            yield from schema_nodes(child)


class ConfigurationSchemaTests(unittest.TestCase):
    def test_pages_serves_exact_schema_and_template_downloads(self):
        spec = importlib.util.spec_from_file_location("guide_builder", ROOT / "tools/build_guide_site.py")
        builder = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(builder)
        expected = {f"schemas/{name}" for name in SCHEMAS.values()} | {
            f"examples/templates/{name}-{kind}.json" for name in ("clear", "encrypted", "mixed4")
            for kind in ("outputs", "keys")}
        assets = builder.configuration_assets(ROOT)
        self.assertEqual(set(assets), expected)
        for route, content in assets.items():
            self.assertEqual((ROOT / "site" / route).read_bytes(), content)
        for filename in SCHEMAS.values():
            schema = json.loads(assets[f"schemas/{filename}"])
            self.assertEqual(schema["$id"], "https://kazuhide.github.io/drmr/schemas/" + filename)
        for filename, content in examples.template_artifacts().items():
            value = json.loads(content)
            self.assertEqual(value["$schema"], "https://kazuhide.github.io/drmr/schemas/" +
                             SCHEMAS["outputs" if filename.endswith("-outputs.json") else "keys"])

    def validators(self):
        result = {}
        for kind, filename in SCHEMAS.items():
            schema = json.loads((ROOT / "schemas" / filename).read_text(encoding="utf-8"))
            self.assertEqual(schema["$schema"], "http://json-schema.org/draft-07/schema#")
            Draft7Validator.check_schema(schema)
            self.assertTrue(all(node["$ref"].startswith("#/")
                                for node in schema_nodes(schema) if "$ref" in node))
            result[kind] = Draft7Validator(schema)
        return result

    def test_shared_rust_parser_corpus_has_explicit_schema_expectations(self):
        validators = self.validators()
        corpus = json.loads(CORPUS.read_text(encoding="utf-8"))
        self.assertEqual(corpus["schema_version"], 1)
        names = set()
        for case in corpus["cases"]:
            with self.subTest(case=case["name"]):
                self.assertNotIn(case["name"], names)
                names.add(case["name"])
                self.assertIsInstance(case["parser_valid"], bool)
                document = json.loads(case["json"]) if "json" in case else case["document"]
                self.assertEqual(validators[case["file"]].is_valid(document), case["schema_valid"])
                if case["schema_valid"] != case["parser_valid"]:
                    self.assertTrue(case["boundary"])
        self.assertGreaterEqual(len(names), 30)

    def test_all_published_examples_and_templates_validate(self):
        validators = self.validators()
        for kind, directory in (("outputs", "outputs"), ("keys", "keys")):
            for path in sorted((ROOT / "docs/examples" / directory).glob("*.json")):
                with self.subTest(path=path.name):
                    validators[kind].validate(json.loads(path.read_text(encoding="utf-8")))
        for path in sorted((ROOT / "docs/examples/templates").glob("*.json")):
            kind = "outputs" if path.name.endswith("-outputs.json") else "keys"
            with self.subTest(path=path.name):
                validators[kind].validate(json.loads(path.read_text(encoding="utf-8")))

    def test_generated_template_pairs_have_matching_roles_and_no_stale_files(self):
        expected = examples.template_artifacts()
        directory = ROOT / "docs/examples/templates"
        actual = {path.name: path.read_text(encoding="utf-8") for path in directory.glob("*.json")}
        self.assertEqual(set(expected), {f"{name}-{kind}.json"
                                        for name in ("clear", "encrypted", "mixed4")
                                        for kind in ("outputs", "keys")})
        self.assertEqual(actual, expected)
        for name, count in (("clear", 2), ("encrypted", 2), ("mixed4", 5)):
            outputs = json.loads(expected[f"{name}-outputs.json"])
            keys = json.loads(expected[f"{name}-keys.json"])
            self.assertEqual(len(outputs["outputs"]), count)
            self.assertEqual(set(keys["outputs"]),
                             {item["id"] for item in outputs["outputs"] if item["encryption"] != "clear"})
            self.assertNotIn("input", keys)
            self.assertTrue(outputs["$schema"].endswith("/schemas/output-config-v4.schema.json"))
            self.assertTrue(keys["$schema"].endswith("/schemas/named-keys-v2.schema.json"))
            self.assertEqual({item["output_format"] for item in outputs["outputs"]},
                             {"hls-fmp4", "dash", "hls-ts"} if name == "mixed4" else {"hls-fmp4", "dash"})
        mixed = json.loads(expected["mixed4-outputs.json"])["outputs"]
        self.assertEqual({(item["output_format"], item["encryption"], tuple(item["drms"]))
                          for item in mixed}, {
                              ("hls-fmp4", "clear", ()), ("hls-fmp4", "cbcs", ("fairplay",)),
                              ("hls-ts", "clear", ()), ("hls-ts", "hls-sample-aes", ("fairplay",)),
                              ("dash", "cbcs", ("widevine", "playready"))})


if __name__ == "__main__":
    unittest.main()
