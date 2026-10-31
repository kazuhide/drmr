"""Check complete example files and their clear/encrypted command boundaries."""
import importlib.util
import json
from pathlib import Path
import re
import shlex
import unittest


TOOLS = Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location("conversion_examples", TOOLS / "conversion_examples.py")
examples = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(examples)
OUTPUTS = {
    "mp4": ("clear", "cenc", "cbcs"),
    "dash": ("clear", "cenc", "cbcs"),
    "hls": ("clear", "cenc", "cbcs", "hls-aes128"),
    "hls-ts": ("clear", "hls-aes128", "hls-sample-aes"),
}


class ConversionExampleTests(unittest.TestCase):
    def test_complete_key_files_have_exact_input_and_output_roles(self):
        directory = TOOLS.parent / "docs/examples/keys"
        expected = {f"{format}-{target}-from-{mode}.json"
                    for format, targets in OUTPUTS.items() for target in targets
                    for mode in (("cenc-cbcs", "hls-aes128", "hls-sample-aes", "clear")
                                 if target != "clear" else ("cenc-cbcs", "hls-aes128", "hls-sample-aes"))}
        self.assertEqual({path.name for path in directory.glob("*.json")}, expected)
        for name in expected:
            with self.subTest(name=name):
                format, rest = next((format, name[len(format) + 1:])
                                    for format in sorted(OUTPUTS, key=len, reverse=True)
                                    if name.startswith(format + "-"))
                target, mode = rest[:-5].split("-from-")
                config = json.loads((directory / name).read_text(encoding="utf-8"))
                self.assertEqual(set(config), {"schema_version", "outputs"} | ({"input"} if mode != "clear" else set()))
                self.assertEqual(config["schema_version"], 2)
                if mode == "cenc-cbcs":
                    self.assertEqual(config["input"], {"keys": [{
                        "kid": "000102030405060708090a0b0c0d0e0f",
                        "key": "00112233445566778899aabbccddeeff"}]})
                elif mode != "clear":
                    key = {"uri": "https://example.com/keys/content.key",
                           "key": "00112233445566778899aabbccddeeff"}
                    if mode == "hls-sample-aes":
                        key.update(uri="skd://source-content", iv="000102030405060708090a0b0c0d0e0f")
                    self.assertEqual(config["input"], {"hls_keys": [key]})
                if target == "clear":
                    self.assertEqual(config["outputs"], {})
                    continue
                key = {"key": "ffeeddccbbaa99887766554433221100"}
                if target in ("cenc", "cbcs"):
                    key["kid"] = "101112131415161718191a1b1c1d1e1f"
                if format in ("hls", "hls-ts"):
                    key["hls_key_uri"] = "https://keys.example.invalid/content.key"
                if target == "hls-sample-aes":
                    key["iv"] = "000102030405060708090a0b0c0d0e0f"
                self.assertEqual(config["outputs"], {format: key})
        self.assertEqual(examples.key_artifacts(),
                         {name: (directory / name).read_text(encoding="utf-8") for name in expected})

    def test_direct_commands_omit_keys_only_for_clear_to_clear(self):
        for source, format, target in examples.routes():
            extension = "mp4" if source == "mp4" else "mpd" if source == "dash" else "m3u8"
            for protection in examples.INPUTS[source]["protections"]:
                with self.subTest(source=source, format=format, target=target, protection=protection):
                    command = examples.commands(source, format, target, protection)
                    expected = ["drmr", "convert", f"https://example.com/input.{extension}",
                                "--download-dir", "src", "--outputs", "outputs.json", "--output", "result"]
                    if protection != "clear" or target != "clear":
                        expected += ["--keys-file", "keys.json"]
                    self.assertEqual(shlex.split(command.replace("\\\n", "")), expected)

    def test_complete_output_files_define_all_thirteen_choices(self):
        expected = {f"{format}-{target}.json": {"schema_version": 3, "outputs": [
            {"id": format, "format": format, "target": target, "drms": []}]}
            for format, targets in OUTPUTS.items() for target in targets}
        directory = TOOLS.parent / "docs/examples/outputs"
        actual = {path.name: json.loads(path.read_text(encoding="utf-8")) for path in directory.iterdir()}
        self.assertEqual(actual, expected)
        self.assertEqual(examples.output_artifacts(),
                         {name: (directory / name).read_text(encoding="utf-8") for name in expected})

    def test_generated_commands_keep_readable_physical_line_width(self):
        commands = [examples.commands(source, format, target, protection)
                    for source, format, target in examples.routes()
                    for protection in examples.INPUTS[source]["protections"]]
        self.assertLessEqual(max(len(line) for command in commands for line in command.splitlines()), 64)
        for command in commands:
            lines = command.splitlines()
            self.assertEqual(len(lines), 3)
            self.assertTrue(all(line.endswith(" \\") for line in lines[:2]))
            self.assertFalse(lines[2].endswith("\\"))

    def test_both_languages_link_complete_files_with_matching_commands(self):
        values = []
        fence = chr(96) * 3
        for locale in ("en", "ja"):
            markdown = examples.markdown(locale)
            self.assertNotIn(fence + "json", markdown)
            blocks = re.findall("^" + fence + r"sh\n(.*?)^" + fence + "$", markdown, re.M | re.S)
            self.assertEqual(len(blocks), 65)
            self.assertEqual(markdown.count(" --outputs outputs.json "), 85)
            for operation in ("plan", "analyze", "verify"):
                self.assertNotIn("drmr " + operation, markdown)
            for (source, format, target), code in zip(examples.routes(), blocks):
                extension = "mp4" if source == "mp4" else "mpd" if source == "dash" else "m3u8"
                direct = (f'drmr convert "https://example.com/input.{extension}" \\\n'
                          '  --download-dir src --outputs outputs.json \\\n  --output result')
                expected = (direct + " --keys-file keys.json\n" if target != "clear" else
                            "# Clear input\n" + direct + "\n\n# Encrypted input\n" + direct + " --keys-file keys.json\n")
                self.assertEqual(code, expected)
            values.append(blocks)
        self.assertEqual(values[0], values[1])

    def test_all_encrypted_example_links_exist_and_match_source_key_type(self):
        expected_modes = {
            "mp4": ("cenc-cbcs",), "dash": ("cenc-cbcs",),
            "hls-fmp4": ("cenc-cbcs", "hls-aes128"),
            "hls-ts-video": ("hls-aes128", "hls-sample-aes"),
            "hls-ts-audio": ("hls-aes128", "hls-sample-aes"),
        }
        for source, modes in expected_modes.items():
            self.assertEqual(examples.input_key_modes(source), modes)
        for path in re.findall(r"https://github.com/kazuhide/drmr/blob/main/(docs/examples/(?:keys|outputs)/[^)]+)",
                               examples.markdown("en")):
            self.assertTrue((TOOLS.parent / path).is_file(), path)

    def test_unknown_profiles_are_refused(self):
        for action in (lambda: examples.markdown("fr"),
                       lambda: examples.commands("mp4", "hls-ts", "cenc", "clear"),
                       lambda: examples.commands("mp4", "mp4", "clear", "hls-aes128"),
                       lambda: examples.key_config("mp4", "clear", "../escape"),
                       lambda: examples.key_filename("mp4", "clear", "clear")):
            with self.assertRaises(ValueError):
                action()


if __name__ == "__main__":
    unittest.main()
