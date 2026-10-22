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
        input_profiles = {
            "mp4": ("clear", "cenc", "cbcs"), "dash": ("clear", "cenc", "cbcs"),
            "hls-fmp4": ("clear", "cenc", "cbcs", "hls-aes128"),
            "hls-ts-video": ("clear", "hls-aes128", "hls-sample-aes"),
            "hls-ts-audio": ("clear", "hls-aes128", "hls-sample-aes"),
        }
        checked = 0
        for source, output, target in EXAMPLES.routes():
            for protection in input_profiles[source]:
                with self.subTest(source=source, output=output, target=target, protection=protection):
                    code = EXAMPLES.commands(source, output, target, protection)
                    self.assertTrue(all(len(line) <= 72 for line in code.splitlines()))
                    lines = re.sub(r"\\\n[ \t]*", " ", code).splitlines()
                    self.assertEqual(len(lines), 4)
                    commands = [shlex.split(line) for line in lines]
                    self.assertEqual([command[1] for command in commands], ["analyze", "plan", "convert", "verify"])
                    self.assertTrue(all(command[-1] == "&&" for command in commands[:3]))
                    self.assertNotIn("&&", commands[3])
                    for index, command in enumerate(commands):
                        if index < 3:
                            command.pop()
                        needs_keys = protection != "clear" or (index >= 2 and target != "clear")
                        self.assertEqual("--keys-file" in command, needs_keys)
                        if needs_keys:
                            self.assertEqual(command[command.index("--keys-file") + 1], "keys.json")
                        self.assertNotIn("--json", command)
                    plan = commands[1]
                    for flag, value in (("--to", target), ("--format", output), ("--save", "plan.json"),
                                        ("--output", "result.mp4" if output == "mp4" else "result")):
                        self.assertEqual(plan[plan.index(flag) + 1], value)
                    for command in commands[2:]:
                        self.assertEqual(command[command.index("--plan") + 1], "plan.json")
                        self.assertNotIn("--to", command)
                    extension = "mp4" if source == "mp4" else "mpd" if source == "dash" else "m3u8"
                    name = {"hls-aes128": "aes128", "hls-sample-aes": "sample-aes"}.get(protection, protection)
                    self.assertEqual(commands[0][2], f"https://example.com/{name}.{extension}")
                    self.assertIn(f'analyze "https://example.com/{name}.{extension}"', code.splitlines()[0])
                    self.assertEqual(commands[0][commands[0].index("--download-dir") + 1], "src")
                    self.assertEqual(plan[2], "src/input." + extension)
                    checked += 1
        self.assertEqual(sum(map(len, input_profiles.values())), 16)
        self.assertEqual(checked, 144)

    def test_locales_retain_identical_commands_and_recipe_identifiers(self):
        en, ja = (EXAMPLES.markdown(locale) for locale in ("en", "ja"))
        self.assertEqual(re.findall(r"\{#(recipe-[a-z0-9-]+)\}", en), re.findall(r"\{#(recipe-[a-z0-9-]+)\}", ja))
        self.assertEqual(re.findall(r"```sh\n(.*?)```", en, re.S), re.findall(r"```sh\n(.*?)```", ja, re.S))
        self.assertEqual(en.count("```sh"), 45)
        self.assertEqual(len(re.findall(r"^# .+ input$", en, re.M)), 144)
        self.assertNotEqual(en, ja)

    def test_unsupported_routes_and_locales_are_refused(self):
        for route in (("hls-ts-video", "mp4", "clear"), ("mp4", "hls", "cenc"),
                      ("mp4; exit", "mp4", "clear")):
            with self.subTest(route=route), self.assertRaises(ValueError):
                EXAMPLES.commands(*route, "clear")
        for source, protection in (("mp4", "hls-aes128"), ("dash", "hls-sample-aes"),
                                   ("hls-fmp4", "hls-sample-aes"), ("hls-ts-video", "cenc"),
                                   ("hls-ts-audio", "cbcs"), ("mp4", "clear; exit")):
            with self.subTest(source=source, protection=protection), self.assertRaises(ValueError):
                EXAMPLES.commands(source, "dash", "clear", protection)
        with self.assertRaises(ValueError):
            EXAMPLES.markdown("unknown")


if __name__ == "__main__":
    unittest.main()
