---
title: User guide
updated: 2026-10-20
version: 0.1.0
---

# 1. Introduction

drmr changes media containers, delivery formats and encryption while retaining encoded video and audio. It is for engineers who control the media and already have the required decryption keys. It is not an encoder for creating a new quality ladder or a client for obtaining DRM licenses.

This guide describes drmr 0.1.0 and was updated on 2026-10-20. Public source and command help define the behavior. For a different version, also read its accompanying documentation. A passing example does not qualify arbitrary media or playback devices.

## Choose an operation

| Goal | Command | What it establishes |
| --- | --- | --- |
| Find external tools | `doctor` | Required executables can be found in the launch environment |
| Check versions and options | `doctor --probe` | Version and help output advertise required features; no media is processed |
| Read input declarations | `inspect` | Local MP4, DASH or HLS metadata; no decryption or complete sample proof |
| Check complete source media | `analyze` | Actual input admission; URL input is retained in an explicit download directory |
| Describe a conversion | `plan` | Requested formats and protection, candidate routes and runtime conditions |
| Save a conversion request | `plan --save` | Source file fingerprints, one output format, protection and destination |
| Convert media | `convert` | Actual input admission, conversion and comparison before publishing new output |
| Compare independently | `verify` | Reads original and result independently and checks complete media and timing |

Use `analyze`, `plan --save`, `convert --plan` and `verify --plan` to check the source, save the conversion request, execute it and compare the result. `inspect` remains a quick metadata check. Direct `convert INPUT` also remains available; generic HLS conversion does not require an earlier `analyze-hls` step.

## First local example

Start with a supported unencrypted local MP4. Create the output parent directory first and choose an output name that does not exist. The original is retained unchanged.

```sh
drmr --version
drmr doctor --probe --route progressive-mp4
drmr inspect original.mp4 --json
drmr analyze original.mp4 --json
drmr plan original.mp4 --to clear --format mp4 \
  --output clear.mp4 --save clear.plan.json --json
drmr convert --plan clear.plan.json --json
drmr verify --plan clear.plan.json --json
```

Encrypted input or output requires the private key configuration described in chapter 5. Unsupported media is refused even in this simple example. Input checks and a saved plan do not guarantee a later conversion: source bytes, runtime keys, tools and the requested output are checked again when used.

The CLI is the main interface. Docker provides an option with media tools included. The experimental local Web console and Desktop host serve specific prepared workflows; they are not general file-conversion screens exposing every CLI capability.

# 2. Formats, encryption and DRM

## Containers and delivery

| Term | Meaning |
| --- | --- |
| MP4 | A container for encoded media and the metadata needed to interpret it |
| Progressive MP4 | An MP4 with sample location and timing tables, commonly distributed as one file |
| fMP4 | Fragmented MP4: initialization metadata followed by media fragments; usable in a file or segmented delivery |
| MPEG-TS | A transport-stream container using 188-byte packets; abbreviated TS here |
| DASH | Delivery described by an MPD manifest referencing initialization data and media |
| HLS | Delivery described by playlists; drmr processes completed on-demand presentations |
| Segment | A portion of media prepared for delivery; audio and video boundaries need not coincide |
| Track and rendition | A track is an audio or video sequence; a rendition is an available quality or language variant |

HLS can contain TS or fMP4. `--format hls` selects fMP4 HLS output; `--format hls-ts` selects TS HLS. Knowing that an input is HLS does not establish its media container.

## Select protection separately

AES is a symmetric encryption algorithm. CTR means counter mode; CBC means cipher block chaining. A sample is an encoded unit such as a video picture or audio frame.

| `--to` | Encryption unit | Output choices |
| --- | --- | --- |
| `clear` | No encryption | Supported MP4, DASH, fMP4 HLS and TS HLS profiles |
| `cenc` | AES-CTR applied to MP4 samples | MP4 and DASH |
| `cbcs` | AES-CBC sample encryption, including a supported video block pattern | MP4, DASH and fMP4 HLS |
| `hls-aes128` | AES-CBC applied to complete HLS resources | fMP4 HLS and TS HLS |
| `hls-sample-aes` | HLS sample encryption inside supported TS media | TS HLS |

AES-128 HLS is not an MP4 sample-protection scheme. `cens` and `cbc1` may be identified but are not conversion targets. Not every format/protection combination is supported.

## DRM systems are a separate layer

Widevine, PlayReady and FairPlay involve signaling, key exchange and device behavior beyond selecting an AES mode. A KID identifies a key; it is not the secret content key. A PSSH is DRM initialization metadata, not a license.

Generic `convert` uses supplied keys. Its explicit PlayReady signaling profile is limited to CBCS DASH. The separate saved-HLS workflow supports its own signaling configuration. SAMPLE-AES TS can carry FairPlay-oriented HLS signaling, but successful known-key verification does not prove a license service or device will play it.

# 3. Supported input and output profiles

## Select the container

These combinations describe generic `convert` for completed local media. Codec, track, clock and encryption-layout conditions still apply. URL acquisition has additional restrictions.

| Input | MP4 output | DASH output | fMP4 HLS output | TS HLS output |
| --- | --- | --- | --- | --- |
| MP4 | clear / cenc / cbcs; joint tracks conditional | clear / cenc / cbcs | clear / cbcs / AES-128 | Outside profile |
| DASH / fMP4 | clear / cenc / cbcs; one track | clear / cenc / cbcs | clear / cbcs / AES-128 | Outside profile |
| HLS / fMP4 | clear / cenc / cbcs; one track | clear / cenc / cbcs | clear / cbcs / AES-128 | Outside profile |
| HLS / TS with video | Outside profile | clear / cenc / cbcs | clear / cbcs / AES-128 | clear / AES-128 / SAMPLE-AES |
| HLS / TS, audio only | Outside profile | clear / cenc / cbcs | clear / cbcs / AES-128 | clear / AES-128 / SAMPLE-AES |

Joint MP4 output is limited to the qualified tracks from one progressive source described below. Separate DASH/HLS tracks or unrelated files are not combined into one MP4.

## HLS input protection

| Container | Input protection | Required source keys |
| --- | --- | --- |
| fMP4 | clear, cbcs, AES-128 | cbcs by KID; AES-128 by the exact playlist key URI |
| TS | clear, AES-128, SAMPLE-AES | Encrypted inputs by key URI; SAMPLE-AES also needs its associated IV |

Each fMP4 media playlist must contain one video or audio track. A master can associate separate video and audio playlists. Unsupported rendition metadata and inconsistent audio-group associations are refused.

## Finite audio presentation windows

This is an MP4 playback-metadata profile, not a separate codec. An edit list can specify the exact decoded-audio start and duration, excluding encoder warm-up and final padding.

The complete progressive source must contain one AAC-LC audio track (`mp4a`), optionally with one H.264 video track (`avc1`). The audio edit must be one normal-rate continuous window, have a nonnegative start and convert exactly into integer audio clock units.

| Input protection | MP4 output | DASH output | fMP4 HLS output |
| --- | --- | --- | --- |
| clear | clear / cenc / cbcs | clear / cenc / cbcs | clear / cbcs / AES-128 |
| cenc | clear / cenc / cbcs | clear / cenc / cbcs | clear / cbcs / AES-128 |
| cbcs | clear / cenc / cbcs | clear / cenc / cbcs | clear / cbcs / AES-128 |

Clear MP4 retains progressive sample tables. Encrypted MP4 is fragmented and can retain the two qualified tracks in one file. DASH/HLS uses separate tracks. Every encrypted source track needs its own key mapping.

Multiple edits, initial empty edits, rate changes, extra tracks, unrelated-file combinations, key rotation and input DRM initialization headers are refused by conversion. CENC accepts supported 8/16-byte sample-IV layouts; CBCS accepts supported 16-byte constant-IV and pattern layouts. This does not admit arbitrary encrypted MP4.

## TS admission

The TS profile uses one fixed program, one progressive H.264 video stream with optional 48 kHz AAC-LC audio. DASH, fMP4 HLS and TS output also accept audio-only input under the same single-program, 48 kHz AAC-LC conditions.

TS packets must be 188 bytes; H.264 decoder configuration must remain fixed. AAC uses seven-byte ADTS transport headers without a checksum and one raw data block. Multiple programs, configuration changes, corrupt packets, clock wrap, discontinuities, interlaced video, 44.1 kHz AAC and unsupported descriptors are refused.

SAMPLE-AES accepts one effective `identity` or `com.apple.streamingkeydelivery` key format, with version 1 when declared. Distinct simultaneously active key formats and unsupported method changes are refused. Nonzero audio priming is outside the profile. Joint TS output uses separate audio/video playlists and a master; it does not multiplex both into one output segment.

## Common exclusions and bounds

Local DASH requires one static Period, a finite continuous `SegmentTimeline` and supported confined relative references. Live/dynamic input, multiple periods, byte ranges, discontinuities and unsupported events or rendition metadata are refused. Generic conversion does not retain arbitrary subtitles; the saved-HLS workflow has a separate supported subtitle profile.

Generic conversion stages at most 16 GiB per track, 64 tracks and 100,000 DASH segment references. Metadata, fragment and resource-admission bounds also apply. Exceeding a bound fails rather than verifying a partial result.

See [conversion support](https://github.com/kazuhide/drmr/blob/main/docs/conversion-support.md) and [processing and verification](https://github.com/kazuhide/drmr/blob/main/docs/media-processing.md) for the complete current restrictions.

# 4. Installation and external tools

## Build the CLI

Source builds require Rust 1.95 or newer. The repository does not automatically switch toolchains. Versions used in tests are observations, not a requirement to use exactly that newer compiler. A prebuilt CLI archive does not require Rust to be installed.

From the repository root:

```sh
cargo install --path crates/drmr --locked
drmr --version
```

To build without installing:

```sh
cargo build --release --locked -p drmr
./target/release/drmr --help
```

`--locked` uses the dependency versions in `Cargo.lock`. `Cargo.toml` declares compatible dependency ranges; the lockfile records one concrete combination. Dependency updates still need minimum-Rust and stable qualification.

## Media tools

| Executable | Supported minimum | Purpose |
| --- | --- | --- |
| `ffmpeg` / `ffprobe` | 9.0.1 | Media reading/decryption, packet clocks and complete decoded data |
| `packager` | Shaka Packager 3.8.0 | Sample encryption, container changes and segmentation |
| `mp4dump` | Bento4 1.6.0, build 641 | MP4 structure inspection for prepared DASH and related workflows |

Obtain trusted tool distributions and put the executables on PATH. CLI archives and Desktop bundles do not include the media tools; the Docker image does. A newer version still needs the required options and complete packet clocks. Bento4's banner does not establish its build number: `build-unverified` is not proof of build 641 or newer.

## Discovery versus probing

```sh
drmr doctor
drmr doctor --probe --route all --json
drmr doctor --probe --route dash-presentation
```

Default `doctor` discovers paths without executing tools. `--probe` checks version and help output with a three-second budget per call and 1 MiB per stdout/stderr stream. Raw output is not included in the report. It does not validate media, keys, licenses or playback devices.

`--route` chooses the required tool set. `progressive-mp4` needs FFmpeg/ffprobe; `prepared-dash` and `all` also need Bento4. Other routes inspect Packager as needed. Unrelated tools may be optional, but unconfirmed process cleanup prevents readiness even for an optional probe. `ready` means the selected diagnostics passed; `media_compatibility_checked` stays false. Desktop and terminal launch environments may have different PATH values.

See [requirements](https://github.com/kazuhide/drmr/blob/main/docs/requirements.md) and [Windows](https://github.com/kazuhide/drmr/blob/main/docs/windows.md).

# 5. Key configuration and confidentiality

## Separate input and output roles

Store generic conversion keys in an owner-only JSON file outside output directories. On Unix, use an owner-only directory and mode 600. Do not commit real keys. Windows ACL preparation is in chapter 14.

`analyze` and `plan --save` also accept `--keys-file` when source checks need input keys. Saving a plan does not require an output key; `convert --plan` and `verify --plan` receive runtime keys separately.

```sh
umask 077
mkdir config
chmod 700 config
```

Create the private file before entering secret values, then protect and edit it:

```sh
chmod 600 config/keys.json
```

The examples below are complete schemas. Replace the angle-bracket placeholders with valid hexadecimal values. A KID is 16 bytes (32 hex digits); a content key is also 16 bytes. Invalid lengths, duplicate or unknown fields and unsafe files are refused.

## MP4 sample keys

For clear input and encrypted output:

```json
{
  "schema_version": 1,
  "output": {
    "kid": "<32-hex-output-kid>",
    "key": "<32-hex-output-key>"
  }
}
```

For encrypted input, list every source-track key under `input.keys`; the output key is a separate role:

```json
{
  "schema_version": 1,
  "input": {
    "keys": [
      {"kid": "<32-hex-video-kid>", "key": "<32-hex-video-key>"},
      {"kid": "<32-hex-audio-kid>", "key": "<32-hex-audio-key>"}
    ]
  },
  "output": {
    "kid": "<32-hex-output-kid>",
    "key": "<32-hex-output-key>"
  }
}
```

Source and result KIDs need not be the same. Independent `verify` still uses `input` for the original and `output` for the candidate. Do not swap roles to accommodate a failure. Existing encrypted verification can support profiles that conversion deliberately refuses, including some input PSSH metadata; that is not conversion permission.

## HLS key URI and IV

HLS input keys map the exact URI string from the playlist. drmr does not fetch a playlist key URI. For SAMPLE-AES input, associate the IV with the same input key entry, as in this example:

```json
{
  "schema_version": 1,
  "input": {
    "hls_keys": [{
      "uri": "skd://source-content",
      "key": "<32-hex-source-key>",
      "iv": "<32-hex-associated-iv>"
    }]
  }
}
```

For AES-128 HLS output, supply the output key and key URI:

```json
{
  "schema_version": 1,
  "output": {
    "key": "<32-hex-output-key>",
    "hls_key_uri": "https://keys.example.invalid/content.key"
  }
}
```

For SAMPLE-AES TS output, supply the common key, IV and HLS key URI:

```json
{
  "schema_version": 1,
  "output": {
    "key": "<32-hex-output-key>",
    "iv": "<32-hex-associated-iv>",
    "hls_key_uri": "skd://output-content"
  }
}
```

These fields belong under `output`; `input.hls_keys[].iv` belongs to its source key. The HLS URI is signaling and can appear in generated manifests. It is not an instruction for drmr to retrieve a key. An `skd://` URI alone does not establish FairPlay licensing or playback.

## Logs, memory and temporary files

Known key material is masked in retained tool logs and errors. Masking does not classify arbitrary filenames, URLs or metadata as secret. Keys are held in ordinary process memory. External tools can receive keys through process arguments/environment, visible to permitted operating-system inspection while running.

Some workflows create private temporary key/configuration files. Cleanup and zeroizing reduce retention but do not promise secure erasure, protection from the same user, crash-dump exclusion or hardware isolation. Treat original media, saved URLs, manifests and raw tool results as potentially sensitive. Local ClearKey playback intentionally delivers a usable key to the browser; chapter 15 explains that boundary.

See [local security](https://github.com/kazuhide/drmr/blob/main/docs/security.md).

# 6. Inspect and plan

## Inspect declarations

```sh
drmr inspect media.mp4
drmr inspect source/manifest.mpd --json
drmr inspect source/master.m3u8 --json
```

`inspect` reads local MP4, DASH and HLS declarations without keys, network acquisition or output media. It can identify a declared container, codec or protection scheme but does not establish complete sample validity. Unknown protection stays unknown; do not interpret missing evidence as clear.

## Analyze complete media

```sh
drmr analyze source/master.m3u8 --json
drmr analyze encrypted.mp4 --keys-file config/keys.json --json
```

`analyze` checks the actual complete source with the conversion engine, including encoded packets, decoded content and timestamps. Its `input_admitted` result applies to the checked input profile; candidate output formats still need a target-specific plan. It does not certify a converted result or DRM playback. Local MP4, DASH and HLS are accepted within their supported profiles. Chapter 8 shows URL input with mandatory source retention.

When a documented source compatibility rule applies, `compatibility_notes` lists its fixed code. Examples include empty TS padding and adjacent HLS cue markers with no active interval. These rules retain the original bytes and all media and timing checks. See [transport-stream compatibility](https://github.com/kazuhide/drmr/blob/main/docs/media-processing.md#transport-stream-compatibility) for the exact accepted conditions.

## Plan a request

```sh
drmr plan media.mp4 --to cbcs --format dash --json
drmr plan source/master.m3u8 --to clear --format dash,hls
```

Without `--save`, `plan` reports requested formats, key roles and candidate processing routes. Comma-separated formats describe alternatives, while conversion selects one. Every requested format must have a candidate. Unsupported or empty candidate sets are incompatible; source-profile admission remains a runtime condition. This metadata-only JSON is not an execution plan and is refused by `convert --plan`.

Use `--save` with a new result destination to check the complete source and save one resolved output format:

```sh
drmr plan source/input.m3u8 --to cbcs --format dash \
  --output converted-dash --save conversion.plan.json --json
drmr convert --plan conversion.plan.json --keys-file config/keys.json --json
drmr verify --plan conversion.plan.json --keys-file config/keys.json --json
```

The saved plan records the local playlist/media files used by conversion, their sizes and SHA-256 content fingerprints, the output format/protection and an absolute destination. Paths are tied to that machine and location; keep the plan private and outside source/output trees. Unknown versions or fields, inconsistent plan identifiers, changed source bytes and unsafe destinations are refused. The identifier detects inconsistency; it is not a signature or authorization from another party.

Keys, signed source URLs, authentication headers, license URLs, HLS key URIs and executable arguments are not stored in the plan. Supply keys at execution and verification with `--keys-file`. Output signaling also comes from that runtime configuration and is not fixed by the plan. `verify --plan` checks source fingerprints, actual output format/protection and independent complete media correspondence. It does not acquire a license or prove device playback.

Saving a plan creates no converted media. Encrypted source checks can decrypt with supplied input keys. Source admission is repeated when executing. The `plan --mpd` mode remains the separate prepared-DASH rendition listing described in chapter 11.

# 7. Local conversion examples

## Common rules

Choose a new destination with an existing safe parent. Generic conversion leaves the original unchanged and publishes only after verification and source/Stop checks. Existing output is refused rather than replaced. Use `--keys-file` when input or output protection requires keys.

Omitting `--format` follows the inspected MP4/DASH/HLS input category. HLS defaults to fMP4 HLS even when its input media is TS. Request `--format hls-ts` explicitly for TS output.

## MP4 to MP4 or DASH

```sh
drmr convert original.mp4 --to cenc --format mp4 \
  --output encrypted.mp4 --keys-file config/output.json
drmr verify encrypted.mp4 --source original.mp4 \
  --keys-file config/output.json --json
```

Encrypted output uses the separately configured output key. Joint MP4 remains limited to the qualified progressive source profile.

```sh
drmr convert original.mp4 --to cbcs --format dash \
  --output result-dash --keys-file config/output.json
drmr verify result-dash/manifest.mpd --source original.mp4 \
  --keys-file config/output.json --json
```

## Decrypt or re-encrypt DASH

```sh
drmr convert protected/manifest.mpd --to clear --format dash \
  --output clear-dash --keys-file config/source.json
drmr verify clear-dash/manifest.mpd \
  --source protected/manifest.mpd --keys-file config/source.json --json
```

```sh
drmr convert cenc-source/manifest.mpd --to cbcs --format dash \
  --output cbcs-result --keys-file config/both.json
drmr verify cbcs-result/manifest.mpd \
  --source cenc-source/manifest.mpd --keys-file config/both.json --json
```

Fresh verification points to the actual generated manifest, not just its directory. Input and output keys keep their roles even when protection changes.

## fMP4 HLS and transport-stream inputs

```sh
drmr convert source/master.m3u8 --to cbcs --format hls \
  --output cbcs-hls --keys-file config/hls-output.json
drmr verify cbcs-hls/master.m3u8 --source source/master.m3u8 \
  --keys-file config/hls-output.json --json
```

Create unencrypted or CBCS DASH from supported completed TS HLS:

```sh
drmr convert ts-source/media.m3u8 --to clear --format dash \
  --output clear-dash
drmr verify clear-dash/manifest.mpd --source ts-source/media.m3u8 --json
```

```sh
drmr convert ts-source/media.m3u8 --to cbcs --format dash \
  --output protected-dash --keys-file config/output.json
drmr verify protected-dash/manifest.mpd \
  --source ts-source/media.m3u8 --keys-file config/output.json --json
```

Absolute packet PTS, DTS and duration, complete encoded units and relative audio/video offsets must match. drmr does not shift or trim the original to make a route pass.

## Audio-only TS to fMP4 HLS

A completed audio-only TS playlist can produce clear, CBCS or AES-128 fMP4 HLS. For CBCS, use an output key with `output.hls_key_uri`; encrypted source playlists also need their own input key-URI mapping.

```sh
drmr convert audio-ts/master.m3u8 --to cbcs --format hls \
  --output audio-fmp4-hls --keys-file config/hls-output.json
drmr verify audio-fmp4-hls/master.m3u8 \
  --source audio-ts/master.m3u8 --keys-file config/hls-output.json --json
```

Shaka Packager separates the complete audio track and packages new segments; FFmpeg/FFprobe provide probe, decode and hash evidence. The original ordered PES, every AAC frame and absolute PTS/DTS/duration remain authoritative. A two-second segment target does not permit trimming an AAC frame. The 470-frame TS regression retains 481,280 samples per channel; the progressive source with a finite 480,000-sample window is a different presentation.

## SAMPLE-AES TS

```sh
drmr convert clear-ts/media.m3u8 --to hls-sample-aes --format hls-ts \
  --output fairplay-ts --keys-file config/sample-aes-output.json
drmr verify fairplay-ts/master.m3u8 --source clear-ts/media.m3u8 \
  --keys-file config/sample-aes-output.json --json
```

```sh
drmr convert encrypted-ts/media.m3u8 --to clear --format hls-ts \
  --output clear-ts --keys-file config/sample-aes-source.json
drmr verify clear-ts/master.m3u8 --source encrypted-ts/media.m3u8 \
  --keys-file config/sample-aes-source.json --json
```

Supply output key/IV/URI or source URI/IV mappings as applicable. Clear, AES-128 and SAMPLE-AES TS outputs use the qualified source profile; license/device qualification is separate.

## Explicit PlayReady signaling

This opt-in generic-conversion profile produces CBCS DASH from a qualified clear MP4 source. Supply the output key identifier (KID), key, and license URL as shown below. drmr generates the PlayReady Object (PRO) and Protection System Specific Header (PSSH), then checks their consistency in the initialization segments and DASH manifest. Source DRM initialization metadata is refused by conversion.

```json
{
  "schema_version": 1,
  "output": {
    "kid": "<32-hex-output-kid>",
    "key": "<32-hex-output-key>",
    "signaling": {
      "system": "playready",
      "license_url": "https://license.example.invalid/acquire"
    }
  }
}
```

```sh
drmr convert qualified-clear.mp4 --to cbcs --format dash \
  --output playready-dash --keys-file config/playready.json
```

The explicit signaling regression uses a derived fragmented clear MP4. It does not separately establish a finite-progressive-plus-signaling playback matrix. Matching DRM declarations and known-key media checks do not validate a license server.

# 8. Acquire a URL inside conversion

## A retained download directory is mandatory

Generic URL input supports completed clear HLS with qualified TS or fragmented MP4 media. To analyze before conversion, use these four commands. Prepare the output key file using chapter 5's clear-input CBCS structure and keep it outside source/output trees.

```sh
drmr analyze "https://example.com/vod/master.m3u8" \
  --download-dir ./saved-source --json
drmr plan ./saved-source/input.m3u8 --to cbcs --format dash \
  --output ./converted-dash --save ./conversion.plan.json --json
drmr convert --plan ./conversion.plan.json --keys-file config/keys.json --json
drmr verify --plan ./conversion.plan.json --keys-file config/keys.json --json
```

URL input requires a new `--download-dir` under an existing parent. The plan file and result destination must also be new and separate from the source. `analyze` retains original playlist/resource bytes and creates the local entrypoint `saved-source/input.m3u8`; local playlists change resource references only. Private originals can contain signed URLs. No key URI is retrieved. Remote DASH, standalone MP4, encrypted URLs, live input and byte ranges remain outside this acquisition profile.

Only the first command needs the server. The remaining commands use the retained local source. A completed download remains available after a later analysis or conversion failure; an incomplete acquisition is not admitted. Direct `convert URL --download-dir NEW_DIR` also remains available for a single invocation without a saved plan.

## Reuse offline

After a successful acquisition, the local retained input can be used without the remote server:

```sh
drmr convert saved-source/input.m3u8 --to cbcs --format hls \
  --output retry-hls --keys-file config/hls-output.json
drmr verify retry-hls/master.m3u8 --source saved-source/input.m3u8 \
  --keys-file config/hls-output.json --json
```

Do not reuse an existing download directory for a new acquisition. Partial failed acquisition is not a complete saved source. Source byte identity and physical ownership are checked before final publication; cancellation or mutation prevents success. `convert --plan` preserves its recorded destination and refuses intent overrides or an existing result. Choose a new plan for a different destination; use `verify --plan` to recheck an already published result.

## Fetch policy

Default policy is HTTPS with same-origin references and bounded acquisition. `analyze URL` and direct `convert URL` both accept `--fetch-policy`. Explicit origin/private-network/HTTP access requires the supported private policy configuration; it is not enabled by weakening Host or Origin checks.

```json
{
  "allowed_origins": ["https://media.example.com"],
  "headers_by_origin": {
    "https://example.com": {"Authorization": "Bearer <your-token>"}
  },
  "max_total_bytes": 1073741824,
  "connections": 4
}
```

Protect this JSON file before use:

```sh
drmr convert "https://example.com/vod/master.m3u8" \
  --download-dir authenticated-source --fetch-policy config/fetch.json \
  --to clear --format hls --output clear-result
```

Unknown fields, duplicates, present null values and invalid ranges are refused. Headers are scoped to exact origins; redirects and referenced resources must remain allowed. Admission includes destination relationships, resource counts, bytes and deadlines. A successful plan does not establish these remote conditions.

See [URL conversion](https://github.com/kazuhide/drmr/blob/main/docs/usage.md).

# 9. What verification proves

## Four complete comparisons

| Result field | Required correspondence |
| --- | --- |
| `stream_metadata` | Decoder configuration, dimensions, pixel/sample format, audio rate/channels, color, rotation and supported language; success is `pass` |
| `packet_payloads_equal` | Ordered complete encoded packets or validated canonical access units, including count |
| `packet_timing_equal` | Every PTS, DTS and duration plus applicable exact presentation windows |
| `decoded_content_equal` | Every decoded video frame and presented audio sample, without spot sampling |

Use the actual output `manifest.mpd` or `master.m3u8` for DASH/HLS. The verifier reads source and result independently and decrypts each with its own role keys. Packet identity alone cannot establish that a playback edit survived.

A source with a finite video window and a candidate missing that window fail `packet_timing_equal` even if all encoded packets and decoded pictures match. Input preparation must preserve the original start and duration before comparing the prepared source.

## Audio frames and segment boundaries

An AAC frame has a fixed number of samples. A two-second target does not require each audio segment to be exactly two seconds, nor force equal audio/video segment counts. Frames stay complete. A TS source may already present padding not present in an earlier MP4 edit; verification preserves the supplied TS rather than shortening it to that older file.

## Each file owns its playback window

The finite progressive profile compares exact native edit windows and playback settings. FFmpeg 9.0.2 does not apply finite end discard on every fragmented-MP4 demux path. drmr therefore checks the complete coded audio decode, then applies each actual file's own declared window and compares complete presented PCM. It does not substitute the reference window for the result's metadata.

Integer start/duration conversion, ordinary movie/track rate and gain, track selection and video display extent are checked. A changed end, missing window or modified playback header fails. This interpretation is qualified by media/mutation tests; it does not prove every player honors the edit.

## Manifest timing

DASH uses one coherent presentation origin at the earliest actual presented sample across tracks, including composition offsets and supported edits. A later clipping origin or earlier artificial gap is refused. Exact packet clocks remain unchanged. Where decimal durations are needed, the canonical upward rounding is to nine decimal places, a one-nanosecond text resolution, not a packet tolerance.

Finite HLS segment declarations and end must match the presented window. Initialization movie/track duration headers remain zero; they are not a substitute for the actual edit. Ordinary nonfinite HLS retains its separate declared-decimal rounding rule.

## Processing units and storage

Preparation admits the complete source. Track demuxing can run separately; encryption and packaging follow the chosen route. Segments are scheduling units, not independent proof that the complete final presentation is correct. Complete source-to-final comparison precedes publication.

Packet evidence and PCM can be streamed, but metadata and temporary media are retained as required. Total memory and disk usage are not constant regardless of input length. Resource admission and bounds still apply.

See [processing and timing](https://github.com/kazuhide/drmr/blob/main/docs/media-processing.md).

# 10. Saved HLS workflows

## Why separate commands exist

The generic `analyze` command checks input for `plan --save` and `convert --plan`. The separate `analyze-hls` command captures a durable source snapshot for supported clear HLS. `package-hls` uses that analyzed source and a DRM-signaling configuration, including supported subtitles and audio selection. These are not aliases for generic conversion. Do not discard subtitle or signaling requirements when selecting a workflow.

## Capture a local source

Create a private source JSON with explicit local-read permission:

```json
{
  "url": "/absolute/path/media/master.m3u8",
  "policy": {
    "allowed_local_roots": ["/absolute/path/media"]
  }
}
```

```sh
chmod 600 source.json
drmr analyze-hls --source-config source.json \
  --asset-id sample --output output
drmr verify-snapshot --files output/sample/clear/source
```

Remote inputs instead need the supported fetch policy and origin permissions. The private analysis directory retains the source URLs and downloaded resources. `analysis.json` binds the saved snapshot; an arbitrary edited JSON file is not a new valid source.

## Package with signaling

```json
{
  "widevine_pssh": "<hex-or-base64-widevine-initialization-data>",
  "playready_la_url": "https://license.example.invalid/acquire",
  "fairplay_uri": "skd://content-identifier"
}
```

```sh
chmod 600 drm.json
drmr package-hls --analysis output/sample/clear/source/analysis.json \
  --drm-config drm.json
```

Widevine PSSH is initialization metadata supplied as a complete box or supported payload in hex/Base64. Its identifiers must match `DRM_KID`. Dummy bytes do not create a valid license. Content keys for these commands come from `DRM_KID`, `DRM_KEY` and `DRM_IV`, not the generic `--keys-file` schema.

Audio selection uses the analyzed `audio_source_id` in a private selection JSON. Bind `analysis_id` and `snapshot_digest` to detect stale choices.

`verify-snapshot` checks the recorded content digest; `--files` also rehashes saved playlists and media. It does not replace `verify OUTPUT --source INPUT`, which compares complete media. Snapshot exit values are 0 for matching, 1 for changed and 2 for an invalid snapshot.

See [saved HLS](https://github.com/kazuhide/drmr/blob/main/docs/usage.md).

# 11. Prepared DASH and batch processing

## Prepared input layout

`run` uses a downloaded layout corresponding to original URLs. It does not accept any isolated MPD as a general-purpose conversion request.

```text
sample/
  download/
    manifest.example.test/sample/dash/manifest.mpd
    manifest.example.test/sample/hls/manifest.m3u8
    cdn.example.test/sample/video/v0/init.mp4
    cdn.example.test/sample/video/v0/seg_1.m4s
    cdn.example.test/sample/video/v0/seg_2.m4s
```

The MPD needs BaseURL, MP4 adaptation sets, and a supported SegmentTemplate for each representation with `initialization`, `media`, `timescale` and `duration`. Media names use `$Number$` and numeric segments. BaseURL `https://cdn.example.test/sample/` maps `video/v0/init.mp4` to the illustrated downloaded path.

Prepare `DRM_KID`, `DRM_KEY` and `DRM_IV`, then run:

```sh
drmr plan --mpd sample/download/manifest.example.test/sample/dash/manifest.mpd
drmr run \
  --mpd sample/download/manifest.example.test/sample/dash/manifest.mpd \
  --hls-master \
    sample/download/manifest.example.test/sample/hls/manifest.m3u8 \
  --output output --asset-id sample --log-format jsonl
```

Results appear under `output/sample/`. `--packager-bin` accepts the executable or a directory containing it in this workflow. `--no-round-trip`, `--no-manifests` and `--limit-renditions` are diagnostic reductions, not equivalent to complete conversion and verification.

## Work-list JSON Lines

`batch` processes assets serially and records resumable attempts. It is not an arbitrary list of generic `convert` commands. Its supported kinds are the prepared-DASH and clear-HLS workflows.

The first JSONL line is `{"schema_version":"1"}` with a string value. Each subsequent asset has only these four fields:

| Field | Meaning |
| --- | --- |
| `asset_id` | Asset identifier |
| `kind` | `dash-cenc` or `hls-clear` |
| `dash_manifest` | Relative object key from the storage root, or null when unused |
| `hls_master` | Relative object key from the storage root, or null when unused |

Save this readable asset record as `asset.json`; the actual work list uses one line per record:

```json
{
  "asset_id": "sample",
  "kind": "dash-cenc",
  "dash_manifest":
    "sample/download/manifest.example.test/sample/dash/manifest.mpd",
  "hls_master":
    "sample/download/manifest.example.test/sample/hls/manifest.m3u8"
}
```

```sh
printf '%s\n' '{"schema_version":"1"}' > work-list.jsonl
{
  drmr canonical asset.json && printf '\n'
} >> work-list.jsonl
drmr batch --work-list work-list.jsonl --output output \
  --resume-log resume.jsonl --plan
```

`canonical` emits stable one-line JSON without a terminating newline. The guarded append above adds LF only after success. It prints input values and is not a secret-removal command; do not use it to display key files or credentials.

## Keys and resumption

Default key provider is environment variables. The private file provider uses one JSONL record per asset with `asset_id`, `kid`, `key`, `iv`, without a schema header. This differs from generic `--keys-file` JSON.

```sh
drmr batch --work-list work-list.jsonl --output output \
  --resume-log resume.jsonl --storage local --storage-root ./catalog \
  --key-provider file --key-file ./private/keys.jsonl
```

Keep the key file outside output. HLS configuration can come from `--hls-config-dir`. `--retry-failed` selects failed items; default `--max-attempts` is 3. `--per-asset-timeout-s` defaults to 3600 and `--heartbeat-s` to 60. Resumption is not rollback: inspect existing output and logs first.

S3 storage is implemented with current supported region `ap-northeast-1`. It uses AWS environment/default-profile credentials. `--bucket`, `--region` and `--output-prefix` configure storage. Native local-media CI results do not qualify a real S3 account.

See [batch usage](https://github.com/kazuhide/drmr/blob/main/docs/usage.md).

# 12. Workers, memory and Stop

## Worker limits

`convert` and `verify` select workers from available CPU, memory, media dimensions, resource sizes and track count. `--max-workers` is an upper bound from 1 to 64; the actual worker count can be lower. Automatic recommendations cap at 32.

```sh
drmr convert source.mpd --to cbcs --format dash \
  --output result --keys-file config/keys.json --max-workers 2
drmr verify result/manifest.mpd --source source.mpd \
  --keys-file config/keys.json --max-workers 1 --json --measure
```

Worker count never reduces proof coverage. Track, segment and source-probe pools share reservations within one process. `batch` processes assets serially. Separate CLI processes do not share a reservation pool.

## Admission is not OS enforcement

Readable Linux CPU/memory cgroup limits, including parent limits, inform admission. If unavailable, drmr assumes a 512 MiB budget and one child process. An operation that cannot fit fails with `resource-budget`.

When memory is known, a quarter is reserved with a 256 MiB minimum, and per-track metadata is estimated. Child reservations include a base, frame counts, workload and capture bounds. These estimates are not an OS memory limit or a guarantee of exact peak usage.

FFmpeg decoder/encoder/filter and ffprobe decoding thread settings use one thread; auxiliary threads may still exist. Packet/audio evidence is streamed, while retained metadata still grows. Default captured stdout is 16 MiB and stderr 1 MiB. Over-limit output fails rather than verifying truncated evidence.

## Cancellation and cleanup

Generic convert/verify use SIGINT/SIGTERM on macOS/Linux and Ctrl+C/Ctrl+Break on Windows. Stop prevents new work, terminates owned processes and confirms cleanup. A Stop observed before publication leaves the destination unpublished.

Exit 130 means cooperative cancellation completed including process confirmation. `cleanup-unconfirmed` and `media-cleanup-unconfirmed` are failures, not successful Stop. Force-kill is a different operation and may leave private work behind.

Unix process groups and Windows job objects retain descendant ownership. Sending a signal alone does not prove locks are released. Unix final group checks and Windows job notifications have their own 30-second budgets; total wall-clock shutdown also includes I/O, name lookup and scheduling.

## Measurements

Use `--measure` with `--json`. It adds observations without changing verification. Nested/parallel phases overlap; their times cannot simply be summed into wall time. Short clips are dominated by startup and cache effects. Sampled memory is not an exact peak. Compare the same original, tools, build settings, worker bounds and host load only after verification passes.

See [resources and measurements](https://github.com/kazuhide/drmr/blob/main/docs/resources.md).

# 13. Docker with media tools

## Build an image from committed source

The image includes drmr, FFmpeg/ffprobe, Bento4 mp4dump and Shaka Packager. Build requires Git and BuildKit-capable Docker. Native Linux amd64/arm64 are supported build targets; select Linux containers in Docker Desktop.

```sh
revision=$(git rev-parse HEAD)
tree=$(git rev-parse "${revision}^{tree}")
git archive --format=tar "$revision" | docker build \
  --build-arg DRMR_REVISION="$revision" \
  --build-arg DRMR_TREE="$tree" --tag drmr:local -
docker run --rm --init drmr:local doctor --probe --route all
```

This captures committed files and the lockfile, excluding uncommitted changes. Keep keys, input, downloads and output outside the repository and bind-mount them at runtime. Initial compilation uses time and disk; Docker retains caches. Image compiler pins are separate from the CLI's supported minimum Rust version.

## Read-only input and private configuration

Prepare host `media`, `config` and `output` directories. Protect configuration/output for your user and key JSON with mode 600. This example assumes supported DASH and valid keys already exist:

```sh
docker run --rm --init --user "$(id -u):$(id -g)" \
  --mount "type=bind,src=$PWD/media,dst=/input,readonly" \
  --mount "type=bind,src=$PWD/config,dst=/config,readonly" \
  --mount "type=bind,src=$PWD/output,dst=/output" \
  drmr:local convert /input/manifest.mpd \
  --to cbcs --format dash --output /output/protected-dash \
  --keys-file /config/keys.json
```

Paths passed to drmr are container paths. `/input` and `/config` are read-only; `/output` is writable. Matching host UID/GID allows owner-only key reading and output ownership. The default image user is not root. drmr does not repair unsafe host ownership or permissions.

Fresh verification can mount the result read-only:

```sh
docker run --rm --init --user "$(id -u):$(id -g)" \
  --mount "type=bind,src=$PWD/media,dst=/input,readonly" \
  --mount "type=bind,src=$PWD/config,dst=/config,readonly" \
  --mount "type=bind,src=$PWD/output,dst=/output,readonly" \
  drmr:local verify /output/protected-dash/manifest.mpd \
  --source /input/manifest.mpd --keys-file /config/keys.json --json
```

Docker does not expand the media profile. URL conversion additionally requires a separate writable retained-download mount and its allowed connection policy. Readable container CPU/memory limits inform worker admission; they are not a promise of an exact memory maximum.

## Stop and reclaim owned resources

`--init` forwards termination signals and reaps exited children. drmr still confirms its owned tools. Use Ctrl+C when attached, or allow cooperative shutdown time with `docker stop --timeout 60 NAME`. Forced termination is not confirmed cleanup.

`--rm` removes the container's temporary filesystem after exit; bind-mounted originals/configuration/results remain. After no container uses your own image tag, it can be removed:

```sh
docker image rm drmr:local
```

Build cache remains separately. Remove only your unused dedicated builder/cache as appropriate; do not globally prune unrelated resources.

## Web console networking

On Linux, host networking preserves the default loopback address `127.0.0.1:8080`:

```sh
docker run --rm --init --name drmr-console --network host \
  --user "$(id -u):$(id -g)" \
  --mount "type=bind,src=$PWD/catalog,dst=/catalog,readonly" \
  --mount "type=bind,src=$PWD/output,dst=/output" \
  --env DRMR_CATALOG_ROOT=/catalog --env DRMR_OUTPUT_ROOT=/output \
  drmr:local serve
```

Docker Desktop requires its additional host-networking feature. Ordinary port publication cannot reach a service listening only on container loopback. Do not disable loopback/Host/Origin safeguards to work around this. The image does not contain a native Desktop GUI.

On Windows, use Docker Desktop WSL2 integration and a Linux shell. Keep private configuration on the Linux filesystem; Windows-drive mounts have different permission semantics from native Windows ACL workflows.

`/usr/share/drmr/build-manifest.json` records commit/tree and tool provenance. Notices are under `/usr/share/drmr/licenses`; corresponding selected media-tool sources are under `/usr/share/drmr/sources`, not the complete Debian source tree.

See [Docker usage and scope](https://github.com/kazuhide/drmr/blob/main/docs/docker.md).

# 14. Native Windows CLI

## Environment

Use a normal Windows x64 user on local NTFS. Elevated runs creating Administrators-owned media, network shares, junctions in private paths, alternate data streams and unsafe existing permissions are refused.

Extract the CLI archive and add it to your user PATH or invoke the absolute executable. Media tools remain separate. Builds use VCRUNTIME140.dll; install the Microsoft Visual C++ x64 Redistributable when required.

```powershell
cargo build --release --locked -p drmr
.\target\release\drmr.exe --help
.\target\release\drmr.exe doctor --probe --route all
```

## Owner-only key files

Windows uses an access-control list (ACL), not chmod. The protected ACL must disable inheritance, retain the current user as owner and grant that user full control. The following creates a new private directory and empty key file before editing secret content:

```powershell
$sid = [System.Security.Principal.WindowsIdentity]::GetCurrent().User
$private = Join-Path $env:LOCALAPPDATA 'drmr-keys'
if (Test-Path -LiteralPath $private) { throw 'Choose a new directory' }
New-Item -ItemType Directory -Path $private | Out-Null

function Set-OwnerOnlyAcl($path, $security) {
    $security.SetOwner($sid)
    $security.SetAccessRuleProtection($true, $false)
    $rule = [System.Security.AccessControl.FileSystemAccessRule]::new(
        $sid, 'FullControl', 'Allow')
    $security.AddAccessRule($rule)
    Set-Acl -LiteralPath $path -AclObject $security
}

```

Apply the ACL to the directory and file, then enter keys:

```powershell
Set-OwnerOnlyAcl $private ([Security.AccessControl.DirectorySecurity]::new())
$keyFile = Join-Path $private 'keys.json'
New-Item -ItemType File -Path $keyFile | Out-Null
Set-OwnerOnlyAcl $keyFile ([Security.AccessControl.FileSecurity]::new())
notepad $keyFile
```

If an editor replaces the file and changes its ACL, reapply the owner-only ACL while keeping it inside the private directory. drmr does not silently fix an unsafe existing key file.

## PowerShell conversion

```powershell
.\drmr.exe inspect 'C:\media\source.mp4' --json
.\drmr.exe plan 'C:\media\source.mp4' --to cbcs --format mp4
$outDir = Join-Path $env:LOCALAPPDATA 'drmr-output'
New-Item -ItemType Directory -Path $outDir -Force | Out-Null
$result = Join-Path $outDir 'protected.mp4'
.\drmr.exe convert 'C:\media\source.mp4' --to cbcs --format mp4 `
  --output $result --keys-file $keyFile
.\drmr.exe verify $result --source 'C:\media\source.mp4' `
  --keys-file $keyFile --json
```

Create `drmr-output` first and choose a nonexistent `protected.mp4`. Manifest URIs and object keys continue to use `/`, unlike native Windows paths. Ctrl+C/Ctrl+Break return 130 only after descendant and file-lock confirmation. Arbitrary filesystem/tool configurations, including long paths, are not guaranteed merely because native Windows CI passes.

See [Windows conditions](https://github.com/kazuhide/drmr/blob/main/docs/windows.md).

# 15. Local Web console

## Start with prepared roots

The console is an unauthenticated local operator interface. Use trusted loopback only; do not expose it with a reverse proxy or port forwarding. Host/Origin validation does not authenticate another local process.

```sh
DRMR_CATALOG_ROOT=/absolute/path/catalog \
DRMR_OUTPUT_ROOT=/absolute/path/output \
drmr serve
```

Open `http://127.0.0.1:8080`. Both roots are required. `DRMR_BIND_PORT` changes the port; `DRMR_MAX_WORKERS` bounds HLS packaging workers. Catalog entries need supported prepared layouts. The console is not a general file picker or complete generic-conversion UI.

Use the same-version server and CLI. `--cli` selects the actual executable; a wrapper launching a different CLI can be refused by the supervision contract.

## HLS connection policy

Without extra configuration, the default profile allows only the input URL's own HTTPS origin. Additional origins/credentials require a private named-profile file. `DRMR_HLS_PROFILES` is that file path, not inline JSON:

```json
{
  "default": {
    "policy": {
      "allowed_origins": ["https://media.example.com"],
      "headers_by_origin": {
        "https://media.example.com": {"Authorization": "Bearer <token>"}
      }
    }
  }
}
```

```sh
chmod 600 profiles.json
DRMR_CATALOG_ROOT=/absolute/path/catalog \
DRMR_OUTPUT_ROOT=/absolute/path/output \
DRMR_HLS_PROFILES=/absolute/path/profiles.json \
drmr serve
```

This corresponds to the UI's default profile. Include the input origin itself in its allowlist. Web acquisition clears local roots and does not allow file input. Prepared catalog DASH and explicitly permitted CLI local HLS are separate workflows.

## Job state and recovery

UI JSON requests have a 15-second headers-and-body response budget for recovery, not the media processing deadline. A lost mutation response does not trigger an automatic POST retry. The UI reconciles recorded jobs, leaves unresolved outcomes unknown and offers GET-only status recovery.

Stop requests termination for the owned job and tools. An accepted response alone is not confirmed cleanup; inspect terminal job status and logs. Normal Stop can advance the queued job. New jobs are not started during server shutdown.

Prepared Web workflows can modify partial/older results, unlike generic conversion's new-destination publication. Stop is not rollback. Exit 0, file existence or a stale converted flag alone does not establish playback readiness; the output-readiness generation must match.

Unconfirmed cleanup retains the job slot and private configuration ownership. Status/log/Stop diagnosis remains available while new work is refused. Fix the cause and retry orderly shutdown; force-exit skips this assurance.

## Retention and keys

Adding clear HLS retains URLs, original playlists and downloaded media under `OUTPUT_ROOT/<asset-id>/clear/source/`. Removing history does not delete saved originals/results/logs.

Key inputs are visually masked by default, can be revealed explicitly and clear after submission. The application does not persist content keys to localStorage/sessionStorage. That does not isolate keys from browser memory, developer tools, extensions or page/network inspection.

Local ClearKey playback intentionally supplies keys as JWK JSON or player configuration. Even `no-store` responses remain observable in developer tools/HAR. It is not an operational DRM license service or hardware-protected playback.

HLS playback authorization context can remain in memory for 30 minutes of inactivity, checked on related access and a 30-second sweep. Clearing a form is not immediate revocation. The final playback-session revocation or server shutdown destroys owned context.

Player setup has a 20-second local budget including prior-player destruction, key lookup, media loading and actual `playing`. Failure restores controls; unconfirmed destruction prevents overlapping players and may require reloading the page. Playback recovery does not alter conversion verification.

See [console operation](https://github.com/kazuhide/drmr/blob/main/docs/usage.md) and [security](https://github.com/kazuhide/drmr/blob/main/docs/security.md).

# 16. Desktop host

## Open the same local console

The experimental native host wraps the local Web console. Catalog, jobs, media tools and restrictions are shared.

1. Choose File > Choose catalog folder.
2. Choose File > Choose output location. A private `drmr-output` child is created or reopened there.
3. Choose File > Check external tools in the actual application environment.
4. Choose File > Open console.

Restart the application to change roots after opening. Navigation stays inside the owned loopback origin; external origins and new windows are denied. Reports open in the same window, and Open console returns home. The page receives no native shell/filesystem bridge.

Quit drmr stops admission and waits for server/tool confirmation. If cleanup is unconfirmed, the app keeps the window and owners, displays the error and permits another Quit attempt after the cause is fixed. This differs from OS force-quit.

## Launch environment and bundle

Finder's PATH may differ from a terminal. The app does not run shell startup files or guess installation paths. Launch from a terminal that can find your tools when needed:

```sh
"/path/to/drmr.app/Contents/MacOS/drmr" doctor
"/path/to/drmr.app/Contents/MacOS/drmr-desktop"
```

Check external tools performs doctor-style version/help diagnostics, not media validation. An unconfirmed diagnostic process retains ownership and prevents normal exit or opening the console.

CLI and host are paired inside one bundle; the host checks the CLI SHA-256. Media tools, keys, downloads and deployment configuration are not bundled. `build-manifest.json` records source/tree and binary identity; it is not a signed build-origin certificate.

## Build a native bundle

Install Tauri's OS prerequisites and media tools. Windows uses WebView2, macOS WKWebView and Linux WebKitGTK.

```sh
RUSTUP_TOOLCHAIN=1.95.0 python3 tools/build_desktop.py --bundle
```

The preceding is a POSIX shell example. PowerShell uses a separate environment assignment:

```powershell
$env:RUSTUP_TOOLCHAIN = '1.95.0'
python tools/build_desktop.py --bundle
```

`--release` selects optimization. Use a dedicated `CARGO_TARGET_DIR`; the default is `target/desktop`. The helper stages matching CLI/notices and produces an unsigned macOS app or Windows/Linux portable directory/archive. Existing bundles are refused. Signing, notarization and installers are outside this workflow.

## Observed behavior and limits

Native macOS checks have exercised folder menus, keyboard input, report navigation, catalog conversion, active Stop and orderly close/listener release. Chrome's local ClearKey original/result playback has reached the end.

WKWebView English/Japanese error and retry recovery was exercised; completed playback remains unqualified. Windows/Linux GUI behavior also remains unqualified. Native builds do not establish dialog/WebView/DRM playback success. Linux GUI dependencies have documented maintenance limitations.

See [Desktop scope](https://github.com/kazuhide/drmr/blob/main/docs/desktop.md) and [dependency policy](https://github.com/kazuhide/drmr/blob/main/docs/dependency-security.md).

# 17. Results, exit values and troubleshooting

## JSON and progress

Use `convert --json` or `verify --json` for machine-readable output. A successful unmeasured verification produces one versioned JSON object:

```json
{
  "schema_version": 1,
  "passed": true,
  "media_verified": true,
  "checks": {
    "stream_metadata": "pass",
    "packet_payloads_equal": true,
    "packet_timing_equal": true,
    "decoded_content_equal": true
  },
  "license_playback_verified": false
}
```

Operational failure after a valid command returns `schema_version: 1`, `command`, `status` (`failed` or `cancelled`) and a fixed safe `error` containing `code`, `hint`, `last_activity`. A completed mismatch also retains comparison fields. For example, removing a finite video window can retain payload/decoded equality but fail timing:

```json
{
  "schema_version": 1,
  "passed": false,
  "media_verified": false,
  "checks": {
    "stream_metadata": "pass",
    "packet_payloads_equal": true,
    "packet_timing_equal": false,
    "decoded_content_equal": true
  },
  "license_playback_verified": false,
  "command": "verify",
  "status": "failed",
  "error": {
    "code": "verification-mismatch",
    "hint": "Review every source/result check. Do not use the result as verified media.",
    "last_activity": "verification"
  }
}
```

Argument parsing errors occur before this JSON contract and retain stderr/help with exit 2. If stdout itself is unavailable, complete JSON cannot be guaranteed; stderr reports `result-output-unavailable` and exit 1.

Without JSON, observed activity/count/elapsed lines go to stderr. A closed track scope is not proof that the conversion succeeded. Phases may overlap; the last observed activity is not an exclusive phase or a diagnosed cause.

## Exit values

| Value | Meaning |
| --- | --- |
| 0 | The selected command succeeded; what it checked depends on the command |
| 1 | Processing, comparison, required diagnostics or cleanup confirmation failed |
| 2 | Invalid arguments/configuration; invalid snapshot for `verify-snapshot` |
| 130 | A supported CLI confirmed cooperative cancellation and owned-process cleanup |

`doctor` success is selected diagnostic success; `inspect` success is metadata reading, not complete media verification. Snapshot exit 1 means content changed.

## Common failures

| Symptom/code | Action |
| --- | --- |
| Tool unavailable | Check the actual launch PATH, executable permission and Desktop diagnostics |
| Unsupported version/features | Inspect build options as well as version; confirm Bento4 build with its distributor |
| Key file refused | Check owner, mode 600/ACL, link count, duplicate/unknown fields and schema |
| `input-key-required` | Supply every source KID or exact HLS key-URI mapping |
| `output-key-required` / `output-key-not-used` | Supply the required output key; remove unnecessary output keys for clear output |
| `presentation-window-not-supported` | Check edit structure, start/end, normal rate and exact clock conversion; do not trim the original |
| `progressive-cenc-profile-not-supported` / `progressive-cbcs-profile-not-supported` | Check sample encryption, IV/auxiliary layout and input initialization headers |
| `unsupported-packet-timing` | Check complete PTS/DTS/duration evidence and required tools; do not invent missing clocks |
| `round-trip-verification-failed` | Inspect metadata, encoded payloads, full decode and timing individually |
| `resource-budget` | Check observed CPU/memory, media size/dimensions; more workers may not help |
| `media-output-limit` / `media-probe-output-limit` | Inspect capture/record bounds; truncated evidence cannot prove success |
| `cleanup-unconfirmed` / `media-cleanup-unconfirmed` | Keep ownership and recover; do not treat locks/processes as released |

Choose a fresh destination for another conversion. A complete retained URL input can be reused locally, but an existing download directory cannot be acquired again. Confirm tools have stopped before deleting private work.

Share fixed error codes, tool versions, supported profile and a minimal sanitized reproduction. Redacted logs do not mean every URL, filename or signaling field is confidential-data-free.

# 18. Regression tests and evidence limits

## Short public media

Real-media regressions use the attributed ten-second Big Buck Bunny excerpt with 240 H.264 video packets and AAC audio. Video segmentation targets five two-second segments; audio follows complete frame boundaries rather than forcing every segment to exactly two seconds.

| Regression | Scope |
| --- | --- |
| Legacy 16 protection routes | Prepared clear/cenc/cbcs DASH and AES-128 fMP4 HLS, separate original/result keys, conversion and fresh verification |
| TS to DASH: 27 routes | Three input protections, video/joint/audio layouts, clear/cenc/cbcs DASH |
| Audio-only TS to fMP4 HLS: 9 pairs | Clear/AES-128/SAMPLE-AES inputs to clear/CBCS/AES-128 fMP4 HLS; complete original packet, clock and decoded-audio checks |
| TS output: 27 routes | The same input protections/layouts to clear/AES-128/SAMPLE-AES TS |
| Finite audio: 54 unique routes | Three MP4 input protections, audio/joint layouts, nine MP4/DASH/HLS output choices |
| Finite CLI: 27 repeat routes | Real CLI confirmation of the joint subset of the same 54 combinations |
| URL input | Qualified clear TS/fMP4 acquisition, CBCS HLS conversion and offline retained-source reuse |

Encrypted fixtures use different video/audio KIDs and keys. GPAC is only an offline fixture-generation tool, not a runtime/test dependency. Negative cases alter clocks/windows by one sample, keys, payloads, playback controls, dimensions, auxiliary encryption and manifest declarations.

## Read results for the actual revision

Minimum Rust and stable run on Linux/macOS/Windows. Native Windows checks cover regular-user permissions, Ctrl+C/Ctrl+Break, descendants, locks and workspaces. Linux AMD64 Docker checks cover real media and HTTP/Stop/shutdown. Desktop native builds remain separate from GUI/device playback.

Read each revision's CI logs and artifacts for source identity, compilers, tools and results. This guide does not certify every later moving-main revision or arbitrary device.

The 54 finite combinations consist of audio27 plus joint27; CLI27 repeats the latter. The legacy16 use a prepared fragmented reference, whereas finite tests use the original progressive file's own playback range. Do not mix sample counts across different references/decoding paths.

Bento4 installation pins and observed build banners are different evidence. Docker HTTP tests do not qualify a browser UI; native bundle construction does not qualify playback. Known-key media checks do not establish license acquisition or device operation.

## Recorded audio-only TS results

The following jobs completed on 20 October 2026 (Japan Standard Time) for source [f0096539](https://github.com/kazuhide/drmr/commit/f0096539e698d66cc77affcd7ea891c793aa87bf). Rust 1.95 or later is required; 1.99.1 is the stable compiler observed in this run, not a fixed requirement.

| Environment and job | Observed Rust | Audio HLS pairs | TS regression tests |
| --- | --- | --- | --- |
| [Linux AMD64 / minimum](https://github.com/kazuhide/drmr/actions/runs/39327394237/job/118317107508) | `1.95.0` | 9/9 | 34/34 |
| [Linux AMD64 / stable](https://github.com/kazuhide/drmr/actions/runs/39327394237/job/118317106710) | `1.99.1` | 9/9 | 34/34 |
| [macOS ARM64 / minimum](https://github.com/kazuhide/drmr/actions/runs/39327394237/job/118317107104) | `1.95.0` | 9/9 | 34/34 |
| [macOS ARM64 / stable](https://github.com/kazuhide/drmr/actions/runs/39327394237/job/118317107079) | `1.99.1` | 9/9 | 34/34 |
| [Windows AMD64 / minimum](https://github.com/kazuhide/drmr/actions/runs/39327394310/job/118316974281) | `1.95.0` | 9/9 | 34/34 |
| [Windows AMD64 / stable](https://github.com/kazuhide/drmr/actions/runs/39327394310/job/118316974370) | `1.99.1` | 9/9 | 34/34 |

Each environment exercised the same nine protection pairs. The 34 tests are `conversion_ts` (11) plus `conversion_ts_output` (23), including five new audio-HLS regressions. These are nine capabilities checked on six environments.

All 470 original AAC frames and 481,280 decoded samples per channel were checked, preserving the original first presentation time of 7/5 seconds and playback end of 857/75 seconds. Each output passed the four comparisons during conversion and a fresh verification against the original TS; source files were unchanged. Decoded hashes are compared within each job's decoding environment, not assumed identical across different tool builds.

The [Linux AMD64 Docker job](https://github.com/kazuhide/drmr/actions/runs/39327394297/job/118317216488) separately passed one representative clear audio TS → CBCS fMP4 HLS route and fresh original-TS verification. It does not exercise all nine audio-HLS pairs. These results do not cover DRM license acquisition or device playback.

## Reproduce selected suites

With source and media tools installed, run from the repository root:

```sh
cargo test --locked -p drmr --test bbb_media -- --nocapture
cargo test --locked -p drmr-core --test conversion_ts_output -- --nocapture
cargo test --locked -p drmr-core --test conversion_ts_output \
  audio_hls -- --nocapture
cargo test --locked -p drmr --test conversion_ts_dash -- --nocapture
cargo test --locked -p drmr-core --test conversion_finite_outputs \
  -- --nocapture
cargo test --locked -p drmr --test conversion_finite_outputs -- --nocapture
```

`DRMR_CONVERSION_REPORT` saves the legacy16 `bbb_media` matrix's JSON to a new path outside the repository. It does not configure every TS/finite suite's report. Use their test logs and CI artifacts. Reports include actual fixture hashes, four comparisons, tool versions, elapsed/sampled-memory observations and measurement limits.

A separate long-clock regression repeats a tiny derived 32x18 source 417 times for 4,170 seconds and 100,080 packets:

```sh
cargo test --locked -p drmr --test long_conversion
```

This checks complete long packet evidence, not a benchmark for high-resolution throughput or an exact memory maximum. Neither this nor the short fixture establishes universal input or DRM-device compatibility.

See [fixture attribution](https://github.com/kazuhide/drmr/blob/main/crates/drmr/tests/fixtures/bbb/README.md), [encrypted fixtures](https://github.com/kazuhide/drmr/blob/main/crates/drmr/tests/fixtures/bbb/CENC.md) and [processing](https://github.com/kazuhide/drmr/blob/main/docs/media-processing.md).

# 19. CLI argument and option reference

All public commands accept `-h` / `--help`; `drmr help COMMAND` also displays help. Use top-level `-V` / `--version` or `drmr version`. Private supervision bootstrap arguments are not a user interface.

## doctor

| Option | Meaning/default |
| --- | --- |
| `--json` | One machine-readable diagnostic object |
| `--probe` | Bounded version/help/advertised-options checks; default is discovery only |
| `--route PROFILE` | Required tool set, default `all` |

PROFILE is `all`, `prepared-dash`, `progressive-mp4`, `fragmented-mp4`, `dash-presentation`, `fmp4-hls-presentation`, `mpeg-ts-hls`, `mpeg-ts-sample-aes-hls` or `mpeg-ts-hls-ts`. Output route names are not automatically valid doctor values; use `all` to include the tool set for TS-DASH.

## inspect

| Argument/option | Meaning |
| --- | --- |
| `INPUT` | Local MP4, MPD or M3U8; remote resources are not acquired |
| `--json` | Versioned metadata report |

## analyze

| Argument/option | Meaning |
| --- | --- |
| `INPUT` | Complete local MP4, DASH or HLS; completed clear HLS URL within the acquisition profile |
| `--download-dir NEW_DIR` | Required for a URL; retain the original privately |
| `--fetch-policy FILE` | Private origin/header/resource policy for URL input |
| `--keys-file FILE` | Protected input keys when source checks require decryption |
| `--packager-bin FILE` | Shaka executable; default packager |
| `--max-workers N` | Upper bound 1-64; otherwise automatic CPU/memory choice |
| `--json` | One input-analysis report; input_admitted does not verify a converted output |
| `--measure` | Add numeric measurements; requires --json |

Local input refuses download/fetch options. URL analysis always requires a new download directory.

## plan

| Argument/option | Meaning |
| --- | --- |
| `INPUT` | Local input for generic conversion intent |
| `--to TARGET` | Required in INPUT mode: clear, cenc, cbcs, hls-aes128, hls-sample-aes |
| `--format FORMAT` | mp4, dash, hls, hls-ts; alternatives allowed without --save, one resolved format when saving; omitted follows MP4/DASH/HLS, with HLS meaning fMP4 |
| `--json` | Versioned plan without execution |
| `--save NEW_FILE` | Validate the source and save an execution plan; requires INPUT, --to and --output |
| `--output NEW_PATH` | Result destination fixed by --save; refused without --save |
| `--keys-file FILE` | Input keys for source checks; only with --save |
| `--packager-bin FILE` | Shaka executable for source checks; only with --save, default packager |
| `--max-workers N` | Upper bound 1-64 for source checks; only with --save |
| `--measure` | Add measurements to stdout; requires --json and --save; saved file stays an execution plan |
| `--mpd MPD` | Separate prepared-DASH rendition-listing mode |
| `--asset-id ID` | Asset name in MPD mode |

## convert

| Argument/option | Meaning |
| --- | --- |
| `INPUT` | Supported local original or completed clear HLS URL; omitted with --plan |
| `--to TARGET` | Required output protection without --plan |
| `--format FORMAT` | One output format; HLS defaults to fMP4 even for TS input |
| `--output NEW_PATH` | Required new file/directory without --plan |
| `--plan FILE` | Execute saved source, format, protection and output path; refuse media-intent overrides |
| `--keys-file FILE` | Private JSON separating input/output key roles |
| `--download-dir NEW_DIR` | Mandatory for URL input; retained original |
| `--fetch-policy FILE` | Private URL-origin/header/resource policy JSON |
| `--packager-bin FILE` | Shaka executable; default packager |
| `--max-workers N` | Upper bound 1-64; otherwise automatic CPU/memory choice |
| `--json` | One versioned result; suppress ordinary progress lines |
| `--measure` | Add measurement fields; requires --json |

There are no generic `ffmpeg-bin` or `ffprobe-bin` options. Those tools are found on the launch environment's PATH.

`--plan` conflicts with INPUT, --to, --format, --output, --download-dir and --fetch-policy. Runtime keys, tool selection, worker limits and reporting options remain separate.

## verify

| Argument/option | Meaning |
| --- | --- |
| `OUTPUT` | Candidate MP4, MPD or M3U8; omitted with --plan |
| `--source INPUT` | Required original without --plan; never inferred from the result |
| `--plan FILE` | Check recorded source, output format/protection and independent media correspondence; conflicts with OUTPUT and --source |
| `--keys-file FILE` | input keys for original, output keys for candidate |
| `--packager-bin FILE` | Shaka executable; default packager |
| `--max-workers N` | Upper bound 1-64; admission may reduce it |
| `--json` | One independent verification report |
| `--measure` | Add measurements; requires --json |

Verification does not rebuild the result using a new target/format.

Plan verification adds plan_id, source_unchanged and output_matches_plan, with complete comparison results under verification. Success requires source_unchanged, output_matches_plan and verification.passed all to be true. Matching media alone does not establish the planned encryption. Output signaling and license playback remain separate.

## run

| Option | Meaning/default |
| --- | --- |
| `--mpd MPD` | Required supported prepared manifest |
| `--output ROOT` | Required; results under ROOT/asset-id |
| `--asset-id ID` | Asset name, otherwise original directory name |
| `--hls-master M3U8` | Original HLS master; needed unless --no-manifests |
| `--packager-bin PATH` | Executable or its containing directory |
| `--max-workers N` | Rendition parallelism upper bound |
| `--manifest-local-url-base URL` | Also generate manifests for local HTTP serving |
| `--log-format FORMAT` | text or jsonl; default text |
| `--no-round-trip` | Diagnostic: omit complete decode comparison |
| `--limit-renditions N` | Diagnostic: process only part of the rendition set |
| `--no-manifests` | Diagnostic: omit playlist/manifest generation |

## analyze-hls / package-hls

| analyze-hls option | Meaning |
| --- | --- |
| `--source-config FILE` | Required private source URL/path and permission configuration |
| `--asset-id ID` | Required saved asset name |
| `--output ROOT` | Required; save under ROOT/ID/clear/source |

| package-hls option | Meaning |
| --- | --- |
| `--analysis FILE` | Required completed analysis.json |
| `--drm-config FILE` | Required private signaling configuration |
| `--selection FILE` | Private analyzed audio selection |
| `--packager-bin FILE` | Shaka executable, default packager |
| `--max-workers N` | Shared track/segment bound 1-64 |

Content keys for these workflows are `DRM_KID`/`DRM_KEY`/`DRM_IV`. Passing generic key JSON as drm-config is not supported.

## verify-snapshot / canonical / version

| Command/argument | Meaning |
| --- | --- |
| `verify-snapshot PATH` | Check recorded digest for snapshot directory or analysis.json |
| `verify-snapshot --files PATH` | Also rehash saved playlists/media |
| `canonical PATH` | Stable JSON output; no final LF or secret removal |
| `version` | Print version; no command-specific options |

## batch

| Option | Meaning/default |
| --- | --- |
| `--work-list FILE` | Required schema header and asset JSONL |
| `--output ROOT` | Required output root |
| `--resume-log FILE` | Required attempt-state record |
| `--storage STORAGE` | local or s3; default local |
| `--storage-root DIR` | Local object-key root |
| `--bucket NAME` | S3 bucket |
| `--region REGION` | Default and currently supported S3 region ap-northeast-1 |
| `--output-prefix PREFIX` | Storage result prefix, default output |
| `--key-provider PROVIDER` | env or file; default env |
| `--key-file FILE` | Private per-asset JSONL for file provider |
| `--hls-config-dir DIR` | HLS workflow configurations |
| `--packager-bin PATH` | Packager location |
| `--max-workers N` | Per-asset processing bound; assets remain serial |
| `--retry-failed` | Select failed attempts |
| `--plan` | Show intended work without executing |
| `--max-attempts N` | Per-asset attempt cap, default 3 |
| `--per-asset-timeout-s SEC` | Per-asset budget, default 3600 |
| `--heartbeat-s SEC` | Status interval, default 60 |

## serve

| Option/environment | Meaning/default |
| --- | --- |
| `--static-dir DIR` | Trusted UI directory replacing embedded assets |
| `--cli FILE` | Same-version CLI for jobs |
| `DRMR_CATALOG_ROOT` | Required prepared catalog root |
| `DRMR_OUTPUT_ROOT` | Required results/saved-HLS/log root |
| `DRMR_BIND_HOST` | Default loopback; remote exposure is not intended |
| `DRMR_BIND_PORT` | Default 8080 |
| `DRMR_MAX_WORKERS` | HLS packaging worker upper bound |
| `DRMR_HLS_PROFILES` | Owner-only named HLS acquisition policy file path |

# 20. References and maintenance

## Current public documentation

These links follow public main. For a different installed version, select its source revision or accompanying documentation.

- [README and installation](https://github.com/kazuhide/drmr/blob/main/README.md)
- [Usage and configuration](https://github.com/kazuhide/drmr/blob/main/docs/usage.md)
- [Conversion profiles](https://github.com/kazuhide/drmr/blob/main/docs/conversion-support.md)
- [Processing and complete verification](https://github.com/kazuhide/drmr/blob/main/docs/media-processing.md)
- [Processing routes and planning](https://github.com/kazuhide/drmr/blob/main/docs/processing-routes.md)
- [Required tools](https://github.com/kazuhide/drmr/blob/main/docs/requirements.md)
- [Keys and local security](https://github.com/kazuhide/drmr/blob/main/docs/security.md)
- [Resources](https://github.com/kazuhide/drmr/blob/main/docs/resources.md)
- [Docker](https://github.com/kazuhide/drmr/blob/main/docs/docker.md)
- [Windows](https://github.com/kazuhide/drmr/blob/main/docs/windows.md)
- [Desktop](https://github.com/kazuhide/drmr/blob/main/docs/desktop.md)
- [Dependency security](https://github.com/kazuhide/drmr/blob/main/docs/dependency-security.md)

## Primary format references

General format rules are separate from drmr's implemented/verified profiles. Do not infer tool or player support from a specification alone.

- [W3C: ISO Common Encryption MP4 stream format](https://www.w3.org/TR/eme-stream-mp4/)
- [RFC 8216: HLS media and encryption declarations](https://www.rfc-editor.org/rfc/rfc8216.html)
- [Apple: HLS Sample Encryption](https://developer.apple.com/library/archive/documentation/AudioVideo/Conceptual/HLS_Sample_Encryption/)
- [W3C: MP4 initialization and edit lists](https://www.w3.org/TR/mse-byte-stream-format-isobmff/)

This guide summarizes public source, help, documentation and stated qualification limits. Example paths/domains and angle-bracket key placeholders must be replaced with your own authorized media and configuration before executing commands. A saved PDF also has its own stated target version.
