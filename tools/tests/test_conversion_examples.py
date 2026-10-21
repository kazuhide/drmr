"""Check the public examples' route coverage and CLI intent."""
import importlib.util
from pathlib import Path
import re
import shlex
import unittest


TOOLS = Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location("conversion_examples", TOOLS / "conversion_examples.py")
EXAMPLES = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(EXAMPLES)


class ConversionExamples(unittest.TestCase):
    def test_exact_supported_route_inventory(self):
        expected = set()
        for source in ("mp4", "dash", "hls-fmp4", "hls-ts-video", "hls-ts-audio"):
            outputs = ("dash", "hls", "hls-ts") if source in ("hls-ts-video", "hls-ts-audio") else ("mp4", "dash", "hls")
            for output in outputs:
                targets = {"mp4": ("clear", "cenc", "cbcs"), "dash": ("clear", "cenc", "cbcs"),
                           "hls": ("clear", "cbcs", "hls-aes128"),
                           "hls-ts": ("clear", "hls-aes128", "hls-sample-aes")}[output]
                expected.update((source, output, target) for target in targets)
        self.assertEqual(set(EXAMPLES.routes()), expected)
        self.assertEqual(len(EXAMPLES.routes()), 45)
        self.assertEqual(len({EXAMPLES.identifier(*route) for route in expected}), 45)

    def test_every_example_has_four_matching_cli_operations(self):
        for source, output, target in EXAMPLES.routes():
            with self.subTest(source=source, output=output, target=target):
                code = EXAMPLES.commands(source, output, target)
                commands = [shlex.split(line.strip()) for line in code.replace("\\\n", "").splitlines()
                            if line.strip().startswith("drmr ")]
                self.assertEqual([command[1] for command in commands], ["analyze", "plan", "convert", "verify"])
                plan = commands[1]
                for flag, value in (("--to", target), ("--format", output), ("--save", "./conversion.plan.json"),
                                    ("--output", "./result.mp4" if output == "mp4" else "./result")):
                    self.assertEqual(plan[plan.index(flag) + 1], value)
                for command in commands:
                    self.assertEqual(command[command.index("--keys-file") + 1], "../config/keys.json")
                    self.assertEqual(command[-1], "--json")
                for command in commands[2:]:
                    self.assertEqual(command[command.index("--plan") + 1], "./conversion.plan.json")
                    self.assertNotIn("--to", command)
                if source.startswith("hls-"):
                    self.assertTrue(commands[0][2].startswith("https://media.example.com/vod/"))
                    self.assertIn("--download-dir", commands[0])
                    self.assertEqual(plan[2], "./source/input.m3u8")
                else:
                    self.assertFalse(commands[0][2].startswith("https://"))
                    self.assertNotIn("--download-dir", commands[0])
                    self.assertIn("curl --fail --location", code)

    def test_locales_retain_identical_commands_and_recipe_identifiers(self):
        en, ja = (EXAMPLES.markdown(locale) for locale in ("en", "ja"))
        self.assertEqual(re.findall(r"\{#(recipe-[a-z0-9-]+)\}", en), re.findall(r"\{#(recipe-[a-z0-9-]+)\}", ja))
        self.assertEqual(re.findall(r"```sh\n(.*?)```", en, re.S), re.findall(r"```sh\n(.*?)```", ja, re.S))
        self.assertEqual(en.count("```sh"), 45)
        self.assertNotEqual(en, ja)

    def test_unsupported_routes_and_locales_are_refused(self):
        for route in (("hls-ts-video", "mp4", "clear"), ("mp4", "hls", "cenc"),
                      ("mp4; exit", "mp4", "clear")):
            with self.subTest(route=route), self.assertRaises(ValueError):
                EXAMPLES.commands(*route)
        with self.assertRaises(ValueError):
            EXAMPLES.markdown("unknown")


if __name__ == "__main__":
    unittest.main()
