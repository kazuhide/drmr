"""Keep the public matrix's concrete, single-output command examples consistent."""


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


def routes():
    result = []
    for source in INPUTS:
        formats = ("dash", "hls", "hls-ts") if source.startswith("hls-ts-") else ("mp4", "dash", "hls")
        for output in formats:
            targets = (("clear", "hls-aes128", "hls-sample-aes") if output == "hls-ts" else
                       ("clear", "cbcs", "hls-aes128") if output == "hls" else
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
    name = {"hls-aes128": "aes128", "hls-sample-aes": "sample-aes"}.get(input_protection, input_protection)
    input_keys = " --keys-file keys.json" if input_protection != "clear" else ""
    runtime_keys = " --keys-file keys.json" if input_protection != "clear" or target != "clear" else ""
    destination = "result.mp4" if output == "mp4" else "result"
    lines = [f'drmr analyze "https://example.com/{name}.{extension}" --download-dir src{input_keys}',
             f"drmr plan {INPUTS[source]['source']} --to {target} --format {output} "
             f"--output {destination} --save plan.json{input_keys}",
             f"drmr convert --plan plan.json{runtime_keys}",
             f"drmr verify --plan plan.json{runtime_keys}"]
    return " &&\n".join(lines) + "\n"


def markdown(locale):
    if locale not in ("en", "ja"):
        raise ValueError("conversion-example-locale")
    preparation = "Setup" if locale == "en" else "準備"
    blocks = []
    for source, output, target in routes():
        heading = f"{INPUTS[source][locale]} → {FORMATS[output]} / {PROTECTIONS[target]}"
        preparation_link = f"[{preparation}](#recipe-setup)"
        blocks += [f"## {heading} {{#{identifier(source, output, target)}}}", "",
                   preparation_link, "", "```sh"]
        for protection in INPUTS[source]["protections"]:
            blocks += [f"# {PROTECTIONS[protection]} input", commands(source, output, target, protection).rstrip(), ""]
        blocks += ["```", ""]
    return "\n".join(blocks)
