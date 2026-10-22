---
title: User guide
updated: 2026-10-22
version: 0.1.0
---

# 1. Introduction

drmr changes media containers, delivery formats and encryption while retaining encoded video and audio. It is for engineers who control the media and already have the required decryption keys. It is not an encoder for creating a new quality ladder or a client for obtaining DRM licenses.

This guide describes drmr 0.1.0 and was updated on 2026-10-21. Public source and command help define the behavior. For a different version, also read its accompanying documentation. A passing example does not qualify arbitrary media or playback devices.

## Choose an operation

| Goal | Command | What it establishes |
| --- | --- | --- |
| Find external tools | `doctor` | Required executables can be found in the launch environment |
| Check versions and options | `doctor --probe` | Version and help output advertise required features; no media is processed |
| Read input declarations | `inspect` | Local MP4, DASH or HLS metadata; no decryption or complete sample proof |
| Check complete source media | `analyze` | Actual input admission; URL input is retained in an explicit download directory |
| Describe a conversion | `plan` | Requested formats and protection, candidate routes and runtime conditions |
| Save a conversion request | `plan --save` | Source fingerprints and one output, or an explicit list of outputs with DRM selections and manifest-check policies |
| Convert media | `convert` | Actual input admission, conversion and comparison before publishing new output |
| Compare independently | `verify` | Reads original and result independently and checks complete media and timing |
| Check only final manifests | `verify --manifest-only` | Native HLS/DASH linting; add --external-validator for external checks; no media comparison or decryption |

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

The CLI is the main interface. Docker provides an option with media tools included. The experimental local Web console and Desktop host support analysis, plan saving, conversion and verification using the same saved plans, alongside prepared-content workflows. They do not expose every CLI command.

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

Widevine, PlayReady and FairPlay involve signaling, key exchange and device behavior beyond selecting an AES mode. A KID identifies a key; it is not the secret content key.

Common `plan`, `convert` and `verify` commands accept an output list for DASH with Widevine/PlayReady and HLS with FairPlay. Protection System Specific Header (PSSH) data carries DRM initialization information; it is separate from the encrypted media and from a license. Chapter 5 describes the runtime metadata, and chapter 6 describes the output list. The existing single-output PlayReady configuration and saved-HLS workflow remain available. Successful known-key verification does not prove a license service or device will play the result.

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
| fMP4 | clear, cenc, cbcs, AES-128 | cenc/cbcs by KID; AES-128 by the exact playlist key URI |
| TS | clear, AES-128, SAMPLE-AES | Encrypted inputs by key URI; SAMPLE-AES also needs its associated IV |

Each fMP4 media playlist must contain one video or audio track. A master can associate separate video and audio playlists. Unsupported rendition metadata and inconsistent audio-group associations are refused.

## Explicit DRM profiles in an output list

The initial explicit multi-DRM profiles accept supported clear HLS/TS or the existing qualified clear MP4 profile. Existing source DRM initialization metadata and other source layouts are refused by these profiles. This additional restriction does not change the other conversion profiles in the tables above.

| Output list format and target | DRM systems | Input for this explicit profile |
| --- | --- | --- |
| `dash`, `cbcs` | Widevine, PlayReady, or both | Qualified clear HLS/TS or clear MP4 |
| `hls`, `cbcs` | FairPlay | Qualified clear HLS/TS or clear MP4 |
| `hls-ts`, `hls-sample-aes` | FairPlay | Qualified clear HLS/TS |

`hls` produces fragmented MP4. `hls-ts` produces transport streams. One output list can select either HLS container alongside DASH, or request both HLS containers. Each output has its own named directory and runtime key settings. The list does not imply that every DRM system can be combined with every encryption scheme.

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

## One key configuration for several outputs

When keys or DRM metadata are required, an output list uses a protected schema-version-2 key file. Shared source keys belong under `input`; output keys and DRM metadata belong under `outputs`, indexed by the IDs in the output list. Encrypted outputs require their own entry, clear outputs have no entry, and extra or missing IDs are refused. The following template is for the `dash` and `hls` outputs in chapter 6. Replace every placeholder before use.

```json
{
  "schema_version": 2,
  "outputs": {
    "dash": {
      "kid": "<32-hex-dash-kid>",
      "key": "<32-hex-dash-key>",
      "signaling": {
        "system": "multi-drm",
        "widevine_pssh": "<complete Widevine PSSH box in hex or base64>",
        "playready_license_url": "https://license.example.invalid/playready"
      }
    },
    "hls": {
      "kid": "<32-hex-hls-kid>",
      "key": "<32-hex-hls-key>",
      "hls_key_uri": "skd://<content-identifier>",
      "signaling": {
        "system": "multi-drm",
        "fairplay": true
      }
    }
  }
}
```

Supply a complete Widevine PSSH box whose declared KIDs match the DASH output's `kid`. drmr checks that metadata and generates PlayReady information from the selected KID and license URL. It writes the FairPlay HLS declaration from the explicit FairPlay selection and `hls_key_uri`. Runtime DRM systems must exactly match the saved plan. No license service or advertised key URI is contacted.

For a FairPlay TS output named `hls-ts`, replace the HLS entry with this member of `outputs`:

```json
{
  "hls-ts": {
    "key": "<32-hex-ts-key>",
    "iv": "<32-hex-associated-iv>",
    "hls_key_uri": "skd://<content-identifier>",
    "signaling": {
      "system": "multi-drm",
      "fairplay": true
    }
  }
}
```

TS SAMPLE-AES requires the associated IV in the runtime key configuration and uses no MP4 KID. Its playlist may omit the `IV` attribute when the associated IV is supplied separately; an explicitly declared conflicting IV is refused. For fMP4 FairPlay, a playlist IV can also be absent when the initialization segment's track-encryption metadata (`tenc`) supplies the required constant IV. The actual media and IV declarations are checked, rather than assuming that an omitted playlist attribute means unencrypted media.

The file schemas serve different roles: the public output recipe uses version 1, the private multi-output key file uses version 2, and the saved multi-output execution plan uses version 2. Existing single-output key files and execution plans remain version 1. This does not extend the initial explicit multi-DRM input profile to encrypted sources.

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

Without `--save`, `plan` reports requested formats, key roles and candidate processing routes. Comma-separated formats in this mode describe alternatives. Use `--outputs` below to request several actual outputs. Every requested format must have a candidate. Unsupported or empty candidate sets are incompatible; source-profile admission remains a runtime condition. This metadata-only JSON is not an execution plan and is refused by `convert --plan`.

For a single output, use `--save` with a new result destination to check the complete source and save one resolved format:

```sh
drmr plan source/input.m3u8 --to cbcs --format dash \
  --output converted-dash --save conversion.plan.json --json
drmr convert --plan conversion.plan.json --keys-file config/keys.json --json
drmr verify --plan conversion.plan.json --keys-file config/keys.json --json
```

The saved plan records the local playlist/media files used by conversion, their sizes and SHA-256 content fingerprints, the output format/protection and an absolute destination. Paths are tied to that machine and location; keep the plan private and outside source/output trees. Unknown versions or fields, inconsistent plan identifiers, changed source bytes and unsafe destinations are refused. The identifier detects inconsistency; it is not a signature or authorization from another party.

Keys, signed source URLs, authentication headers, license URLs, HLS key URIs and executable arguments are not stored in the plan. Supply keys at execution and verification with `--keys-file`. In a version-1 single-output plan, DRM signaling selections also remain runtime configuration. Version-2 plans fix each requested DRM system and manifest-check policy while leaving their secret values and addresses separate. `verify --plan` checks source fingerprints, actual output format/protection and independent complete media correspondence. It does not acquire a license or prove device playback.

On Unix, use trusted output and plan parent directories owned by your user that other users cannot modify. Saving a plan refuses an unsafe output parent before media admission. See [workspace permissions](https://github.com/kazuhide/drmr/blob/main/docs/security.md#temporary-files-and-cleanup) for the supported shared temporary-directory rule and cleanup boundaries.

Saving a plan creates no converted media. Encrypted source checks can decrypt with supplied input keys. Source admission is repeated when executing. The `plan --mpd` mode remains the separate prepared-DASH rendition listing described in chapter 11.

## Save several explicitly named outputs

Use `--outputs` when all listed outputs must be produced together. Save this public recipe as `outputs.json`:

```json
{
  "schema_version": 1,
  "outputs": [
    {
      "id": "dash",
      "format": "dash",
      "target": "cbcs",
      "drms": ["widevine", "playready"],
      "conformance": "required"
    },
    {
      "id": "hls",
      "format": "hls",
      "target": "cbcs",
      "drms": ["fairplay"],
      "conformance": "required"
    }
  ]
}
```

For FairPlay TS instead of fMP4 HLS, use an output with `id: "hls-ts"`, `format: "hls-ts"`, `target: "hls-sample-aes"` and `drms: ["fairplay"]`. Supply the matching TS key entry from chapter 5. Up to eight outputs are accepted. IDs contain one to 32 lowercase ASCII characters, start with a letter, and continue with letters, digits or hyphens; reserved device names are refused. Duplicate IDs, unknown fields and arbitrary child paths are invalid. The recipe contains no keys or DRM initialization data and may be a normal readable JSON file.

```sh
drmr plan clear-ts/master.m3u8 --outputs outputs.json \
  --output result --save multi.plan.json --json
```

The saved version-2 plan binds one original file graph to every output's format, protection, DRM selection and external manifest-check policy. The result root and source fingerprints are fixed. `--outputs` requires `--save` for `plan` and replaces `--to` and `--format`; a comma-separated `--format` in the older metadata-only mode still describes alternatives.

`conformance: "required"` makes the external manifest check mandatory during both conversion and independent plan verification. `"none"`, the default when omitted, requests no external check. Native manifest linting and complete media verification run in both cases. Prepare the tools described in chapter 9 before executing a required check; planning itself does not establish that those tools are available.

# 7. Local conversion examples

## Common rules

Choose a new destination with an existing safe parent. Generic conversion leaves the original unchanged and publishes only after verification and source/Stop checks. Existing output is refused rather than replaced. Use `--keys-file` when input or output protection requires keys.

Omitting `--format` follows the inspected MP4/DASH/HLS input category. HLS defaults to fMP4 HLS even when its input media is TS. Request `--format hls-ts` explicitly for TS output.

## Execute the complete output list

Use the version-2 key file from chapter 5 with the saved multi-output plan:

```sh
drmr convert --plan multi.plan.json \
  --keys-file config/multi-keys.json --json
drmr verify --plan multi.plan.json \
  --keys-file config/multi-keys.json --json
```

The example creates `result/dash/manifest.mpd` and `result/hls/master.m3u8`, with their media and child playlists. A requested `hls-ts` output has its own TS resources. Each output is packaged separately; DASH and fMP4 HLS do not share physical media files in this implementation.

All requested outputs are checked inside a private working directory. The new result root is published in one filesystem operation after every output passes its media, requested DRM and required manifest checks. Failure or cancellation does not publish a subset. Existing results are refused, and the retained original remains available.

For an immediate local operation without saving a plan:

```sh
drmr convert clear-ts/master.m3u8 --outputs outputs.json \
  --output result --keys-file config/multi-keys.json --json
```

Use a saved plan when a later `verify --plan` must also enforce the original fingerprints and the complete output list. The single-output examples below remain supported.

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

# 8. Acquire and analyze a URL

## A retained download directory is mandatory

This example uses clear HLS and a CBCS DASH output. Prepare chapter 5's `config/keys.json` with only the output key. `&&` proceeds only after success. `inspect` reads retained declarations after acquisition; omit it if this summary is unnecessary.

```sh
drmr analyze "https://example.com/vod/master.m3u8" \
  --download-dir ./saved-source &&
drmr inspect ./saved-source/input.m3u8 &&
drmr plan ./saved-source/input.m3u8 --to cbcs --format dash \
  --output ./converted-dash --save ./conversion.plan.json &&
drmr convert --plan ./conversion.plan.json --keys-file config/keys.json &&
drmr verify --plan ./conversion.plan.json --keys-file config/keys.json
```

URL input requires a new `--download-dir` under an existing parent. The plan file and result destination must also be new and separate from the source. Format detection uses the response content, so URLs need no filename extension. Queries and allowed redirects do not select the format.

| URL input | Retained local entrypoint | Acquisition profile |
| --- | --- | --- |
| MP4 | `saved-source/input.mp4` | One complete MP4 within the supported local media profiles |
| DASH | `saved-source/input.mpd` | Static MPD with one `Period`, `SegmentTemplate` and an explicit finite `SegmentTimeline` |
| HLS | `saved-source/input.m3u8` | Completed TS/fMP4 playlists within the supported input profiles |

For DASH or MP4, change the local input path above accordingly. DASH saves every referenced initialization and media file. Its localized `input.mpd` retains protection declarations, PSSH DRM initialization data and the timeline. HLS localizes media references and relative key identifiers. Original manifests are kept separately and may contain signed URLs. Acquisition leaves encrypted media and initialization bytes unchanged.

The same local codec, track, encryption and timing restrictions still apply. Live input, byte ranges, multiple DASH periods, `SegmentList`, `SegmentBase`, duration-only DASH templates and negative timeline repeats remain outside this acquisition profile. These are implementation limits, rather than technical impossibilities of those formats.

Encrypted HLS accepts AES-128 TS/fMP4, SAMPLE-AES TS and CENC/CBCS fMP4 under the local input restrictions. For AES-128 and TS SAMPLE-AES, `input.hls_keys[].uri` must match the absolute identifier resolved after redirects: `keys/key.bin` in `https://example.com/vod/media.m3u8` becomes `https://example.com/vod/keys/key.bin`. Absolute `skd:` URIs stay unchanged. CENC/CBCS uses `input.keys`. No key or license URL is fetched. Encrypted input requires `--keys-file config/keys.json` on `analyze` and `plan` too; clear input does not.

Only `analyze` needs the server. Completed downloads survive later failures; incomplete downloads are refused. `convert URL --download-dir NEW_DIR` combines acquisition and conversion without saving a plan.

## URL to several DRM outputs

For a supported clear TS HLS URL, use the same public `outputs.json` and private version-2 key file as the local workflow:

```sh
drmr analyze "https://example.com/vod/master.m3u8" \
  --download-dir ./multi-source --json
drmr plan ./multi-source/input.m3u8 --outputs outputs.json \
  --output multi-result --save multi.plan.json --json
drmr convert --plan multi.plan.json \
  --keys-file config/multi-keys.json --json
drmr verify --plan multi.plan.json \
  --keys-file config/multi-keys.json --json
```

One retained source is used by all outputs. URL acquisition also supports the other documented input profiles, but explicit multi-DRM conversion still requires a qualified clear TS HLS or clear MP4 source. Acquisition support does not extend that output profile. For one acquisition-and-conversion command without saving a plan:

```sh
drmr convert "https://example.com/vod/master.m3u8" \
  --download-dir ./multi-source --outputs outputs.json \
  --output multi-result --keys-file config/multi-keys.json --json
```

Choose fresh source and result directory names when running either example. The direct URL operation retains acquisition ownership and source byte checks through final publication of the complete result root.

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

`--fetch-policy FILE` reads an owner-only UTF-8 JSON object saved as, for example, `config/fetch.json`. It contains the policy fields directly, without a `schema_version` field; the input URL stays on the command line. The file is optional, and its fields are optional: omit `headers_by_origin` when authentication is not needed. The example fields mean:

- `allowed_origins`: an array of additional exact origins (scheme, host and port), without a path. The input URL's own origin is already allowed.
- `headers_by_origin`: an object mapping an allowed origin to its request-header object, such as `Authorization`. This field does not itself grant access to another origin.
- `max_total_bytes`: the total acquisition byte budget; `1073741824` is 1 GiB.
- `connections`: the fetch concurrency upper bound; worker and CPU/memory admission can reduce actual concurrency.

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

Prepare a protected configuration directory as described in chapter 5 and save the JSON as `config/fetch.json`. Replace the token placeholder with your own token if authentication is needed. For macOS/Linux, set owner-only file permissions before analyzing the URL; use the current-user-only ACL procedure in chapter 14 on Windows.

```sh
chmod 600 config/fetch.json
drmr analyze "https://example.com/vod/master.m3u8" \
  --download-dir analyzed-source --fetch-policy config/fetch.json --json
```

The same file can be used for direct conversion:

```sh
drmr convert "https://example.com/vod/master.m3u8" \
  --download-dir authenticated-source --fetch-policy config/fetch.json \
  --to clear --format hls --output clear-result
```

Unknown fields, duplicates, present null values and invalid ranges are refused. Headers are scoped to exact origins; redirects and referenced resources must remain allowed. `max_playlist_bytes` bounds each HLS playlist or DASH MPD; MP4 and media resources use `max_resource_bytes`, and `max_total_bytes` applies to the whole download. Admission includes destination relationships, resource counts, bytes and deadlines. A successful plan does not establish these remote conditions.

See [URL conversion](https://github.com/kazuhide/drmr/blob/main/docs/usage.md).

## Example prerequisites and key file {#recipe-setup}

Select an output in the matrix and **one four-command group** for your input's protection. The groups are alternatives. These POSIX shell examples use `&&` to stop on failure. They share files; `|` instead passes standard output. Replace the `https://example.com` URL.

Use an existing private working directory. drmr saves the input in `src` and creates `plan.json` and `result` (MP4: `result.mp4`); these paths must be new. No `mkdir` or `cd` is needed. For another run, change all paths consistently or use another directory. For [fetch policy settings](#url-input), add `--fetch-policy config/fetch.json` to `analyze`.

When needed, prepare `keys.json` here using [chapter 5's schemas and permissions](#key-configuration), replacing its `config/keys.json` path. Keep it outside source/output directories. Use `schema_version: 1`; omit the clear side's `input` or `output` role. `analyze` and `plan` need only input keys when encrypted; `convert` and `verify` need keys for each encrypted side. Clear → Clear needs no key file.

CENC/CBCS input uses `input.keys`; AES-128 and TS SAMPLE-AES use `input.hls_keys` with absolute key URIs and required IVs. Cenc/cbcs output needs `output.kid` and `output.key`; HLS also needs `output.hls_key_uri`. Other output fields are in chapter 5. Input and output keys may differ; no key or license URL is fetched.

Inputs must meet [chapter 3's profiles](#supported-profiles). MP4, DASH and fMP4 HLS examples assume one video track. TS video may include audio; audio-only TS uses 48 kHz AAC-LC. Joint TS output has separate audio/video playlists.

Explicit FairPlay HLS or Widevine/PlayReady DASH signaling uses [chapter 6's multiple-output workflow](#inspect-plan) and its input/runtime requirements.

{{conversion-examples}}

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

The integer `EXT-X-TARGETDURATION` bounds segment durations after the nearest-integer rounding specified by RFC 8216. drmr derives conservative track bounds by rounding final segment durations upward, then applies the largest bound to every generated audio/video media playlist. For example, video and audio bounds of 2 and 3 become 3 and 3, while the encoded segments and `EXTINF` durations stay unchanged. Independent verification rejects inconsistent final bounds. RFC 8216 defines separate exceptions for VOD subtitles and I-frame playlists; those exceptions do not allow ordinary audio/video bounds to differ.

## Each file owns its playback window

The finite progressive profile compares exact native edit windows and playback settings. FFmpeg 9.0.2 does not apply finite end discard on every fragmented-MP4 demux path. drmr therefore checks the complete coded audio decode, then applies each actual file's own declared window and compares complete presented PCM. It does not substitute the reference window for the result's metadata.

Integer start/duration conversion, ordinary movie/track rate and gain, track selection and video display extent are checked. A changed end, missing window or modified playback header fails. This interpretation is qualified by media/mutation tests; it does not prove every player honors the edit.

## Manifest timing

DASH uses one coherent presentation origin at the earliest actual presented sample across tracks, including composition offsets and supported edits. A later clipping origin or earlier artificial gap is refused. Exact packet clocks remain unchanged. Where decimal durations are needed, the canonical upward rounding is to nine decimal places, a one-nanosecond text resolution, not a packet tolerance.

Finite HLS segment declarations and end must match the presented window. Initialization movie/track duration headers remain zero; they are not a substitute for the actual edit. Ordinary nonfinite HLS retains its separate declared-decimal rounding rule.

## DRM information and manifest checks

Version-2 `verify --plan` reads all current result graphs again and compares each with the retained original. The report separates the following outcomes:

| Outcome | Meaning |
| --- | --- |
| `verification` | Complete encoded/decoded content, metadata and timing correspondence |
| `native_manifest` in `verification` | Native Rust checks of final HLS/DASH declarations, with rule coverage and profile revision |
| `output_matches_plan` | Actual container and protection match the saved output intent |
| `signaling` | Requested DRM metadata matches runtime settings and actual initialization/manifest records |
| `conformance` | Required external manifest checks passed, or no check was requested |

Changed PSSH, PlayReady data, KIDs or FairPlay declarations can fail even when media decryption succeeds. An empty `drms` list requests no service-specific DRM information and its signaling result is not marked verified. It still requires the complete ordinary media and protection checks.

CENC or CBCS output with no selected DRM provider may still contain a
W3C Common SystemID PSSH record. It names a key identifier without containing
the content key or provider-specific policy. drmr checks its generated single-key,
empty-data form against the output settings and actual track protection. This
does not mark a DRM service or license playback as verified, or broaden the
admitted input profiles.

The native manifest linter runs during common HLS/DASH conversion and full verification. It checks the final manifest bytes without an external validator. HLS syntax, segment/key state, feature versions and rendition references are distinct from Apple-specific VOD rules. Native DASH rules check the named static-presentation profile, including structure, inheritance, addressing, timelines and protection declarations. This is independently implemented Rust linting, rather than a complete XML schema or Schematron engine.

Native reports distinguish passed, failed and not-checked rules, and include specification references. In-scope unsupported features, missing linked playlists, resource limits and truncated diagnostics prevent a clean result. Media-dependent and delivery-dependent rules are outside this manifest scope; full verification supplies separate media evidence. Rule identifiers and source positions are reported without echoing manifest text, key addresses or protection payloads. Small inputs run inline; larger independent documents can run in parallel within CPU and memory admission.

For `conformance: "required"`, HLS uses Apple's `mediastreamvalidator` in playlist-checking mode. DASH uses the local DASH-IF Conformance Software XML schema and Schematron checks, which validate MPD structure and rules. These checks read the final encrypted output manifests, including reachable HLS child playlists. They are distinct from full media verification and do not prove encrypted-media conformance, license acquisition or device playback. Missing tools, interrupted checks, incomplete reports and validation errors cause failure. The policy `"none"` leaves the external check unrequested and is reported separately from a pass.

Prepare the Apple HLS tools in a macOS environment. The DASH route requires the supported local DASH-IF software and dependencies; set `DRMR_DASH_IF_ROOT` to that checkout. `--hls-validator-bin` selects the Apple executable and defaults to `mediastreamvalidator`. `--dash-validator-bin` selects the PHP interpreter for the local DASH-IF adapter and defaults to `php`; it is not a path to an arbitrary DASH-checking command. These runtime paths are separate from the saved plan. See [manifest validation setup and scope](https://github.com/kazuhide/drmr/blob/main/docs/manifest-validation.md).

HLS protocol versions are feature-based minimums, not exact required values:

| Output features in these profiles | Minimum `EXT-X-VERSION` |
| --- | --- |
| Normal fMP4 media playlists using `EXT-X-MAP` | 6 |
| FairPlay SAMPLE-AES TS with key-format declarations | 5 |
| Either profile declaring version 7 | Accepted by the minimum-version rule |

A sufficient version alone does not establish that all other playlist declarations are valid. An omitted playlist IV may be supported by the associated TS IV or fMP4 track-encryption metadata described in chapter 5; any explicit inconsistency is refused.

## Check existing manifests without comparing media

Use this distinct mode when only the final HLS/DASH manifest check is needed:

```sh
drmr verify result/hls/master.m3u8 --manifest-only --json
drmr verify result/dash/manifest.mpd --manifest-only --json
```

These commands run the native Rust linter and need no external validator. Add `--external-validator` to run Apple or DASH-IF checks as well. The JSON result separates `native_manifest` and `external_manifest`. `--max-workers` sets an upper bound; omitting it selects concurrency automatically. Apple's adapter uses parse-only mode; its rule coverage differs from the native Apple authoring profile.

The default `--manifest-profile base` checks RFC 8216 manifest rules for HLS and the static-presentation profile for DASH. With `--manifest-only`, select `--manifest-profile apple-vod` to add the supported Apple VOD authoring rules to HLS; DASH rejects this selection. The report names the selected profile. A single video bitrate may pass base HLS rules and fail Apple's section 9.9 requirement for multiple video bitrates. Conversion preserves existing video qualities and does not generate additional ones. These profiles do not certify media, delivery or device behavior outside their declared manifest scope.

The report has `scope: "manifest-only"` and `media_verified: false`. It reads no content keys and performs no source-to-result media comparison. `--manifest-only` cannot be combined with `--plan`, `--source` or `--keys-file`. To establish complete media correspondence and the saved conditions, use ordinary `verify --plan` with the appropriate key file.

## Processing units and storage

Preparation admits the complete source. Track demuxing can run separately; encryption and packaging follow the chosen route. Segments are scheduling units, not independent proof that the complete final presentation is correct. Complete source-to-final comparison precedes publication.

Packet evidence and PCM can be streamed, but metadata and temporary media are retained as required. Total memory and disk usage are not constant regardless of input length. Resource admission and bounds still apply.

Multi-output conversion retains byte-bound private copies of completed outputs until the whole result is ready for publication. Independent plan verification also retains all result copies until its final consistency checks. Together with separate packaging for every requested output, this increases temporary disk usage with media size and output count. The copies are streamed to disk rather than retaining complete media in memory. Keep enough free working space and allow the owned cleanup to finish.

See [processing and timing](https://github.com/kazuhide/drmr/blob/main/docs/media-processing.md).

# 10. Saved HLS workflows

## Why separate commands exist

Use common `analyze`, `plan --outputs`, `convert --plan` and `verify --plan` for new multi-output DRM conversions. The separate `analyze-hls` and `package-hls` commands remain compatibility workflows for saved HLS analysis, including their supported subtitles and audio selection. `package-hls` can produce both DASH and HLS despite its historical name. It is not the recommended entrypoint for a new multi-output conversion.

The examples below retain that workflow's source snapshot and configuration conventions. These commands are not aliases for common conversion, and its specific subtitle or selection features should be checked before migrating an existing job.

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

For `hls-clear`, `--hls-config-dir DIR` contains an owner-only `ASSET_ID.json` object with `source_config` and `drm_config` paths and optional `selection`; those referenced files use the [saved HLS JSON formats in chapter 10](https://kazuhide.github.io/drmr/guide/#saved-hls). Relative references resolve from that configuration directory. `--packager-bin` is passed to `run` for `dash-cenc`, which accepts an executable or its directory, and to `package-hls` for `hls-clear`, which requires the executable. Use an executable path for mixed or HLS batches.

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

Native manifest checks share this capacity. Small inputs run inline; larger HLS graphs can distribute document work, and a large DASH manifest can share one parsed tree across independent checks. Pools do not nest. Parsed child facts and inherited timelines are reused. The adapter reserves memory for retained manifest bytes and lint work, then verifies file identities and content. Exhausted byte, node, reference or diagnostic limits produce an incomplete report and cannot pass.

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

A synthetic native-library measurement on 2026-10-21 used source
[`c1458e7d`](https://github.com/kazuhide/drmr/commit/c1458e7deea5b31118073c682d799f60c8f4e8b5), Rust 1.99 release and a macOS ARM64 host with 10 logical CPUs
and 32 GiB of memory. A master and 64 media playlists with 1,000 segments each
took 14.50 ms serially and 3.37 ms with automatic selection of 10 workers.
Complete reports matched. Small inputs stayed inline; a 30,000-entry DASH
timeline took 5.22 ms serially and 5.27 ms automatically, so more workers
do not always reduce time. These are whole-library-call observations from three
rotating-order rounds, separate from conversion time. Our media tests were paused;
other host processes were not isolated. See the linked resource guide for the
separate allocation and process-memory observations.

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

Keep output and temporary paths shallow: a media tool can create a longer temporary manifest filename even after its segments were written successfully. For HLS and DASH children of a multi-output request, drmr places media-output stages beside the private bundle stage under the original output parent, avoiding extra nesting. Verified HLS/DASH children move into the bundle on the same filesystem; MP4 retains its separate file-copy publication stage. Only the complete checked bundle is published. This preserves atomic publication but does not remove external tools' path-length limits.

See [Windows conditions](https://github.com/kazuhide/drmr/blob/main/docs/windows.md).

# 15. Local Web console

## Start with prepared roots

The console is an unauthenticated local operator interface. Use trusted loopback only; do not expose it with a reverse proxy or port forwarding. Host/Origin validation does not authenticate another local process.

```sh
DRMR_CATALOG_ROOT=/absolute/path/catalog \
DRMR_OUTPUT_ROOT=/absolute/path/output \
drmr serve
```

Open `http://127.0.0.1:8080`. Both roots are required. `DRMR_BIND_PORT` changes the port; `DRMR_MAX_WORKERS` caps workers for saved-plan operations and prepared HLS packaging. Automatic allocation stays within that cap. New conversions select from allowed input folders. Prepared catalog entries still need their supported layouts.

Use the same-version server and CLI. `--cli` selects the actual executable; a wrapper launching a different CLI can be refused by the supervision contract.

For keys or fetch settings, set `DRMR_COMMON_CONFIG_ROOT` before startup to a private configuration folder separate from input, output and custom static folders.

The private configuration picker replaces file and folder names with numbered labels. Each page follows filename order; check the corresponding local folder before choosing a file. Configuration contents and original names are not shown in the console.

## Convert with a saved plan

**New conversion** runs the same saved-plan operations as the CLI, one action at a time:

1. Choose **Local media**, **Source URL** or **Saved CLI plan**, then **Open input**. URL input accepts the [supported MP4, static DASH and completed HLS profiles](#url-input). Files come from folders allowed by the server or Desktop; the page does not upload files or accept arbitrary host paths.
2. For new media, select a **Key file** when needed, set **Workers**, then choose **Analyze**. A URL can use a **Fetch policy file**. Keep keys and fetch policies in the private configuration folder; do not enter their contents into the page.
3. Add 1–8 named outputs. Choose format, encryption, DRM signaling and external manifest checks, then select a parent folder and a new output folder name. **Save plan** validates the input profile and supported combinations.
4. Use **Download original plan** to retain a private copy, then **Convert**. Existing destinations are refused. Editing output settings requires saving a new plan.
5. Choose **Verify again** to compare the saved plan with existing output without reconverting it. Later, open the same file as a **Saved CLI plan** to verify without repeating analysis or conversion.

New plans use the named-output v2 format, even with one output. When supplying a key file, analyze accepts v1 or v2; new plans and their conversion/verification use v2. An imported CLI v1 plan uses v1 keys. Chapter 19 documents the file formats.

Plan downloads preserve the original bytes and absolute paths. Keep them private: they can contain local paths and signed URLs. Do not rewrite paths to move a plan between environments. The page sends references rather than raw keys, configuration contents or executable paths, and never retrieves key-file contents.

URL sessions use a generic **Source URL** label. The server does not return the submitted URL to the page; credentials can occur in the path as well as in query parameters. The input field is cleared when submitted.

A session admits at most 32 operations. At the limit, download the plan, choose **Close this conversion**, then reopen the saved plan to verify existing output. If a launch outcome is unknown, **Check status** looks up that same operation without resubmitting it. A running or cleanup-unconfirmed session cannot close. Changing the selection or language does not release its execution slot.

**Prepared content** retains catalog conversion, HLS snapshots, queues and local playback. The named HLS profiles and key-entry form described below belong to that workflow; they are separate from the new workflow's fetch-policy file.

## Prepared-HLS connection policy

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

Prepared-workflow key inputs are visually masked by default, can be revealed explicitly and clear after submission. The application does not persist content keys to localStorage/sessionStorage. That does not isolate keys from browser memory, developer tools, extensions or page/network inspection.

Local ClearKey playback intentionally supplies keys as JWK JSON or player configuration. Even `no-store` responses remain observable in developer tools/HAR. It is not an operational DRM license service or hardware-protected playback.

HLS playback authorization context can remain in memory for 30 minutes of inactivity, checked on related access and a 30-second sweep. Clearing a form is not immediate revocation. The final playback-session revocation or server shutdown destroys owned context.

Player setup has a 20-second local budget including prior-player destruction, key lookup, media loading and actual `playing`. Failure restores controls; unconfirmed destruction prevents overlapping players and may require reloading the page. Playback recovery does not alter conversion verification.

See [console operation](https://github.com/kazuhide/drmr/blob/main/docs/usage.md) and [security](https://github.com/kazuhide/drmr/blob/main/docs/security.md).

# 16. Desktop host

## Open the same local console

The experimental native host wraps the local Web console. Catalog, jobs, media tools and restrictions are shared.

1. Choose **File > Choose input folder…** for allowed media and saved plans. Existing prepared catalog layouts remain usable.
2. Choose **File > Choose output location…**. A private `drmr-output` child is created or reopened there.
3. Optionally choose **File > Choose private configuration folder…** for keys and fetch policies, separate from input/output. Clear private configuration selection removes this selection before console startup.
4. Choose File > Check external tools in the actual application environment.
5. Choose File > Open console.

Use **New conversion** for the saved-plan steps in chapter 15. Without a configuration folder, clear flows without keys or fetch settings remain available. Key-file contents are not uploaded through the browser.

**Download original plan** opens a native save dialog for a new private JSON file. Choose a folder outside the input folder and `drmr-output`: the unchanged plan can contain absolute paths and signed URLs. The private folder must not contain either media folder. The default is the private configuration folder, or an owner-only `drmr-plans` sibling of `drmr-output`. Unsafe existing folders and existing files are refused without changing them. To reopen the copy later, select its folder as **Private configuration folder** before console startup, then choose **Saved CLI plan**. Select `drmr-plans` in the later session if the copy was saved there; the private folder must remain separate from input/output media. CLI verification with `drmr verify --plan /path/to/private/drmr-plan.json` is also available, with runtime keys if needed. Other attachment downloads are unavailable in the Desktop window.

Canceling the save dialog creates no file. If Quit drmr is waiting for an open save dialog, cancel the dialog to finish closing. A late selection is discarded, active transfer and owned partial-file cleanup are awaited, and a replaced file or parent folder is preserved.

If cleanup remains unconfirmed, inspect the selected private folder and any hidden .drmr-plan-cleanup- folders. Unfamiliar files are preserved and are never restored by overwriting an occupied name. Keep those files, restore the original private folder, then choose Quit drmr again.

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
| `output-spec-invalid` | Check recipe schema version 1, unique safe IDs and supported format/protection/DRM combinations |
| `execution-plan-output-keys-mismatch` / `execution-plan-drm-mismatch` | Match the version-2 key file's encrypted output IDs and DRM selections to the plan |
| `manifest-validator-unavailable` / `manifest-validator-not-configured` | Install the selected Apple/DASH-IF dependencies and check executable paths; a required check cannot be skipped |
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
| HLS URL input | Qualified clear and encrypted TS/fMP4 acquisition, input-key URI mapping, CBCS conversion and offline retained-source reuse |
| MP4 and DASH URLs: 6 cases | Each format with clear/cenc/cbcs input; authentication and query string required; offline saved-plan execution and independent verification |
| Explicit DRM: five output cases | Widevine/PlayReady together or separately in CBCS DASH, plus FairPlay fMP4/TS HLS; complete media comparison and fresh checks of actual manifest/init declarations |
| Named encrypted outputs | One clear TS source to DASH, fMP4 HLS, SAMPLE-AES TS and AES-128 TS, with separate output keys; saved single-output transport plans are also independently verified |

Encrypted fixtures use different video/audio KIDs and keys. GPAC is only an offline fixture-generation tool, not a runtime/test dependency. Negative cases alter clocks/windows by one sample, keys, payloads, playback controls, dimensions, auxiliary encryption and manifest declarations.

The six MP4/DASH URL cases passed with ten-second fixtures: MP4 has one video track; DASH has separate video and audio tracks with two-second segment targets. Each case analyzes its URL, stops the source server, saves a plan from the retained local entrypoint, runs `convert --plan`, then independently runs `verify --plan`. Clear input produces CBCS DASH; cenc/cbcs input produces clear DASH. Packet content, timing, complete decoded content, source identity and output intent all match. Original media and raw manifests remain unchanged, and stdout, stderr and saved plans exclude the supplied credentials and keys. Run this regression with `cargo test --locked -p drmr --test url_formats_media -- --nocapture`. It does not establish DRM playback or execution of all 45 placeholder recipes.

Explicit DRM regressions also cover the maximum accepted Widevine PSSH, changed
provider/KID/IV declarations, actual initialization-file references and decoys,
and source changes. For ordinary fMP4 HLS, version 5 is rejected and version 7
is accepted. Separate manifest-adapter tests cover incomplete reports, changed
private copies, cancellation and child cleanup. Actual Apple and DASH-IF checks
require the separately installed tools; the ordinary fixture suite does not
claim license acquisition or device playback.

## Read results for the actual revision

Minimum Rust and stable run on Linux/macOS/Windows. Native Windows checks cover regular-user permissions, Ctrl+C/Ctrl+Break, descendants, locks and workspaces. Linux AMD64 Docker checks cover real media and HTTP/Stop/shutdown. Desktop native builds remain separate from GUI/device playback.

Read each revision's CI logs and artifacts for source identity, compilers, tools and results. This guide does not certify every later moving-main revision or arbitrary device.

The 54 finite combinations consist of audio27 plus joint27; CLI27 repeats the latter. The legacy16 use a prepared fragmented reference, whereas finite tests use the original progressive file's own playback range. Do not mix sample counts across different references/decoding paths.

Bento4 installation pins and observed build banners are different evidence. Docker HTTP tests do not qualify a browser UI; native bundle construction does not qualify playback. Known-key media checks do not establish license acquisition or device operation.

## Recorded three-output manifest checks

A local run on 2026-10-21 (Japan time), using source [3f8a9a89](https://github.com/kazuhide/drmr/commit/3f8a9a89d0fc84e9699939a193baf823cafb6127), completed the chapter 6 workflow with the ten-second clear HLS/TS fixture and two-second segment targets. The CLI ran on macOS ARM64. Apple mediastreamvalidator 1.25.36 ran natively in playlist-only mode. DASH used the official DASH-IF v3.1.0 XML schema and Schematron rules at revision `bdf0c27a8f198da7a4bf81634bb01faf7c7c3c06`, in a Linux ARM64 container with network access disabled.

| Final output | Protection and DRM declaration | Observed manifest result |
| --- | --- | --- |
| DASH fMP4 | CBCS; Widevine and PlayReady | Native Rust manifest checks, MPD XML schema and Schematron checks passed |
| HLS fMP4 | CBCS; FairPlay | Native Rust checks passed; Apple checked all three playlists; media playlists declared version 6, meeting minimum 6 |
| HLS TS | SAMPLE-AES; FairPlay | Native Rust checks passed; Apple checked all three playlists; media playlists declared version 6, meeting minimum 5 |

URL acquisition retained the original, and the local source server was stopped before saving the plan. Conversion and independent `verify --plan` both passed the complete metadata, encoded-packet, timing, decoded-content and current DRM checks. Separate manifest-only checks also passed for copies containing only MPD or M3U8 files, without media, initialization files or keys.

The native checks used HLS base and static DASH profiles at revision `2026-10-21.1`. All three manifest-only inputs also passed with an empty executable search path, independently of external validators. Reports contained no errors, no truncated diagnostics and no unchecked rules within their declared scope. Media and delivery requirements outside that scope remain separate evidence; this is not a claim of full Apple authoring conformance.

Changing an fMP4 media playlist to version 5 was refused; version 7 passed. These checks use the minimum required by the declared features, not equality with version 6. The retained source stayed unchanged, the version-test changes were restored, and no run-owned containers remained. This is one qualified fixture and tool environment; it does not establish license acquisition, DRM-device playback or arbitrary-input compatibility.

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
cargo test --locked -p drmr-core --test conversion_multi_drm \
  --test conversion_large_signaling -- --nocapture
cargo test --locked -p drmr-core --test conversion_bundle_encryption \
  --test conversion_bundle_publication -- --nocapture
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

## File and path values

`FILE` is a local filename supplied after the option, not inline JSON. `PATH` can name a file or directory as the row specifies. Relative option paths are resolved from the directory where you run the command. `NEW_FILE`, `NEW_PATH` and `NEW_DIR` name destinations that must be new; the workflow determines when they are written. Replace these placeholders with your paths; a `.json` extension does not make different configuration schemas interchangeable.

For example, `--fetch-policy config/fetch.json` reads the [Fetch policy JSON in chapter 8](https://kazuhide.github.io/drmr/guide/#url-input). `--keys-file config/keys.json` reads the [private key JSON examples in chapter 5](https://kazuhide.github.io/drmr/guide/#key-configuration): version 1 uses `input` and one `output`, while version 2 shares `input` and names each result under `outputs`. `analyze` validates either schema and uses only its source keys. `--outputs` and version-2 saved plans require version-2 key JSON even when the output list has one entry; the older single-result mode uses version 1.

For common commands, `--packager-bin "./tools/packager"` selects one installed executable. A Windows example is `--packager-bin "C:\Media Tools\packager.exe"`. Quote paths containing spaces and supply only the program name/path, without a shell command or appended arguments. The default `packager` is found on the launch environment's PATH; CLI archives and Desktop bundles do not include it. See [Media tools in chapter 4](https://kazuhide.github.io/drmr/guide/#installation). The prepared-DASH `run` workflow also accepts a containing directory; the legacy tables below distinguish that case.

For example, prepare the version-1 output key configuration from chapter 5 and select the installed executable for a conversion. Replace the input, new output, configuration and executable paths with the ones in your environment:

```sh
drmr convert original.mp4 --to cbcs --format mp4 \
  --output protected.mp4 --keys-file config/keys.json \
  --packager-bin "/opt/media-tools/packager"
```

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
| `INPUT` | Complete local MP4, DASH or HLS; supported MP4, static DASH or completed HLS URL; [URL profiles and local entrypoints](#url-input) |
| `--download-dir NEW_DIR` | Required new directory for URL input; retain the original privately; [acquisition example](https://kazuhide.github.io/drmr/guide/#url-input) |
| `--fetch-policy FILE` | Owner-only fetch policy JSON, e.g. config/fetch.json; [origin/header/limit fields and example](https://kazuhide.github.io/drmr/guide/#url-input) |
| `--keys-file FILE` | Owner-only version-1 or version-2 JSON input keys when source checks need decryption; [input key fields and examples](https://kazuhide.github.io/drmr/guide/#key-configuration) |
| `--packager-bin FILE` | Shaka Packager executable name/path; default packager on PATH; [installation and discovery](https://kazuhide.github.io/drmr/guide/#installation) |
| `--max-workers N` | Upper bound 1-64; otherwise automatic CPU/memory choice |
| `--json` | One input-analysis report; input_admitted does not verify a converted output |
| `--measure` | Add numeric measurements; requires --json |

Local input refuses download/fetch options. URL analysis always requires a new download directory.

## plan

| Argument/option | Meaning |
| --- | --- |
| `INPUT` | Local input for generic conversion intent |
| `--to TARGET` | Required for INPUT without --outputs: clear, cenc, cbcs, hls-aes128, hls-sample-aes |
| `--format FORMAT` | mp4, dash, hls, hls-ts; alternatives without --save, one resolved format in a single-output plan; omitted follows MP4/DASH/HLS, with HLS meaning fMP4; conflicts with --outputs |
| `--outputs FILE` | Schema-version-1 output recipe JSON, e.g. outputs.json; requires --save and replaces --to/--format; produces a version-2 execution plan; [output recipe example](https://kazuhide.github.io/drmr/guide/#inspect-plan) |
| `--json` | Versioned plan without execution |
| `--save NEW_FILE` | New execution-plan JSON written by drmr, e.g. conversion.plan.json; requires INPUT, --output and either --to or --outputs; [saving and reusing a plan](https://kazuhide.github.io/drmr/guide/#inspect-plan) |
| `--output NEW_PATH` | New result file/directory fixed by --save; refused without --save; [plan destinations](https://kazuhide.github.io/drmr/guide/#inspect-plan) |
| `--keys-file FILE` | Owner-only source-check key JSON; version 2 with --outputs, otherwise version 1; only with --save; [key fields and examples](https://kazuhide.github.io/drmr/guide/#key-configuration) |
| `--packager-bin FILE` | Shaka Packager executable name/path for source checks; only with --save, default packager; [tool installation](https://kazuhide.github.io/drmr/guide/#installation) |
| `--max-workers N` | Upper bound 1-64 for source checks; only with --save |
| `--measure` | Add measurements to stdout; requires --json and --save; saved file stays an execution plan |
| `--mpd MPD` | Local MPD file for the separate prepared-DASH rendition-listing mode; [prepared layout and example](https://kazuhide.github.io/drmr/guide/#prepared-dash-batch) |
| `--asset-id ID` | Asset name in MPD mode |

## convert

| Argument/option | Meaning |
| --- | --- |
| `INPUT` | Supported local original or MP4, static DASH or completed HLS URL; encrypted input requires source keys; omitted with --plan; [URL profiles](#url-input) |
| `--to TARGET` | Required output protection without --plan or --outputs |
| `--format FORMAT` | One output format; HLS defaults to fMP4 even for TS input |
| `--outputs FILE` | Schema-version-1 output recipe JSON; all listed results use one new result root; conflicts with --to/--format/--plan; [output recipe example](https://kazuhide.github.io/drmr/guide/#inspect-plan) |
| `--output NEW_PATH` | Required new result file/directory without --plan; [destination rules and examples](https://kazuhide.github.io/drmr/guide/#local-conversion) |
| `--plan FILE` | Execution-plan JSON generated by plan --save, version 1 or 2; refuse source/output-intent overrides; [saved-plan workflow](https://kazuhide.github.io/drmr/guide/#inspect-plan) |
| `--keys-file FILE` | Owner-only key JSON; version 2 with --outputs or a version-2 plan, otherwise version 1; [complete key JSON examples](https://kazuhide.github.io/drmr/guide/#key-configuration) |
| `--download-dir NEW_DIR` | Mandatory new directory for URL input; retained original; [acquisition example](https://kazuhide.github.io/drmr/guide/#url-input) |
| `--fetch-policy FILE` | Owner-only URL fetch policy JSON, e.g. config/fetch.json; [origin/header/limit fields and example](https://kazuhide.github.io/drmr/guide/#url-input) |
| `--packager-bin FILE` | Shaka Packager executable name/path; default packager on PATH; [installation and discovery](https://kazuhide.github.io/drmr/guide/#installation) |
| `--hls-validator-bin FILE` | Apple executable name/path for requested HLS manifest checks; default mediastreamvalidator; [external validator setup](https://kazuhide.github.io/drmr/guide/#verification) |
| `--dash-validator-bin FILE` | PHP executable name/path for the local DASH-IF manifest adapter; default php; [external validator setup](https://kazuhide.github.io/drmr/guide/#verification) |
| `--max-workers N` | Upper bound 1-64; otherwise automatic CPU/memory choice |
| `--json` | One versioned result; suppress ordinary progress lines |
| `--measure` | Add measurement fields; requires --json |

There are no generic `ffmpeg-bin` or `ffprobe-bin` options. Those tools are found on the launch environment's PATH.

`--plan` conflicts with INPUT, --to, --format, --outputs, --output, --download-dir and --fetch-policy. Runtime keys, validator/tool paths, worker limits and reporting options remain separate. Each output's saved manifest-check policy cannot be overridden at execution.

## verify

| Argument/option | Meaning |
| --- | --- |
| `OUTPUT` | Candidate MP4, MPD or M3U8; omitted with --plan |
| `--source INPUT` | Required original media/manifest file without --plan or --manifest-only; never inferred from the result; [complete comparisons](https://kazuhide.github.io/drmr/guide/#verification) |
| `--plan FILE` | Generated execution-plan JSON, version 1 or 2; check recorded source, output format/protection and independent media correspondence; conflicts with OUTPUT and --source; [saved-plan workflow](https://kazuhide.github.io/drmr/guide/#inspect-plan) |
| `--manifest-only` | Native HLS/DASH manifest linting with media_verified false; conflicts with --plan/--source/--keys-file |
| `--external-validator` | Add Apple/DASH-IF checks; requires --manifest-only |
| `--manifest-profile PROFILE` | Native manifest profile; requires --manifest-only. Default base; apple-vod accepts HLS only |
| `--keys-file FILE` | Owner-only original/result key JSON; version 2 for a version-2 plan, otherwise version 1; [input/output key roles and examples](https://kazuhide.github.io/drmr/guide/#key-configuration) |
| `--packager-bin FILE` | Shaka Packager executable name/path; default packager on PATH; [installation and discovery](https://kazuhide.github.io/drmr/guide/#installation) |
| `--hls-validator-bin FILE` | Apple executable name/path; default mediastreamvalidator; [external validator setup](https://kazuhide.github.io/drmr/guide/#verification) |
| `--dash-validator-bin FILE` | PHP executable name/path for the local DASH-IF adapter; default php; [external validator setup](https://kazuhide.github.io/drmr/guide/#verification) |
| `--max-workers N` | Upper bound 1-64; admission may reduce it |
| `--json` | One independent verification report |
| `--measure` | Add measurements; requires --json |

Verification does not rebuild the result using a new target/format.

Version-1 plan verification adds plan_id, source_unchanged and output_matches_plan, with complete comparisons under verification. Success requires source_unchanged, output_matches_plan and verification.passed all to be true. An explicitly supplied PlayReady configuration is also checked against the current result. Version-2 results list every output's media, protection, signaling and conformance outcomes and include an overall passed flag. Matching media alone does not establish planned protection or DRM declarations. Neither mode proves license playback.

## run

| Option | Meaning/default |
| --- | --- |
| `--mpd MPD` | Required local prepared DASH MPD file; [Prepared input layout](https://kazuhide.github.io/drmr/guide/#prepared-dash-batch) |
| `--output ROOT` | Required; results under ROOT/asset-id |
| `--asset-id ID` | Asset name, otherwise original directory name |
| `--hls-master M3U8` | Local original HLS master playlist file; needed unless --no-manifests; [prepared layout example](https://kazuhide.github.io/drmr/guide/#prepared-dash-batch) |
| `--packager-bin PATH` | Executable named packager (packager.exe on Windows), or its containing directory; [prepared-DASH tool override](https://kazuhide.github.io/drmr/guide/#prepared-dash-batch) |
| `--max-workers N` | Rendition parallelism upper bound |
| `--manifest-local-url-base URL` | Also generate manifests for local HTTP serving |
| `--log-format FORMAT` | text or jsonl; default text |
| `--no-round-trip` | Diagnostic: omit complete decode comparison |
| `--limit-renditions N` | Diagnostic: process only part of the rendition set |
| `--no-manifests` | Diagnostic: omit playlist/manifest generation |

## analyze-hls / package-hls

| analyze-hls option | Meaning |
| --- | --- |
| `--source-config FILE` | Required owner-only JSON with url and nested policy; [Capture a local source example](https://kazuhide.github.io/drmr/guide/#saved-hls) |
| `--asset-id ID` | Required saved asset name |
| `--output ROOT` | Required; save under ROOT/ID/clear/source |

| package-hls option | Meaning |
| --- | --- |
| `--analysis FILE` | Required analysis.json generated by a completed analyze-hls; [snapshot capture and packaging](https://kazuhide.github.io/drmr/guide/#saved-hls) |
| `--drm-config FILE` | Required owner-only signaling JSON; fields include widevine_pssh, playready_la_url and fairplay_uri; [Package with signaling example](https://kazuhide.github.io/drmr/guide/#saved-hls) |
| `--selection FILE` | Owner-only JSON with analyzed audio_source_id and optional analysis_id/snapshot_digest bindings; [audio selection details](https://github.com/kazuhide/drmr/blob/main/docs/usage.md#package-the-hls-snapshot) |
| `--packager-bin FILE` | Shaka Packager executable name/path, default packager on PATH; [tool installation](https://kazuhide.github.io/drmr/guide/#installation) |
| `--max-workers N` | Shared track/segment bound 1-64 |

Content keys for these workflows are `DRM_KID`/`DRM_KEY`/`DRM_IV`. Passing generic key JSON as drm-config is not supported.

## verify-snapshot / canonical / version

| Command/argument | Meaning |
| --- | --- |
| `verify-snapshot PATH` | Check recorded digest for the generated snapshot directory or analysis.json; [snapshot example](https://kazuhide.github.io/drmr/guide/#saved-hls) |
| `verify-snapshot --files PATH` | Also rehash saved playlists/media in that snapshot; [snapshot example](https://kazuhide.github.io/drmr/guide/#saved-hls) |
| `canonical PATH` | Read a JSON file and print stable JSON; no final LF or secret removal; [JSONL construction example](https://kazuhide.github.io/drmr/guide/#prepared-dash-batch) |
| `version` | Print version; no command-specific options |

## batch

| Option | Meaning/default |
| --- | --- |
| `--work-list FILE` | Required JSONL with a schema header and one asset per line; [Work-list JSON Lines example](https://kazuhide.github.io/drmr/guide/#prepared-dash-batch) |
| `--output ROOT` | Required output root |
| `--resume-log FILE` | Required JSONL attempt-state log written/reused by batch, e.g. resume.jsonl; [Keys and resumption](https://kazuhide.github.io/drmr/guide/#prepared-dash-batch) |
| `--storage STORAGE` | local or s3; default local |
| `--storage-root DIR` | Local object-key root |
| `--bucket NAME` | S3 bucket |
| `--region REGION` | Default and currently supported S3 region ap-northeast-1 |
| `--output-prefix PREFIX` | Storage result prefix, default output |
| `--key-provider PROVIDER` | env or file; default env |
| `--key-file FILE` | Owner-only per-asset JSONL with asset_id/kid/key/iv, no schema header; file provider only; [Keys and resumption example](https://kazuhide.github.io/drmr/guide/#prepared-dash-batch) |
| `--hls-config-dir DIR` | Directory of per-asset HLS JSON configuration references; [HLS batch configuration](https://kazuhide.github.io/drmr/guide/#prepared-dash-batch) |
| `--packager-bin PATH` | Packager executable path; prepared-DASH batches also accept its directory; [workflow-specific override](https://kazuhide.github.io/drmr/guide/#prepared-dash-batch) |
| `--max-workers N` | Per-asset processing bound; assets remain serial |
| `--retry-failed` | Select failed attempts |
| `--plan` | Show intended work without executing |
| `--max-attempts N` | Per-asset attempt cap, default 3 |
| `--per-asset-timeout-s SEC` | Per-asset budget, default 3600 |
| `--heartbeat-s SEC` | Status interval, default 60 |

## serve

| Option/environment | Meaning/default |
| --- | --- |
| `--static-dir DIR` | Trusted directory of UI files replacing embedded assets; [local console setup](https://kazuhide.github.io/drmr/guide/#web-console) |
| `--cli FILE` | Same-version drmr executable file for jobs (drmr.exe on Windows); [console CLI selection](https://kazuhide.github.io/drmr/guide/#web-console) |
| `DRMR_CATALOG_ROOT` | Required prepared catalog root |
| `DRMR_OUTPUT_ROOT` | Required results/saved-HLS/log root |
| `DRMR_BIND_HOST` | Default loopback; remote exposure is not intended |
| `DRMR_BIND_PORT` | Default 8080 |
| `DRMR_MAX_WORKERS` | HLS packaging worker upper bound |
| `DRMR_HLS_PROFILES` | Owner-only named-profile JSON file path; [HLS connection policy JSON example](https://kazuhide.github.io/drmr/guide/#web-console) |

# 20. References and maintenance

## Current public documentation

These links follow public main. For a different installed version, select its source revision or accompanying documentation.

- [README and installation](https://github.com/kazuhide/drmr/blob/main/README.md)
- [Usage and configuration](https://github.com/kazuhide/drmr/blob/main/docs/usage.md)
- [Conversion profiles](https://github.com/kazuhide/drmr/blob/main/docs/conversion-support.md)
- [Processing and complete verification](https://github.com/kazuhide/drmr/blob/main/docs/media-processing.md)
- [Processing routes and planning](https://github.com/kazuhide/drmr/blob/main/docs/processing-routes.md)
- [Multiple outputs and DRM settings](https://github.com/kazuhide/drmr/blob/main/docs/multiple-outputs.md)
- [External manifest validation](https://github.com/kazuhide/drmr/blob/main/docs/manifest-validation.md)
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

This guide summarizes public source, help, documentation and stated qualification limits. Example paths/domains and angle-bracket key placeholders must be replaced with your own authorized media and configuration before executing commands.
