"""Generate complete configuration examples for the public conversion matrix."""

import argparse
import json
from pathlib import Path


INPUTS = {
    "mp4": {
        "en": "MP4", "ja": "MP4", "source": "src/input.mp4",
        "protections": ("clear", "cenc", "cbcs"),
    },
    "dash": {
        "en": "DASH", "ja": "DASH", "source": "src/input.mpd",
        "protections": ("clear", "cenc", "cbcs"),
    },
    "hls-fmp4": {
        "en": "HLS / fMP4", "ja": "HLS / fMP4", "source": "src/input.m3u8",
        "protections": ("clear", "cenc", "cbcs", "hls-aes128"),
    },
    "hls-ts-video": {
        "en": "HLS / MPEG-TS (video)", "ja": "HLS / MPEG-TS（映像）",
        "source": "src/input.m3u8",
        "protections": ("clear", "hls-aes128", "hls-sample-aes"),
    },
    "hls-ts-audio": {
        "en": "HLS / MPEG-TS (audio only)", "ja": "HLS / MPEG-TS（音声のみ）",
        "source": "src/input.m3u8",
        "protections": ("clear", "hls-aes128", "hls-sample-aes"),
    },
}
FORMATS = {"mp4": "MP4", "dash": "DASH", "hls": "HLS / fMP4", "hls-ts": "HLS / MPEG-TS"}
PROTECTIONS = {"clear": "Clear", "cenc": "cenc", "cbcs": "cbcs",
               "hls-aes128": "AES-128", "hls-sample-aes": "SAMPLE-AES"}
SOURCE_KID = "000102030405060708090a0b0c0d0e0f"
SOURCE_KEY = "00112233445566778899aabbccddeeff"
OUTPUT_KID = "101112131415161718191a1b1c1d1e1f"
OUTPUT_KEY = "ffeeddccbbaa99887766554433221100"
IV = "000102030405060708090a0b0c0d0e0f"
KEY_MODES = ("cenc-cbcs", "hls-aes128", "hls-sample-aes")
EXAMPLE_ROOT = "https://github.com/kazuhide/drmr/blob/main/docs/examples/keys/"
OUTPUT_EXAMPLE_ROOT = "https://github.com/kazuhide/drmr/blob/main/docs/examples/outputs/"


def routes():
    result = []
    for source in INPUTS:
        formats = ("mp4", "dash", "hls", "hls-ts")
        for output in formats:
            targets = (("clear", "hls-aes128", "hls-sample-aes") if output == "hls-ts" else
                       ("clear", "cenc", "cbcs", "hls-aes128") if output == "hls" else
                       ("clear", "cenc", "cbcs"))
            result.extend((source, output, target) for target in targets)
    return tuple(result)


def identifier(source, output, target):
    if (source, output, target) not in routes():
        raise ValueError("conversion-example-route")
    return f"recipe-{source}-{output}-{target}"


def commands(source, output, target, input_protection):
    identifier(source, output, target)
    if input_protection not in INPUTS[source]["protections"]:
        raise ValueError("conversion-example-input-protection")
    extension = INPUTS[source]["source"].rsplit(".", 1)[1]
    runtime_keys = " --keys-file keys.json" if input_protection != "clear" or target != "clear" else ""
    return (f'drmr convert "https://example.com/input.{extension}" \\\n'
            f'  --download-dir src --outputs outputs.json --output result{runtime_keys}\n')


def output_spec(output, target):
    if not any(route[1:] == (output, target) for route in routes()):
        raise ValueError("conversion-example-output")
    return {"schema_version": 3, "outputs": [
        {"id": output, "format": output, "target": target, "drms": []}]}


def key_config(output, target, mode="clear"):
    output_spec(output, target)
    if mode not in ("clear",) + KEY_MODES:
        raise ValueError("conversion-example-key-mode")
    config = {"schema_version": 2}
    if mode == "cenc-cbcs":
        config["input"] = {"keys": [{"kid": SOURCE_KID, "key": SOURCE_KEY}]}
    elif mode != "clear":
        key = {"uri": "https://example.com/keys/content.key", "key": SOURCE_KEY}
        if mode == "hls-sample-aes":
            key.update(uri="skd://source-content", iv=IV)
        config["input"] = {"hls_keys": [key]}
    config["outputs"] = {}
    if target != "clear":
        key = {"key": OUTPUT_KEY}
        if target in ("cenc", "cbcs"):
            key = {"kid": OUTPUT_KID, **key}
        if output in ("hls", "hls-ts"):
            key["hls_key_uri"] = "https://keys.example.invalid/content.key"
        if target == "hls-sample-aes":
            key["iv"] = IV
        config["outputs"][output] = key
    return config


def key_filename(output, target, mode):
    key_config(output, target, mode)
    if mode == "clear" and target == "clear":
        raise ValueError("conversion-example-key-mode")
    return f"{output}-{target}-from-{mode}.json"


def input_key_modes(source):
    modes = []
    profiles = INPUTS[source]["protections"]
    if "cenc" in profiles:
        modes.append("cenc-cbcs")
    modes.extend(mode for mode in KEY_MODES[1:] if mode in profiles)
    return tuple(modes)


def json_text(value):
    return json.dumps(value, indent=2) + "\n"


def key_artifacts():
    return {key_filename(output, target, mode): json_text(key_config(output, target, mode))
            for _, output, target in routes()
            for mode in (("clear",) + KEY_MODES if target != "clear" else KEY_MODES)}


def output_artifacts():
    return {f"{output}-{target}.json": json_text(output_spec(output, target))
            for _, output, target in routes()}



def entrypoint(output):
    return f"result/{output}/" + ({"mp4": "media.mp4", "dash": "manifest.mpd"}
                                  .get(output, "master.m3u8"))


def markdown(locale):
    if locale not in ("en", "ja"):
        raise ValueError("conversion-example-locale")
    preparation = "Setup" if locale == "en" else "準備"
    blocks = []
    for source, output, target in routes():
        heading = f"{INPUTS[source][locale]} → {FORMATS[output]} / {PROTECTIONS[target]}"
        output_link = f"[outputs.json]({OUTPUT_EXAMPLE_ROOT}{output}-{target}.json)"
        blocks += [f"## {heading} {{#{identifier(source, output, target)}}}", "",
                   f"[{preparation}](#recipe-setup)", "", output_link, ""]
        if target == "clear":
            blocks += [("Clear input: no `keys.json`." if locale == "en" else
                        "Clear 入力: `keys.json` は不要です。"), ""]
        else:
            link = f"[keys.json]({EXAMPLE_ROOT}{key_filename(output, target, 'clear')})"
            blocks += [(f"Clear input: {link}." if locale == "en" else
                        f"Clear 入力: {link}。"), ""]
        labels = {"cenc-cbcs": "cenc / cbcs", "hls-aes128": "AES-128", "hls-sample-aes": "SAMPLE-AES"}
        links = " / ".join(f"[{labels[mode]}]({EXAMPLE_ROOT}{key_filename(output, target, mode)})"
                           for mode in input_key_modes(source))
        command = commands(source, output, target, "clear").rstrip()
        if target == "clear":
            command = ("# Clear input\n" + command + "\n\n# Encrypted input\n"
                       + commands(source, output, target, INPUTS[source]["protections"][1]).rstrip())
        blocks += [(f"Encrypted input: {links} → `keys.json`." if locale == "en"
                    else f"暗号化入力の `keys.json`: {links}。"), "",
                   "```sh", command, "```", "",
                   (f"Output: `{entrypoint(output)}`." if locale == "en" else
                    f"出力: `{entrypoint(output)}`。"), ""]
    return "\n".join(blocks)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--check", action="store_true", help="Refuse stale complete configuration examples")
    args = parser.parse_args()
    root = Path(__file__).resolve().parents[1] / "docs/examples"
    for name, expected in (("keys", key_artifacts()), ("outputs", output_artifacts())):
        directory = root / name
        if args.check:
            actual = {path.name: path.read_text(encoding="utf-8") for path in directory.glob("*.json")}
            if actual != expected:
                parser.exit(1, "conversion-settings-examples-stale\n")
        else:
            directory.mkdir(parents=True, exist_ok=True)
            for filename, content in expected.items():
                (directory / filename).write_text(content, encoding="utf-8")


if __name__ == "__main__":
    main()
