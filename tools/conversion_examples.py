"""Keep the public matrix's concrete, single-output command examples consistent."""


INPUTS = {
    "mp4": {
        "en": "MP4", "ja": "MP4", "source": "./source.mp4",
        "note_en": "This example uses one video track in an MP4 file.",
        "note_ja": "この例では、映像 1 トラックの MP4 を使います。",
    },
    "dash": {
        "en": "DASH", "ja": "DASH", "source": "./source/manifest.mpd",
        "note_en": "The download list assumes one video representation, one initialization file and five two-second segments. For another MPD, save every referenced resource with its relative path; downloading only the MPD is insufficient.",
        "note_ja": "この取得一覧は、映像 1 トラック、初期化ファイル 1 個、2 秒のセグメント 5 個という構成の例です。別の MPD を使う場合は、その参照先をすべて相対パスどおりに保存してください。MPD だけでは変換できません。",
    },
    "hls-fmp4": {
        "en": "HLS / fMP4", "ja": "HLS / fMP4", "source": "./source/input.m3u8",
        "note_en": "Use a completed fMP4 media playlist with one video track. This also keeps MP4 output within the single-track profile; separate audio and video cannot be combined into one MP4.",
        "note_ja": "映像 1 トラックの、配信が完結した fMP4 メディアプレイリストを使います。MP4 出力も 1 トラックです。別々の音声と映像を一つの MP4 に結合する例ではありません。",
    },
    "hls-ts-video": {
        "en": "HLS / MPEG-TS (video)", "ja": "HLS / MPEG-TS（映像）",
        "source": "./source/input.m3u8",
        "note_en": "Use a completed MPEG-TS playlist with progressive H.264 video and optional 48 kHz AAC-LC audio. TS output uses separate audio and video playlists when both are present.",
        "note_ja": "プログレッシブ H.264 映像を含む、配信が完結した MPEG-TS プレイリストを使います。音声を含める場合は 48 kHz AAC-LC が対象です。音声と映像を TS で出力する場合は別々のプレイリストになります。",
    },
    "hls-ts-audio": {
        "en": "HLS / MPEG-TS (audio only)", "ja": "HLS / MPEG-TS（音声のみ）",
        "source": "./source/input.m3u8",
        "note_en": "Use a completed MPEG-TS playlist containing only 48 kHz AAC-LC audio. The output remains audio only.",
        "note_ja": "48 kHz AAC-LC 音声だけを含む、配信が完結した MPEG-TS プレイリストを使います。出力も音声のみです。",
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


def commands(source, output, target):
    workspace = identifier(source, output, target).removeprefix("recipe-")
    lines = ["(", "  set -eu", "  umask 077", f"  mkdir {workspace}", f"  cd {workspace}", ""]
    curl = "curl --fail --location --proto '=https' --proto-redir '=https'"
    if source == "mp4":
        lines += [f"  {curl} \\", "    --output ./source.mp4 \\",
                  '    "https://media.example.com/vod/mp4/video.mp4"', ""]
    elif source == "dash":
        lines += ["  mkdir -p source/video", "  for resource in manifest.mpd video/init.mp4 \\",
                  "    video/segment-1.m4s video/segment-2.m4s video/segment-3.m4s \\",
                  "    video/segment-4.m4s video/segment-5.m4s; do",
                  f"    {curl} \\", '      --output "source/$resource" \\',
                  '      "https://media.example.com/vod/dash/$resource"', "  done", ""]
    if source.startswith("hls-"):
        path = {"hls-fmp4": "hls-fmp4/video.m3u8", "hls-ts-video": "hls-ts/video.m3u8",
                "hls-ts-audio": "hls-ts-audio/audio.m3u8"}[source]
        lines += [f'  drmr analyze "https://media.example.com/vod/{path}" \\',
                  "    --download-dir ./source \\", "    --keys-file ../config/keys.json --json"]
    else:
        lines += [f"  drmr analyze {INPUTS[source]['source']} \\",
                  "    --keys-file ../config/keys.json --json"]
    destination = "./result.mp4" if output == "mp4" else "./result"
    lines += ["", f"  drmr plan {INPUTS[source]['source']} \\",
              f"    --to {target} --format {output} \\",
              f"    --output {destination} --save ./conversion.plan.json \\",
              "    --keys-file ../config/keys.json --json", "",
              "  drmr convert --plan ./conversion.plan.json \\",
              "    --keys-file ../config/keys.json --json", "",
              "  drmr verify --plan ./conversion.plan.json \\",
              "    --keys-file ../config/keys.json --json", ")"]
    return "\n".join(lines) + "\n"


def markdown(locale):
    if locale not in ("en", "ja"):
        raise ValueError("conversion-example-locale")
    preparation = "Prerequisites and key file" if locale == "en" else "実行前の準備と鍵ファイル"
    blocks = []
    for source, output, target in routes():
        heading = f"{INPUTS[source][locale]} → {FORMATS[output]} / {PROTECTIONS[target]}"
        preparation_link = f"[{preparation}](#recipe-setup)"
        blocks += [f"## {heading} {{#{identifier(source, output, target)}}}", "",
                   preparation_link + ("。" if locale == "ja" else ". ")
                   + INPUTS[source]["note_" + locale], "",
                   "```sh", commands(source, output, target).rstrip(), "```", ""]
    return "\n".join(blocks)
