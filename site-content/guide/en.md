---
title: User guide
updated: 2026-11-03
version: 1.2.0
---

# 1. Introduction

This guide uses the 1.2 CLI and output-configuration schema 4. Earlier names and key files need the [1.2 migration](https://github.com/kazuhide/drmr/blob/main/docs/releases.md#version-12-migration).

drmr changes media containers, delivery formats and encryption while retaining encoded video and audio. It is for engineers who control the media and already have the required decryption keys. It is not an encoder for creating a new quality ladder or a client for obtaining DRM licenses.

This guide describes the current source and its supported operations. For a downloaded CLI, also read the documentation packaged with that release. Public source and command help define the behavior. A passing example does not qualify arbitrary media or playback devices.

Download a versioned CLI from [GitHub Releases](https://github.com/kazuhide/drmr/releases) and check its archive against `SHA256SUMS` before installation. External media tools remain separate. See [releases and compatibility](https://github.com/kazuhide/drmr/blob/main/docs/releases.md) for supported targets and the compatibility policy.

Check each release's **Pre-release** label and notes before use. For a source build, follow [CLI installation](#installation). CLI compatibility and release qualification are separate. The local Web console and Desktop host remain experimental.

## Choose an operation

| Goal | Command | What it establishes |
| --- | --- | --- |
| Find external tools | `doctor` | Required executables can be found in the launch environment |
| Check versions and options | `doctor --probe` | Version and help output advertise required features; no media is processed |
| Read input declarations | `analyze --metadata-only` | Local or HTTP(S) MP4, DASH or HLS metadata; no keys or complete sample proof |
| Check complete source media | `analyze` | Actual input admission; URL input is retained in an explicit download directory |
| Describe a conversion | `plan` | Requested formats and protection, candidate routes and runtime conditions |
| Save a conversion request | `plan --save` | Source fingerprints and one output, or an explicit list of outputs with DRM selections and manifest-check policies |
| Convert media | `convert` | Actual input admission, conversion and comparison before publishing new output |
| Compare independently | `verify` | Reads original and result independently and checks complete media and timing |
| Check only final manifests | `verify --manifest-only` | Native HLS/DASH linting; add --external-manifest-check for external checks; no media comparison or decryption |

Use direct `convert INPUT --output-config outputs.json` for conversion with its built-in checks. Use the separate operations when you need a preflight, a saved request or a later independent comparison. Add `--metadata-only` to `analyze` for a quick declaration check.

Native Rust validation is the default for these four complete-media operations. It reads every selected sample or access unit, codec configuration, clock and supported playback window. Add `--decode` for complete decoding as an additional requirement. Conversion and fresh verification independently read the original and actual output; neither trusts only an earlier report. Unsupported or incomplete evidence is refused at both depths. Native equality does not establish that every compressed payload can be decoded; the optional decoder obtains that distinct evidence. Metadata-only analysis, planning without `--save` and manifest-only verification refuse `--decode`.

## First conversion

For one output or several, describe the requested formats and encryption in `outputs.json` and pass it to `convert`. Add a private `keys.json` only when input or output is encrypted. See [key settings](#key-configuration), [output settings](#inspect-plan), and the [URL conversion examples](#url-input).

Save this as `outputs.json` to request one unencrypted MP4:

```json
{
  "schema_version": 4,
  "outputs": [
    {
      "id": "mp4",
      "drms": [],
      "output_format": "mp4",
      "encryption": "clear"
    }
  ]
}
```

Use a supported unencrypted `original.mp4`. Choose a new `result` directory under an existing parent:

```sh
drmr convert original.mp4 --output-config outputs.json \
  --output-dir result
```

The entrypoint is `result/mp4/media.mp4`. The output ID `mp4` names its directory. Clear input and output need no key file. A single `convert` checks the source, converts it and compares the result with the original. HLS/DASH also receive native manifest checks; explicitly selected DRM declarations are checked too. Separate `analyze`, `plan` and `verify` commands are optional.

Use `analyze` for a separate preflight, `plan --save` to fix conditions for later execution, and `verify --plan-file` to read a completed result again. Chapters 6 and 9 explain those workflows.

The CLI is the main interface. Docker provides media tools in one environment. The experimental local Web console and Desktop host share analysis, plan saving, conversion and verification, but do not expose every CLI command.

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

HLS can contain TS or fMP4. `--output-format hls-fmp4` selects fMP4 HLS output; `--output-format hls-ts` selects TS HLS. Knowing that an input is HLS does not establish its media container.

## Select protection separately

AES is a symmetric encryption algorithm. CTR means counter mode; CBC means cipher block chaining. A sample is an encoded unit such as a video picture or audio frame.

| `--encryption` | Encryption unit | Output choices |
| --- | --- | --- |
| `clear` | No encryption | Supported MP4, DASH, fMP4 HLS and TS HLS profiles |
| `cenc` | AES-CTR applied to MP4 samples | MP4, DASH and fMP4 HLS |
| `cbcs` | AES-CBC sample encryption, including a supported video block pattern | MP4, DASH and fMP4 HLS |
| `hls-aes128` | AES-CBC applied to complete HLS resources | fMP4 HLS and TS HLS |
| `hls-sample-aes` | HLS sample encryption inside supported TS media | TS HLS |

AES-128 HLS is not an MP4 sample-protection scheme. `cens` and `cbc1` may be identified but are not conversion targets. Not every format/protection combination is supported.

## DRM systems are a separate layer

Widevine, PlayReady and FairPlay involve signaling, key exchange and device behavior beyond selecting an AES mode. A KID identifies a key; it is not the secret content key.

Common `plan`, `convert` and `verify` commands accept an output list for DASH with Widevine/PlayReady and fMP4 HLS with Widevine/PlayReady/FairPlay. Protection System Specific Header (PSSH) data carries DRM initialization information; it is separate from the encrypted media and from a license. Chapter 5 describes the runtime metadata, and chapter 6 describes the output list. The single-output PlayReady configuration remains available. Successful known-key verification does not prove a license service or device will play the result.

# 3. Supported input and output profiles

The qualified DASH/fMP4 HLS video-and-audio-to-MP4 extension below is development for 1.3. It is not in the 1.2.0 archives; see [Unreleased changes](https://github.com/kazuhide/drmr/blob/main/CHANGELOG.md#unreleased).

MP4 and fMP4 HLS/DASH accept the implemented AVC (H.264), HEVC (H.265), VP9 and AAC profiles. Transport stream video still requires H.264. HEVC supports single-layer, progressive Main/Main10 with 4:2:0 chroma and matching 8-bit or 10-bit luma/chroma depths. It requires one stable set of video, sequence and picture decoding parameters (VPS/SPS/PPS). See [HEVC validation and limits](https://github.com/kazuhide/drmr/blob/main/docs/verification.md#hevc-in-mp4-and-fmp4) for the exact accepted syntax and refusal conditions.

HEVC and VP9 conversion and verification for the qualified MP4/fMP4 profiles are available from v1.1.0. The v1.0.3 release archives do not include these codecs. Metadata-only identification of AV1, AC-3, E-AC-3, Opus or FLAC does not establish complete validation. Full operations refuse unimplemented codec profiles, even with `--decode`. Container and encryption tables do not qualify every codec those formats can carry.

VP9 (`vp09`) supports clear/CENC MP4 and fMP4 HLS/DASH. Outer HLS AES-128 is separate; VP9/CBCS fails with `vp9-protection-scheme-not-supported`. See [VP9 validation and limits](https://github.com/kazuhide/drmr/blob/main/docs/verification.md#vp9-in-mp4-and-fmp4). Generic HLS can pass base checks while the Apple VOD profile rejects VP9 under section 1.1.

Analysis marks compatible tail-aligned input with `vp9-cenc-prefix-normalization-required`; conversion writes canonical prefix alignment.

## Accepted input containers

Choose an input and an output independently within these profiles. Codec, track, clock, playback-window and encryption-layout conditions still apply. URL acquisition has its own bounds.

| Input | Accepted protection | Track and output conditions |
| --- | --- | --- |
| MP4 | clear, cenc, cbcs | Joint MP4 output is conditional; TS output requires fragmented input |
| DASH / fMP4 | clear, cenc, cbcs | Each representation has one track; development for 1.3 also admits one qualified video/AAC pair for MP4 |
| HLS / fMP4 | clear, cenc, cbcs, AES-128 | Each media playlist has one track; development for 1.3 also admits one qualified video/AAC pair for MP4 |
| HLS / TS with video | clear, AES-128, SAMPLE-AES | One video with optional audio per admitted program; MP4 output selects one video and associated audio |
| HLS / TS, audio only | clear, AES-128, SAMPLE-AES | One supported AAC-LC audio stream |

## Output formats and encryption

| `outputs[].output_format` | `outputs[].encryption` choices | Generated entrypoint under the output ID |
| --- | --- | --- |
| `mp4` | `clear`, `cenc`, `cbcs` | `media.mp4` |
| `dash` | `clear`, `cenc`, `cbcs` | `manifest.mpd` |
| `hls-fmp4` (fMP4) | `clear`, `cenc`, `cbcs`, `hls-aes128` | `master.m3u8` |
| `hls-ts` (MPEG-TS) | `clear`, `hls-aes128`, `hls-sample-aes` | `master.m3u8` |

The home-page matrix links each input/output choice to its settings and command. Joint MP4 output accepts qualified tracks from one MP4 or one video and one audio from HLS TS. Development for 1.3 also accepts one video and one AAC track from the same DASH/fMP4 HLS presentation. Its rendition metadata must match drmr's MP4-to-manifest mapping; custom labels or selection controls that this route cannot retain are refused. Use `auto` or `stitch`; the result is fragmented MP4. See the [exact conditions](https://github.com/kazuhide/drmr/blob/main/docs/conversion-support.md#fragmented-presentations-to-one-mp4). Unrelated inputs and ambiguous choices remain refused.

## HLS input protection

| Container | Input protection | Required source keys |
| --- | --- | --- |
| fMP4 | clear, cenc, cbcs, AES-128 | cenc/cbcs by KID; AES-128 by the exact playlist key URI |
| TS | clear, AES-128, SAMPLE-AES | Encrypted inputs by key URI; SAMPLE-AES also needs its associated IV |

Each fMP4 media playlist must contain one video or audio track. A master can associate separate video and audio playlists. Unsupported rendition metadata and inconsistent audio-group associations are refused.

## Explicit DRM profiles in an output list

With `auto`/`stitch`, DRM output requires complete source admission for a supported MP4, DASH or HLS profile, including its codec, protection and playback window. Supply known keys for encrypted input. Metadata-only inspection is insufficient. With `segments`, fMP4 stays fMP4 for DASH/HLS and TS FairPlay keeps TS.

| `output_format` and `encryption` | DRM systems | Input with auto/stitch |
| --- | --- | --- |
| `dash`, `cenc` or `cbcs` | Widevine, PlayReady, or both | Qualified MP4, DASH or HLS |
| `hls-fmp4`, `cenc` | Widevine, PlayReady, or both | Qualified MP4, DASH or HLS |
| `hls-fmp4`, `cbcs` | Widevine, PlayReady, FairPlay, or any combination | Qualified MP4, DASH or HLS |
| `hls-ts`, `hls-sample-aes` | FairPlay | Qualified TS or fragmented MP4; encrypted input needs known keys |

`hls-fmp4` produces fragmented MP4. `hls-ts` produces transport streams. One output list can select either HLS container alongside DASH, or request both HLS containers. Each output has its own named directory and runtime key settings. The list does not imply that every DRM system can be combined with every encryption scheme.

## Finite audio presentation windows

This is an MP4 playback-metadata profile, not a separate codec. An edit list can specify the exact decoded-audio start and duration, excluding encoder warm-up and final padding.

The complete progressive source must contain one AAC-LC audio track (`mp4a`), optionally with one H.264 (`avc1`), qualified HEVC Main/Main10 (`hvc1`/`hev1`) or VP9 (`vp09`) video track. The audio edit must be one normal-rate continuous window, have a nonnegative start and convert exactly into integer audio clock units.

VP9 uses clear or CENC samples. The CBCS choices below apply to the other qualified codecs.

The qualified clear, cenc and cbcs inputs share the output choices above: MP4/DASH clear, cenc or cbcs; fMP4 HLS also permits AES-128.

Clear output from a progressive MP4 retains its sample tables. Encrypted MP4 is fragmented and can retain the two qualified tracks in one file. DASH/HLS uses separate tracks. Every encrypted source track needs its own key mapping.

Progressive MP4 copying and decryption run in Rust, using a private copy with each sample at its original position. Clear sample types and zeroed padding replace protection metadata. Sample positions, clocks and playback edits stay unchanged. This copy uses no FFmpeg, decoding or new segmentation.

A complete joint fragmented MP4 can also be converted again to clear, cenc or cbcs MP4. It requires exactly one supported video and one AAC-LC audio track, matching protection, and one positive normal-rate finite edit per track. Shaka Packager processes each track; drmr restores the original playback windows and independently checks the combined result. Clear output stays fragmented. This does not combine separate source files.

This progressive-input profile refuses multiple edits, initial empty edits, rate changes, extra tracks, unrelated-file combinations, key rotation and input DRM initialization headers. CENC accepts supported 8/16-byte sample-IV layouts; CBCS accepts supported 16-byte constant-IV and pattern layouts. This does not admit arbitrary encrypted MP4.

## TS admission

The TS profile uses one fixed program, one progressive H.264 video stream with optional 44.1 or 48 kHz AAC-LC audio. MP4, DASH, fMP4 HLS and TS output also accept audio-only input under the same single-program, 44.1 or 48 kHz AAC-LC conditions.

TS packets must be 188 bytes; H.264 decoder configuration must remain fixed. AAC uses seven-byte ADTS transport headers without a checksum and one raw data block. Multiple programs, configuration changes, corrupt packets, clock wrap, discontinuities, interlaced video and unsupported descriptors are refused. The sample rate and channel configuration must remain unchanged throughout the track.

For an AAC-only rendition, the final `EXTINF` may end inside its last frame when its precision identifies one exact audio-sample boundary. drmr keeps the complete encoded frame and preserves the shorter playback range in MP4, HLS or DASH. Ambiguous precision, a partial nonfinal segment, and removing a whole frame are refused. An external audio rendition can accompany video; a multiplexed playlist does not independently declare an audio trim. TS-to-MP4 writes standard playback edits for this exact range, including when audio accompanies video. The generated MP4 can be analyzed, independently verified and converted to DASH or fMP4 HLS. Audio-only input can also be re-encrypted as MP4. Every accepted edit is bound to all coded frames and clocks; other edit layouts and mismatches are refused.

SAMPLE-AES accepts one effective `identity` or `com.apple.streamingkeydelivery` key format, with version 1 when declared. Distinct simultaneously active key formats and unsupported method changes are refused. Nonzero audio priming is outside the profile. Joint TS output uses separate audio/video playlists and a master; it does not multiplex both into one output segment.

## Fragmented MP4 to TS

Fragmented MP4 files and DASH/fMP4 HLS can produce Clear, AES-128 or SAMPLE-AES TS with known input keys when encrypted. `auto`/`stitch` joins and segments again; `segments` refuses this container change. Native verification compares all coded frames, clocks and audio/video relationships.

Initial AAC warm-up trimming uses MP4 edits that this TS profile cannot represent; it fails with `ts-output-presentation-not-representable`. A supported shorter final AAC range from TS-derived fMP4 is retained in the output playlist.

## TS to a single MP4

TS-to-MP4 supports audio only, video only, or one of each, preserving coded samples, clocks and supported final AAC playback ranges without decoding. Restrict a multi-quality master to the chosen video and associated audio before analysis. A video-only media playlist produces video-only MP4.

Supported audio-language metadata is retained, including equivalent `en`/`eng` spellings. HLS group identifiers, names and automatic-selection flags describe playlist choices and are not MP4 selection declarations. Unsupported language mappings and ambiguous video selections are refused.

## Common exclusions and bounds

Local DASH requires one static Period and finite template, list or single-file index references confined to the source directory. Templates can use a nominal duration or an explicit timeline; lists can name objects or supported byte ranges. Single-file indexes must be flat and refer to the same media object. Live/dynamic input, multiple periods, HLS byte ranges, discontinuities, hierarchical or external DASH indexes, and unsupported events or rendition metadata are refused. Subtitle tracks and arbitrary track selection remain outside the conversion profile.

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
| `ffmpeg` / `ffprobe` | 9.0.1 | Additional complete decoding with --decode |
| `packager` | Shaka Packager 3.8.0 | Sample encryption, container changes and segmentation |

Obtain trusted tools and put their executables on PATH. CLI and Desktop do not bundle them; Docker does. Native `analyze`, `plan --save` and `verify` need no external media tools. Conversion uses Shaka Packager on packaging routes; progressive MP4 copying and decryption run in Rust. FFmpeg and ffprobe are used only when complete decoding is required. Their builds must provide the required options and complete packet clocks.

## Discovery versus probing

```sh
drmr doctor
drmr doctor --probe --route all --json
drmr doctor --probe --route dash-presentation --decode
```

Default `doctor` discovers paths without executing tools. `--probe` checks required tools' version and help output, with three seconds per call and 1 MiB per stdout/stderr stream. Optional tools are not launched. Raw output is omitted. Media, keys, licenses and playback devices are not validated.

`doctor` checks native conversion requirements by default. `progressive-mp4` and segment routes need no external tools; packaging routes and the default `all` require Shaka Packager. `--decode` adds FFmpeg/ffprobe and TS decoder preparation tools. Metadata plans list native tools in `required_tools` and decoding conditions in `condition_codes`. Each diagnostic tool's `required` reflects the selection. Unconfirmed process cleanup prevents readiness. `ready` covers these diagnostics; `media_compatibility_checked` stays false. Desktop and terminal PATH values may differ.

See [requirements](https://github.com/kazuhide/drmr/blob/main/docs/requirements.md) and [Windows](https://github.com/kazuhide/drmr/blob/main/docs/windows.md).

# 5. Key configuration and confidentiality

## Files and key roles

Use schema-2 `keys.json` for every command. Quick single-output commands select `outputs.default`; an output configuration selects each explicit output ID. Input keys decrypt the original; output keys encrypt and verify each result. The output ID in `outputs.json` must match the corresponding member of `keys.json.outputs`. These are separate files, with independent schema versions. A saved plan is a third file generated by `plan --save`; it does not contain keys.

| Conversion | `keys.json` content |
| --- | --- |
| clear → clear | No key file and no `--keys-file` |
| clear → encrypted | Only `outputs` for encrypted output IDs; omit `input` |
| encrypted → clear | `input` and an empty `outputs` object |
| encrypted → encrypted | `input` and `outputs` for encrypted output IDs |

`analyze` and `plan --save` need only the input keys when the original is encrypted. `convert` and later `verify --plan-file` need the input and output keys that their work requires. Keys are not filled from environment variables. Supply every encrypted source track's key, including separate video/audio KIDs.

A KID (key identifier) selects a key; it is not the secret key. Keys and KIDs use 16 bytes written as 32 hexadecimal digits. An IV (initialization vector) sets the encryption starting state. Published metadata can contain KIDs and media IVs.

Use an owner-only work directory. On macOS/Linux, protect a new file before entering secrets; for an existing private `keys.json`, use `chmod 600 keys.json`. Chapter 12 explains Windows ACLs. The CLI refuses unsafe files, symbolic links, multiple hard links, duplicate or unknown fields, unsupported schemas and oversized settings. Keep keys outside source/result directories and version control.

Configuration errors can show known field positions and required formats without private values. See [correcting settings](https://github.com/kazuhide/drmr/blob/main/docs/configuration.md#correcting-a-configuration-error).

## Complete named-output key examples

The following values are public test data. Replace them with your own keys and identifiers. For clear input and a `dash` output using cenc or cbcs, save this complete file as `keys.json`:

```json
{
  "schema_version": 2,
  "outputs": {
    "dash": {
      "kid": "000102030405060708090a0b0c0d0e0f",
      "key": "00112233445566778899aabbccddeeff"
    }
  }
}
```

Quick `--encryption`/`--output-format` commands use the same file with `default` instead of the output member `dash`.

For cenc/cbcs input and a clear output, the complete file instead contains only input keys and the required empty output map:

```json
{
  "schema_version": 2,
  "input": {
    "keys": [{"kid": "000102030405060708090a0b0c0d0e0f", "key": "00112233445566778899aabbccddeeff"}]
  },
  "outputs": {}
}
```

| Field | Required when | Meaning |
| --- | --- | --- |
| `schema_version` | Always | `2` for every CLI key file |
| `input.keys` | cenc/cbcs input | Array of complete `kid`/`key` mappings |
| `input.hls_keys` | AES-128 or TS SAMPLE-AES input | Array of exact key URI and key mappings; associated IV when needed |
| `outputs` | Always in schema 2 | Object keyed by encrypted output ID; empty when every output is clear |
| `outputs.ID.kid` | cenc/cbcs output | MP4 key identifier; omit for AES-128 and TS SAMPLE-AES |
| `outputs.ID.key` | Every encrypted output | Content key |
| `outputs.ID.hls_key_uri` | AES-128, TS SAMPLE-AES, generic encrypted HLS or FairPlay | Declaration written to playlists; not a destination where drmr publishes a key |
| `outputs.ID.iv` | TS SAMPLE-AES output | Associated IV; refused for other targets |
| `outputs.ID.signaling` | Explicit DRM selected in `drms` | Matching DRM initialization information; chapter 7 has a complete example |

Do not add entries for clear outputs. Missing or extra output IDs are refused. Generic cenc/cbcs HLS with `drms: []` needs a key URI; explicit Widevine/PlayReady-only HLS does not. FairPlay uses `skd://`.

## HLS input keys and output IVs

For AES-128 input, the URI must match the playlist's resolved key identifier. A relative `keys/content.key` in `https://example.com/media.m3u8` resolves to `https://example.com/keys/content.key`. Absolute `skd:` identifiers remain unchanged. drmr does not fetch keys or licenses.

This complete schema-2 file decrypts AES-128 input to a clear output:

```json
{
  "schema_version": 2,
  "input": {
    "hls_keys": [{"uri": "https://example.com/keys/content.key", "key": "00112233445566778899aabbccddeeff"}]
  },
  "outputs": {}
}
```

For TS SAMPLE-AES input, use the actual URI and its associated IV. This is a complete clear-output example:

```json
{
  "schema_version": 2,
  "input": {
    "hls_keys": [{"uri": "skd://source-content", "key": "00112233445566778899aabbccddeeff", "iv": "000102030405060708090a0b0c0d0e0f"}]
  },
  "outputs": {}
}
```

A declared SAMPLE-AES IV must match the configured associated IV. A sequence number is not a substitute. AES-128 instead follows the declared IV or the HLS sequence-derived IV rules.

AES-128 output uses `key` and `hls_key_uri`, without `kid` or `iv`; drmr creates a new IV for each output resource. TS SAMPLE-AES output uses `key`, `iv` and `hls_key_uri`, without `kid`. Its video/audio use the same configured output key and IV. The [conversion recipes](#url-input) provide complete output settings and input-specific key files for each case.

## DRM information and compatibility formats

Widevine PSSH (Protection System Specific Header) must be a complete box in hexadecimal or Base64, with matching output KIDs and, when declared, the selected cenc/cbcs scheme. PlayReady information is generated from the KID and license URL: CENC uses an AESCTR 4.1.0.0 header and key checksum; CBCS uses an AESCBC 4.3.0.0 header without that checksum. Chapter 7 shows complete JSON for DASH Widevine/PlayReady and HLS with all three DRMs.

In fMP4 HLS, CBCS is declared as `SAMPLE-AES`, CENC as `SAMPLE-AES-CTR`. `KEYFORMAT` distinguishes the DRM systems. FairPlay needs CBCS and a `skd://` identifier; it cannot be selected for CENC. The configured DRM set must match the requested output. These declarations do not establish license acquisition or device playback.

All CLI key files use schema 2, including quick conversion and older saved plans. A former single `output` becomes `outputs.default`. Configured output IDs select their matching keys. Fresh named-output verification accepts `verify RESULT --source INPUT --output-config outputs.json`. See the [configuration reference](https://github.com/kazuhide/drmr/blob/main/docs/configuration.md).

## Logs, memory and temporary files

Known key material is masked in retained tool logs and errors. Masking does not classify arbitrary filenames, URLs or metadata as secret. Keys are held in ordinary process memory. External tools can receive keys through process arguments/environment, visible to permitted operating-system inspection while running.

Some workflows create private temporary key/configuration files. Cleanup and zeroizing reduce retention but do not promise secure erasure, protection from the same user, crash-dump exclusion or hardware isolation. Treat original media, saved URLs, manifests and raw tool results as potentially sensitive. The local interfaces select protected key files without retrieving their contents in the browser.

See [local security](https://github.com/kazuhide/drmr/blob/main/docs/security.md).

# 6. Output settings, analysis and saved plans

## outputs.json and the saved plan

`outputs.json` is an output list that you create. `plan` and `convert` read it through `--output-config`; `plan` does not generate it. Use schema 4 for new configurations. This complete file requests one generic CBCS HLS output:

```json
{
  "schema_version": 4,
  "output_dir": "result",
  "outputs": [
    {
      "id": "hls",
      "drms": [],
      "output_format": "hls-fmp4",
      "encryption": "cbcs"
    }
  ]
}
```

| Field | Requirement or default | Meaning |
| --- | --- | --- |
| `schema_version` | Required, `4` | Independent of key-file and saved-plan schemas |
| `output_dir` | Optional | Result root relative to this JSON file; a direct CLI `--output-dir` overrides it |
| `outputs` | Required, 1–8 entries | Each requested output; one entry also uses this interface |
| `outputs[].id` | Required, unique | Result directory and matching key-file output name; 1–32 lowercase ASCII characters, letter first, then letters/digits/hyphens; reserved device names refused |
| `outputs[].output_format` | Required | `mp4`, `dash`, `hls-fmp4` or `hls-ts` |
| `outputs[].encryption` | Required | Protection from chapter 3's output choices |
| `outputs[].drms` | Required | `[]` for no service-specific signaling, or the supported DRM names from chapter 3 |
| `outputs[].processing` | Optional, CLI default `auto` | `auto`, `segments` or `stitch`; chapter 9 explains boundaries |
| `outputs[].dash_addressing` | DASH only; optional, CLI default `template` | `template`, `template-timeline`, `list` or `base` |
| `outputs[].external_manifest_check` | Optional, `false` | `true` requires additional external manifest checks; native checks always run |
| `outputs[].manifest_profile` | Optional, `base` | HLS/DASH rules; `apple-vod` is HLS only |

Each result is under `result/ID/`: `media.mp4`, `manifest.mpd` or `master.m3u8`. Clear outputs have no key-file entry. Unknown fields, duplicate IDs and unsupported combinations are refused. An empty `drms` array does not disable media or manifest checks. Requesting external checks requires the tools described in chapter 9; MP4 refuses `external_manifest_check: true`.

CLI relative paths use the working directory; JSON `output_dir` uses the configuration file's directory. A saved plan fixes the resolved destination. Explicit CLI manifest settings override every configured output and refuse incompatible profiles; per-output processing still overrides the CLI processing default. Edit the configuration rather than the generated plan. See the [1.2 migration](https://github.com/kazuhide/drmr/blob/main/docs/releases.md#version-12-migration).

## Read declarations without keys

```sh
drmr analyze --metadata-only media.mp4
drmr analyze --metadata-only source/manifest.mpd --json
drmr analyze --metadata-only source/master.m3u8 --json
drmr analyze --metadata-only \
  "https://example.com/vod/master.m3u8" --json
drmr analyze --metadata-only \
  "https://example.com/vod/manifest.mpd" --json
drmr analyze --metadata-only \
  "https://example.com/vod/media.mp4" --json
```

`analyze --metadata-only` reads MP4 metadata or DASH/HLS declarations without keys, a decoder or Shaka Packager. It identifies format, declared protection, known key identifiers and DRM labels, while excluding keys, IVs and key/license addresses from the report. It does not decrypt, decode or check every sample; `media_verified` remains false. Unknown protection stays unknown. An HLS SAMPLE-AES declaration alone does not establish an MP4 encryption scheme.

Local HLS follows bounded child playlists within its directory tree. URL HLS reads declarations and needed child playlists; URL DASH reads the MPD. Neither downloads initialization files, media segments, keys or licenses. URL MP4 uses HTTP Range requests for selected byte positions, validating responses before reading box headers and movie metadata (`moov`) while skipping media payloads (`mdat`). A server that ignores Range is refused instead of causing a full MP4 download.

URL metadata is temporary private data, not a saved original for a plan or offline conversion. No `--download-dir` is needed. Live HLS or dynamic DASH declarations may be readable, but this does not establish a complete on-demand source or conversion support. Normal URL analysis in chapter 8 retains and checks complete media.

An HLS master playlist lists variants and references their media playlists ([RFC 8216, Section 4.3.4.2](#references)). For local and URL metadata inspection, a referenced master playlist fails with `hls-nested-master-playlist` before its references are followed. This prevents recursive playlist graphs from multiplying retained analysis results.

Metadata-only refuses `--download-dir`, `--keys-file`, `--packager-bin`, `--max-workers` and `--stats`. URL input can use chapter 8's `--fetch-config`; local input cannot. Metadata parser limits and fetch-policy limits both apply. Local and URL MP4 inspection both cap `moov` metadata at 16 MiB and the parsed tree at 16,384 boxes, including nested boxes. Many small boxes require memory beyond their payload bytes; excess boxes are refused with `mp4-box-count-limit` before allocating another node.

The former `inspect INPUT` command is removed during the pre-release CLI consolidation. Replace it with `analyze --metadata-only INPUT`; no alias remains. See the [pre-release migration](https://github.com/kazuhide/drmr/blob/main/docs/releases.md#pre-release-cli-consolidation).

## Analyze complete media

```sh
drmr analyze source/master.m3u8 --json
drmr analyze encrypted.mp4 --keys-file config/keys.json --json
```

Without `--metadata-only`, `analyze` checks the actual complete source with the conversion engine, including every selected encoded sample, codec configuration, clock and supported playback window. Add `--decode` to require complete source decoding too. Its `input_admitted` result applies to the checked input profile; candidate output formats still need a target-specific plan. It does not certify a converted result or DRM playback. Local MP4, DASH and HLS are accepted within their supported profiles. Chapter 8 shows URL input with mandatory source retention.

Complete analysis checks each known source track directly, without matching it against a second copy of the same track set. MPEG-TS source checks read every encoded access unit and timestamp, bind them to the original transport-packet evidence, parse all admitted AVC slice headers and codec timing in Rust. Optional `--decode` decodes the complete content. All referenced renditions are checked. The separate `verify` operation continues to compare independently prepared source and output media; analysis results do not replace fresh checks in later commands.

When a documented source compatibility rule applies, `compatibility_notes` lists its fixed code. Examples include empty TS padding and adjacent HLS cue markers with no active interval. These rules retain the original bytes and all media and timing checks. See [transport-stream compatibility](https://github.com/kazuhide/drmr/blob/main/docs/media-processing.md#transport-stream-compatibility) for the exact accepted conditions.

## Plan a request

To save a supported URL source directly, retain its originals while creating the plan:

```sh
drmr plan "https://example.com/vod/master.m3u8" \
  --download-dir source --output-config outputs.json \
  --save conversion.plan.json
```

```sh
drmr plan media.mp4 --encryption cbcs --output-format dash \
  --json
drmr plan source/master.m3u8 --encryption clear \
  --output-format dash,hls-fmp4
```

Without `--save`, `plan` reports requested formats, key roles and candidate processing routes. Comma-separated formats in this mode describe alternatives. Use `--output-config` below to request several actual outputs. Every requested format must have a candidate. Unsupported or empty candidate sets are incompatible; source-profile admission remains a runtime condition. This metadata-only JSON is not an execution plan and is refused by `convert --plan-file`.

For a single output, use `--save` with a new result destination to check the complete source and save one resolved format:

```sh
drmr plan source/input.m3u8 --encryption cbcs --output-format \
  dash \
  --output-dir converted-dash --save conversion.plan.json --json
drmr convert --plan-file conversion.plan.json --keys-file \
  config/keys.json --json
drmr verify --plan-file conversion.plan.json --keys-file \
  config/keys.json --json
```

Saved plans bind source fingerprints, output paths and names, processing, DASH references and the minimum verification depth. Manifest-profile plans also bind their rule profile. The minimum is native by default; `--decode` can strengthen it during saving or later execution. Existing plans remain readable with their recorded conditions; see [plan versions](https://github.com/kazuhide/drmr/blob/main/docs/releases.md#compatibility-from-12).

The plan records local playlists/media, sizes, SHA-256 content hashes, output format/protection and an absolute destination. Keep it private and outside source/output trees; paths belong to the saved machine and location. Unknown versions/fields, inconsistent IDs, changed source bytes and unsafe destinations fail. The ID checks consistency, not a third-party signature or authorization.

Keys, signed source URLs, authentication headers, license URLs, HLS key URIs and executable arguments are not stored in the plan. Supply keys at execution and verification with `--keys-file`. In a version-1/3/5/7/9/11 single-output plan, DRM signaling selections also remain runtime configuration. Version-2/4/6/8/10/12 plans fix each requested DRM system and manifest-check policy while leaving their secret values and addresses separate. `verify --plan-file` checks source fingerprints, actual output format/protection and independent complete media correspondence. It does not acquire a license or prove device playback.

On Unix, use trusted output and plan parent directories owned by your user that other users cannot modify. Saving a plan refuses an unsafe output parent before media admission. See [workspace permissions](https://github.com/kazuhide/drmr/blob/main/docs/security.md#temporary-files-and-cleanup) for the supported shared temporary-directory rule and cleanup boundaries.

Saving a plan creates no converted media. Encrypted source checks can decrypt with supplied input keys. Source admission is repeated when executing.

## Source directories and DASH references

Downloads keep source-relative media and child-playlist paths, such as `video/1080p/`. The `input.m3u8` or `input.mpd` entrypoint is a localized manifest. `raw/` retains the original manifests and private `raw/index.json` (source URLs, sizes and hashes). Media is stored once; `raw/` adds no media copy. Keep it with the saved source. Keys are not downloaded.

Output media retains source directories; manifests use `master.m3u8` or `manifest.mpd`. Separate media directories have no track prefix: `r000600.ts` becomes `init.mp4` plus `r000600.m4s`, or keeps `.ts` for HLS TS. Physical tracks sharing a media directory use a source-playlist/kind prefix. Supported sequences keep their start and padding; stitch can change their length and boundaries. HLS and DASH `list` retain arbitrary stems when counts match; otherwise they use `seg_1`. DASH templates use `seg_1.m4s` unless names match the supported pattern: one decimal counter increasing by one, with fixed text before and after it; choose `--dash-addressing list` for arbitrary names. MP4 inputs lack segment names and start at `seg_1` in the source directory.

Each rendition needs one media parent. Unsafe/colliding paths and references outside the source root fail; extra origins need fetch-policy permission. Old downloads use existing paths; reacquire into a new directory for remote paths. Plans keep their saved placement. See [source directories](https://github.com/kazuhide/drmr/blob/main/docs/source-layout.md).

Use `--dash-addressing` with direct conversion or a saved plan:

| Value | MPD and resources |
| --- | --- |
| `template` (default) | Numbered media segments with a nominal `SegmentTemplate` duration |
| `template-timeline` | Numbered media segments and an explicit `SegmentTimeline` |
| `list` | `SegmentList` with explicit resource names and timing |
| `base` | One indexed fragmented MP4 file per rendition, referenced with `SegmentBase` |

A nominal duration describes an approximate grid; actual AAC frame boundaries can differ. Use an explicit timeline when those exact boundaries must be declared. Every form retains the required sample, clock and playback-window checks. Encryption and DRM signaling are selected separately. An unrepresentable timing profile fails; the tool does not silently substitute another addressing form. See [DASH references](https://github.com/kazuhide/drmr/blob/main/docs/dash-addressing.md).

```sh
drmr plan source/input.m3u8 --encryption clear --output-format \
  dash --dash-addressing list \
  --output-dir result --save conversion.plan.json
drmr convert --plan-file conversion.plan.json
drmr verify --plan-file conversion.plan.json
```

The plan saves the choice. `convert --plan-file` refuses `--dash-addressing`, including the default value. For several outputs, output-configuration schema 4 permits `dash_addressing` on DASH rows; rows without a value use the CLI default. Per-output `processing` is independent; these recipe versions are separate from key-file versions.

## Separate analysis, saved planning, conversion and verification

Use chapter 7's files if you want to inspect each stage. Direct conversion already includes its checks. `&&` proceeds only after success:

```sh
drmr analyze "https://example.com/vod/master.m3u8" \
  --download-dir source &&
drmr plan source/input.m3u8 --output-config outputs.json \
  --output-dir result --save conversion.plan.json &&
drmr convert --plan-file conversion.plan.json --keys-file \
  keys.json &&
drmr verify --plan-file conversion.plan.json --keys-file \
  keys.json
```

Clear input needs no keys during analysis or planning. For encrypted input, pass the input-key file to both. DASH/MP4 use `source/input.mpd` or `source/input.mp4` instead.

`plan --output-config` requires `--save` and refuses `--encryption`/`--output-format`. The plan fixes original filenames and hashes, the result root, formats, protection, DRM selections, processing, addressing and external-check policies. Keys and license addresses remain separate. Execution rechecks source conditions. Later `verify --plan-file` reads the actual original and result again. Existing outputs are never replaced.

A saved `external_manifest_check: true` makes external checks mandatory during conversion and later verification. Plan saving does not establish that those tools will be available. See [processing routes](https://github.com/kazuhide/drmr/blob/main/docs/processing-routes.md).

# 7. Convert with configuration files

## One output or several

Chapter 1 has a complete single-output example. Chapter 8 covers each input/container/protection choice. Both use `outputs.json`, a `keys.json` only when needed, and one `convert`. Local input uses a file path; URL input also needs `--download-dir`.

## DASH Widevine/PlayReady and HLS with three DRMs

This example produces CBCS DASH and CBCS HLS from qualified clear TS HLS. One encrypted HLS media set carries Widevine, PlayReady and FairPlay declarations; DASH carries Widevine and PlayReady.

Save the complete file as `outputs.json`:

```json
{
  "schema_version": 4,
  "outputs": [
    {
      "id": "dash",
      "drms": [
        "widevine",
        "playready"
      ],
      "output_format": "dash",
      "encryption": "cbcs"
    },
    {
      "id": "hls",
      "drms": [
        "widevine",
        "playready",
        "fairplay"
      ],
      "output_format": "hls-fmp4",
      "encryption": "cbcs"
    }
  ]
}
```

Save the complete file below as an owner-only `keys.json`, following chapter 5. Every value is public test data, including a complete PSSH matching its KID. Replace keys, KIDs, PSSH, license URLs and key URIs with matching values for production. Clear input has no `input` object. Output keys are independent even though this example uses the same dummy values:

```json
{
  "schema_version": 2,
  "outputs": {
    "dash": {
      "kid": "00000000000000000000000000000001",
      "key": "00112233445566778899aabbccddeeff",
      "signaling": {
        "system": "multi-drm",
        "widevine_pssh": "000000327073736800000000edef8ba979d64acea3c827dcd51d21ed00000012121000000000000000000000000000000001",
        "playready_license_url": "https://license.example.invalid/playready"
      }
    },
    "hls": {
      "kid": "00000000000000000000000000000001",
      "key": "00112233445566778899aabbccddeeff",
      "hls_key_uri": "skd://example",
      "signaling": {
        "system": "multi-drm",
        "widevine_pssh": "000000327073736800000000edef8ba979d64acea3c827dcd51d21ed00000012121000000000000000000000000000000001",
        "playready_license_url": "https://license.example.invalid/playready",
        "fairplay": true
      }
    }
  }
}
```

For a retained local source, one command checks the input, converts it, compares the result with the original, lints manifests and checks DRM declarations:

```sh
drmr convert source/input.m3u8 --output-config outputs.json \
  --keys-file keys.json --output-dir result
```

Entrypoints are `result/dash/manifest.mpd` and `result/hls/master.m3u8`. Media directories follow the source layout. HLS and DASH are generated separately; shared physical segments are not promised. All outputs must pass before the result root is published; a subset is not published on failure.

Encrypted input requires its `input.keys` or `input.hls_keys` mapping. See chapter 3 for admission conditions and chapter 8 for complete key files. To compare the result in a later process, use chapter 6's saved plan and `verify --plan-file`.

## Select other outputs

For CENC DASH/HLS, select `encryption: "cenc"` and `drms: ["widevine", "playready"]`. Omit FairPlay and `hls_key_uri` from HLS settings; any PSSH scheme declaration must match cenc. FairPlay is not available with CENC.

For FairPlay TS, use `output_format: "hls-ts"`, `encryption: "hls-sample-aes"`, `drms: ["fairplay"]` and an output key entry with `key`, explicit `iv`, `hls_key_uri` and FairPlay signaling, without an MP4 KID. Chapter 8 has TS output recipes. See the [multiple-output guide](https://github.com/kazuhide/drmr/blob/main/docs/multiple-outputs.md).

The older single-output `system: "playready"` has separate source conditions. Distinguish it from these `system: "multi-drm"` examples. See [DRM signaling](https://github.com/kazuhide/drmr/blob/main/docs/drm-signaling.md).

# 8. Acquire and convert a URL

## Convert directly from a URL

Pass the URL to `convert` with chapter 7's `outputs.json` and `keys.json`. One operation acquires and admits the input, converts it, compares media, lints manifests and checks selected DRM declarations:

```sh
drmr convert "https://example.com/vod/master.m3u8" \
  --download-dir source --output-config outputs.json \
  --keys-file keys.json --output-dir result
```

The retained source directory and result root must be distinct new paths under existing parents. Existing downloads are not automatically reused and existing results are never overwritten. Local input needs no `--download-dir` or `--fetch-config`. For separate analysis and saved planning, use chapter 6.

MP4, static DASH and completed HLS are accepted within their profiles. The response content determines the format; extensions, queries and allowed redirects do not select it.

| URL input | Retained local entrypoint | Acquisition profile |
| --- | --- | --- |
| MP4 | `source/input.mp4` | One complete MP4 within the supported local media profiles |
| DASH | `source/input.mpd` | One static `Period`, finite templates or lists, or a supported single-file index |
| HLS | `source/input.m3u8` | Completed TS/fMP4 playlists within the supported input profiles |

For offline reuse of DASH or MP4, choose the local entrypoint from the table. DASH saves every referenced initialization and media file. Its localized `input.mpd` retains protection declarations, PSSH DRM initialization data and the timeline. HLS localizes media references and relative key identifiers. Original manifests are kept separately and may contain signed URLs. Acquisition leaves encrypted media and initialization bytes unchanged.

The same local codec, track, encryption and timing restrictions still apply. Live input, HLS byte ranges, multiple DASH periods, hierarchical or external DASH indexes, and negative timeline repeats remain outside this acquisition profile. DASH objects used through supported byte ranges are retained in full. These are implementation limits, rather than technical impossibilities of the formats.

Encrypted HLS accepts AES-128 TS/fMP4, SAMPLE-AES TS and CENC/CBCS fMP4 under the local input restrictions. For AES-128 and TS SAMPLE-AES, `input.hls_keys[].uri` must match the absolute identifier resolved after redirects: `keys/key.bin` in `https://example.com/vod/media.m3u8` becomes `https://example.com/vod/keys/key.bin`. Absolute `skd:` URIs stay unchanged. CENC/CBCS uses `input.keys`. No key or license URL is fetched. Encrypted input requires `--keys-file config/keys.json` on full `analyze` and source checks for `plan --save` too; clear input does not. Metadata-only analysis never needs keys.

The acquisition graph, preserved paths, ownership records and overlapping verification inventories share a 128 MiB metadata limit. Deep or long paths can reach it before the resource-count limit. Metadata is admitted before files are created and remains included in the memory budget while the downloaded source is in use. Resource bodies stream to private files; worker buffers and transient acquisition state are budgeted separately.

Direct URL conversion uses the retained original for the remaining processing and comparison; subsequent local operations need no connection to the source server.

## Reuse a retained source

```sh
drmr convert source/input.m3u8 --output-config outputs.json \
  --keys-file keys.json --output-dir retry-result
```

A completed source can be used offline, including after a later conversion failure. DASH uses `source/input.mpd`; MP4 uses `source/input.mp4`. Incomplete downloads are refused.

Source bytes and local references are checked through final publication. Changed or missing source files prevent publication. To retry, use the local entrypoint instead of passing an existing download directory to another URL acquisition.

`convert --plan-file` refuses overrides of source, format, protection and destination. Create a new plan for different conditions or a different destination. Use `verify --plan-file` for a later independent check of a completed result.

## Fetch policy

Default policy is HTTPS with same-origin references and bounded acquisition. `analyze URL`, including `--metadata-only`, and direct `convert URL` accept `--fetch-config`. Metadata-only URL reads need no download directory and retain no original. Explicit origin/private-network/HTTP access requires the supported private policy configuration; it is not enabled by weakening Host or Origin checks.

`--fetch-config FILE` reads an owner-only UTF-8 JSON object saved as, for example, `config/fetch.json`. It contains the policy fields directly, without a `schema_version` field; the input URL stays on the command line. The file is optional, and its fields are optional: omit `headers_by_origin` when authentication is not needed. The example fields mean:

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

Prepare a protected configuration directory as described in chapter 5 and save the JSON as `config/fetch.json`. Replace the token placeholder with your own token if authentication is needed. For macOS/Linux, set owner-only file permissions before analyzing the URL; use the current-user-only ACL procedure in chapter 12 on Windows.

```sh
chmod 600 config/fetch.json
drmr analyze "https://example.com/vod/master.m3u8" \
  --download-dir analyzed-source --fetch-config \
  config/fetch.json --json
```

The same file can be used for direct conversion:

```sh
drmr convert "https://example.com/vod/master.m3u8" \
  --download-dir authenticated-source --fetch-config \
  config/fetch.json \
  --encryption clear --output-format hls-fmp4 --output-dir \
  clear-result
```

Unknown fields, duplicates, present null values and invalid ranges are refused. Headers are scoped to exact origins; redirects and referenced resources must remain allowed. `max_playlist_bytes` bounds each HLS playlist or DASH MPD; MP4 and media resources use `max_resource_bytes`, and `max_total_bytes` applies to the whole download. Admission includes destination relationships, resource counts, bytes and deadlines. A successful plan does not establish these remote conditions.

See [URL conversion](https://github.com/kazuhide/drmr/blob/main/docs/usage.md).

## Example prerequisites and key files {#recipe-setup}

Each recipe links a complete `outputs.json` and the input-specific `keys.json` when needed. Save the output list as `outputs.json` and the matching key file as `keys.json`. Encrypted-input files include both source and output settings; no fragments need to be combined. Chapters 5–7 explain the JSON fields with complete representative examples. Where two commands are shown, run only the one matching your input.

All values are public test data. Replace URLs, source KIDs, keys, key URIs, associated IVs and output keys with matching values. Output configurations use schema 4 with `output_dir: "result"`; key files use schema 2. Save linked settings as `outputs.json` next to the command's working directory. Clear input has no `input`; clear output has no output key. Clear → clear needs no key file. Match output IDs and key-file names.

Use an owner-only work directory and protect key files as described in [chapter 5](#key-configuration). `src` and `result` must be new paths. No `mkdir`, `cd` or plan file is required. Retry with new destinations or the retained local input. For extra origins or authentication, add `--fetch-config config/fetch.json` to `convert`; this chapter has the policy example.

Input must satisfy [chapter 3](#supported-profiles). MP4, DASH and fMP4 HLS examples use one video track; development for 1.3 also accepts the qualified video/AAC pair for DASH/fMP4 HLS-to-MP4 output. MP4-to-TS needs qualified fragmented input. TS-to-MP4 admits one video and one audio at most: narrow a multi-quality master to the chosen video and associated audio. Audio-only TS uses 44.1 or 48 kHz AAC-LC. TS output containing video and audio uses separate playlists.

These recipes request encryption without explicit DRM signaling. For Widevine, PlayReady or FairPlay declarations, use [chapter 7's complete example](#local-conversion). Key/license URIs are not fetched. Commands use POSIX shell syntax; a trailing `\` continues the same command.

{{conversion-examples}}

# 9. What verification proves

For configured outputs, a saved plan is optional for fresh verification:

```sh
drmr verify result --source source/input.m3u8 \
  --output-config outputs.json --keys-file keys.json
```

`result` selects the actual output root, regardless of the JSON's `output_dir`. Omit keys only when both source and outputs are clear.

## Native checks and additional complete decoding

| Result field | Required correspondence |
| --- | --- |
| `stream_metadata` | Codec configuration, dimensions, audio rate/channels, color, rotation and supported language; success is `pass` |
| `packet_payloads_equal` | Every ordered encoded sample or validated access unit, including count and non-framing content |
| `packet_timing_equal` | Every PTS/DTS/duration, exact final end and supported playback windows |
| `decode_evidence` | `not-checked` with reason `not-requested` by default; actual complete source decoding or content comparison with --decode |

Use the actual output `manifest.mpd` or `master.m3u8` for DASH/HLS. Rust reads the original and result independently and decrypts each with its own role keys. All admitted samples are checked, including the last one; a matching first segment or file name is insufficient. Packet identity cannot establish that a playback edit survived.

For TS AVC, native validation parses SPS/PPS/VUI and every slice header, then compares access-unit payloads, PTS/DTS, supported picture clocks and original `EXTINF` durations. Missing VUI timing, field-coded or mixed pictures, MBAFF, unsupported repeated-picture durations, slice groups and unsupported slice ordering are refused. Recognizing an AVC profile, bit depth or chroma configuration does not qualify every combination. Native validation does not fully decode entropy-coded macroblocks; `--decode` obtains that additional complete-decoding evidence without relaxing native checks.

A source with a finite video window and a candidate missing that window fail timing even when all encoded samples match. Input preparation must preserve the original start and duration before comparison. A successful local decode does not establish DRM-license acquisition or playback on a particular device.

The supported encryption modes are unauthenticated. Without an independent known clear reference, native correspondence alone does not establish key correctness or complete decodability. `--decode` adds decoder evidence; a successful decode does not cryptographically authenticate the key or original plaintext.

## Audio frames and segment boundaries

An AAC-LC frame contains 1,024 samples in the supported TS profile. It lasts about 21.333 milliseconds at 48 kHz or 23.220 milliseconds at 44.1 kHz. A two-second target does not require each audio segment to be exactly two seconds, nor force equal audio/video segment counts. Frames stay complete. A TS source may already present padding not present in an earlier MP4 edit; verification preserves the supplied TS rather than shortening it to that older file.

The audio sample rate and the timestamp clock have different roles. MPEG-TS counts 90,000 ticks per second. One frame therefore lasts exactly 1,920 ticks at 48 kHz, but about 2,089.796 ticks at 44.1 kHz. drmr retains the original integer PES timestamps and derives the enclosed frame clocks at the actual sample rate. It validates packet-start rounding and interpolation within the packet separately against one shared fractional clock position for the whole track. Repeatedly adding a truncated 2,089-tick duration would create drift and is refused. Qualified static HLS/DASH with single-track fMP4 AAC-LC at 44.1 kHz and a 90 kHz clock uses the same bounded rounding model; every output timestamp must still match its source.

For 44.1 kHz, native AAC preparation writes fragmented MP4 with the same 90 kHz media clock. Its decoder settings still declare 44,100 samples per second and the original channels. Every AAC payload, frame and original PTS/DTS is preserved; `--decode` additionally checks every decoded sample. Shaka Packager then creates the requested HLS, DASH or TS output. The native writer holds at most 128 frames and at most approximately 1 MiB of encoded audio per fragment; it does not keep the complete asset in memory.

FFprobe may label a TS frame with a nominal duration of 2,089 ticks even when the next DTS is 2,090 ticks later. MP4 sample durations describe that actual distance to the next DTS. The last frame retains the nominal integer duration because there is no following timestamp. Verification requires valid consecutive MP4 durations, every original absolute PTS/DTS, and the exact final end. Allowing source timestamp rounding is not an output tolerance: a changed clock or end, missing sample, or changed encoded byte fails. See [AAC clock handling](https://github.com/kazuhide/drmr/blob/main/docs/media-processing.md#aac-sample-rates-and-transport-clocks) for details.

A 44.1 kHz source playlist's nonfinal `EXTINF` may measure from this segment's first frame to the next segment's first frame. A complete final segment can use its exact audio-sample span or its measured last-frame clock with a nominal 2,089- or 2,090-tick final frame. These duration representations do not permit changes to any original PTS/DTS.

A shorter final audio declaration is a playback boundary, not timestamp rounding. drmr keeps all coded frames and their exact sample spans, and separately records which samples should be presented. For fMP4 HLS and DASH, two standard MP4 edits retain the timeline origin and the exact presented duration. The movie clock represents both individual audio samples and transport ticks; the media clock stays at 90 kHz. The edits are written after packaging. Only this recognized declaration is removed from a private copy for coded-frame inspection or repackaging; the declared range must then match the checked frames and clocks. The input and final output retain their edits. HLS uses the output's own playback bounds; DASH keeps the coded segment timeline and covers every track's presented end. TS uses its final playlist declaration. Fresh verification reconstructs the result's range and compares all coded media and native playback bounds; `--decode` also compares raw and presented PCM. Native MP4 playback rates, gains and enabled-track settings are also checked; matching encoded bytes does not make an altered playback setting acceptable. See [AAC playback boundaries](https://github.com/kazuhide/drmr/blob/main/docs/media-processing.md#playback-ending-inside-the-final-aac-frame).

At 44.1 kHz, a six-second audio target contains about 258.4 AAC frames. Complete segments can therefore contain 258 frames (5.990748299… seconds) or 259 frames (6.013968254… seconds). drmr keeps complete encoded frames and does not round their actual durations to six seconds. `segments` retains the source membership and duration text; `stitch` can choose different frame boundaries.

The integer `EXT-X-TARGETDURATION` bounds each duration after nearest-integer rounding, as specified by RFC 8216. When a new operation inherits the six-second source bound, a 6.013…-second interval still fits. drmr checks the final duration text exactly and retains the common source bound in every generated audio/video media playlist. It recalculates master-playlist bandwidth declarations for that final bound and the actual output bytes. Source bytes and `EXTINF` durations are not changed by this header correction. MP4/DASH sources without an inherited HLS bound and older saved targets keep their generated conservative bounds. Independent verification rejects inconsistent final target-duration bounds. RFC 8216 has separate exceptions for VOD subtitles and I-frame playlists; ordinary audio/video bounds must agree.

## Each file owns its playback window

The finite progressive profile compares exact native edit windows and playback settings. FFmpeg 9.0.2 does not apply finite end discard on every fragmented-MP4 demux path. With `--decode`, drmr therefore checks the complete coded audio decode, then applies each actual file's own declared window and compares complete presented PCM. It does not substitute the reference window for the result's metadata.

Integer start/duration conversion, ordinary movie/track rate and gain, track selection and video display extent are checked. A changed end, missing window or modified playback header fails. This interpretation is qualified by media/mutation tests; it does not prove every player honors the edit.

## Manifest timing

DASH uses one coherent presentation origin at the earliest actual presented sample across tracks, including composition offsets and supported edits. A later clipping origin or earlier artificial gap is refused. Exact packet clocks remain unchanged. Where decimal durations are needed, the canonical upward rounding is to nine decimal places, a one-nanosecond text resolution, not a packet tolerance.

An MP4 edit can shift encoded composition timestamps onto the movie's playback timeline. In fragmented MP4, an edit duration of zero can apply that offset to all following media; it does not mean an empty playback range. drmr applies supported audio and video edits when checking DASH segment references, the final Period and indexes, while independently retaining the original packet clocks. See [the W3C initialization-segment requirements](#references).

Finite HLS segment declarations and end must match the presented window. Initialization movie/track duration headers remain zero; they are not a substitute for the actual edit. Ordinary nonfinite HLS retains its separate declared-decimal rounding rule.

## DRM information and manifest checks

Named-output `verify --plan-file` reads all current result graphs again and compares each with the retained original. The report separates the following outcomes:

| Outcome | Meaning |
| --- | --- |
| `verification` | Complete native sample/configuration/timing correspondence and explicit optional decode evidence |
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

For `external_manifest_check: true`, HLS uses Apple's `mediastreamvalidator` in playlist-checking mode. DASH uses the local DASH-IF Conformance Software XML schema and Schematron checks, which validate MPD structure and rules. These checks read the final encrypted output manifests, including reachable HLS child playlists. They are distinct from full media verification and do not prove encrypted-media conformance, license acquisition or device playback. Missing tools, interrupted checks, incomplete reports and validation errors cause failure. The setting `false` leaves the external check unrequested and is reported separately from a pass.

Prepare the Apple HLS tools in a macOS environment. The DASH route requires the supported local DASH-IF software and dependencies; set `DRMR_DASH_IF_ROOT` to that checkout. `--hls-validator-bin` selects the Apple executable and defaults to `mediastreamvalidator`. `--php-bin` selects the PHP interpreter for the local DASH-IF adapter and defaults to `php`; it is not a path to an arbitrary DASH-checking command. These runtime paths are separate from the saved plan. See [manifest validation setup and scope](https://github.com/kazuhide/drmr/blob/main/docs/manifest-validation.md).

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

These commands run the native Rust linter and need no external validator. Add `--external-manifest-check` to run Apple or DASH-IF checks as well. The JSON result separates `native_manifest` and `external_manifest`. `--max-workers` sets an upper bound; omitting it selects concurrency automatically. Apple's adapter uses parse-only mode; its rule coverage differs from the native Apple authoring profile.

The default `--manifest-profile base` checks RFC 8216 manifest rules for HLS and the static-presentation profile for DASH. With `--manifest-only`, select `--manifest-profile apple-vod` to add the supported Apple VOD authoring rules to HLS; DASH rejects this selection. The report names the selected profile. A single video bitrate may pass base HLS rules and fail Apple's section 9.9 requirement for multiple video bitrates. Conversion preserves existing video qualities and does not generate additional ones. These profiles do not certify media, delivery or device behavior outside their declared manifest scope.

The report has `scope: "manifest-only"` and `media_verified: false`. It reads no content keys and performs no source-to-result media comparison. `--manifest-only` cannot be combined with `--plan-file`, `--source` or `--keys-file`. To establish complete media correspondence and the saved conditions, use ordinary `verify --plan-file` with the appropriate key file.

## Processing units and storage

Select `--processing` on direct `convert` or `plan --save`. `auto` selects a qualified packaging route. `segments` processes supported matching containers at their original segment boundaries. `stitch` joins when needed, then writes MP4 or segments HLS/DASH again at the resolved request. Explicit unsupported modes are refused. The saved choice cannot be overridden by `convert --plan-file`. Same-container `segments` accepts the qualified two-edit AAC range after checking every frame and clock. Unsupported edits and container changes are refused.

An HLS target-duration declaration is an upper bound, which can differ from the interval requested from Shaka Packager. New plans derive that request from admitted video intervals. For example, a common three-second bound can describe two-second video and approximately 2.005-second audio segments: the plan saves a two-second request and final HLS keeps the three-second bound after checking every output segment. MP4 and DASH inputs use a two-second default when stitching to segmented output. Earlier plan schemas 1–6 retain their fixed requests, including an implicit two seconds; execution does not shorten a saved request.

Shaka's chunking uses the media clock, so audio starting above zero can otherwise produce a short first segment. For `stitch`, drmr temporarily moves the prepared audio's coded start to zero, packages it, then restores the original timestamps before complete comparison. The original files and final audio/video timing remain unchanged. `segments` keeps original segment membership instead of applying this resegmentation step.

Native `segments` checks sample membership and clocks without a whole-track intermediate. `--decode` can still prepare complete tracks and uses Shaka Packager for TS preparation. Complete source-to-result checks precede publication. Metadata and temporary media remain bounded; this mode promises neither zero temporary storage nor a measured speedup.

Multi-output conversion retains byte-bound private copies of completed outputs until the whole result is ready for publication. Independent plan verification also retains all result copies until its final consistency checks. Together with separate packaging for every requested output, this increases temporary disk usage with media size and output count. The copies are streamed to disk rather than retaining complete media in memory. Keep enough free working space and allow the owned cleanup to finish.

See [processing and timing](https://github.com/kazuhide/drmr/blob/main/docs/media-processing.md).

# 10. Workers, memory and Stop

## Worker limits

Full `analyze`, `plan --save`, `convert` and `verify` select workers from available CPU, memory, media dimensions, resource sizes and track count. `--max-workers` is an upper bound from 1 to 64; the actual worker count can be lower. Automatic recommendations cap at 32. Metadata-only analysis refuses this option.

```sh
drmr convert source.mpd --encryption cbcs --output-format dash \
  --output-dir result --keys-file config/keys.json \
  --max-workers 2
drmr verify result/manifest.mpd --source source.mpd \
  --keys-file config/keys.json --max-workers 1 --json --stats
```

Worker count never reduces proof coverage. Track, segment and source-probe pools share reservations within one process. Separate CLI processes do not share a reservation pool.

Full `analyze` continues after downloading: it validates a private source copy, prepares tracks and checks every selected encoded sample, configuration, clock and supported playback window in Rust. `--decode` adds complete decoded-media checks. Rust jobs and independent outputs can run concurrently; each Shaka process runs its media jobs serially. Network and disk throughput, process startup, track count and ordered checks affect the total time. Compare one worker with automatic selection on the same retained local source to separate processing from download time. `analyze --metadata-only` is a lighter metadata or declaration read and does not establish conversion eligibility.

Within TS preparation, independent segments supply metadata and encoded-frame evidence in parallel. Only a limited number of compact results are retained at once. Duration and clock boundaries are then checked in playlist order, with each media playlist prepared in turn. All source checks remain required; `--decode` adds complete-track decoding.

Native manifest checks share this capacity. Small inputs run inline; larger HLS graphs can distribute document work, and a large DASH manifest can share one parsed tree across independent checks. Pools do not nest. Parsed child facts and inherited timelines are reused. The adapter reserves memory for retained manifest bytes and lint work, then verifies file identities and content. Exhausted byte, node, reference or diagnostic limits produce an incomplete report and cannot pass.

## Admission is not OS enforcement

Readable Linux CPU/memory cgroup limits, including parent limits, inform admission. If unavailable, drmr assumes a 512 MiB budget and one child process. An operation that cannot fit fails with `resource-budget`.

When memory is known, a quarter is reserved with a 256 MiB minimum, and per-track metadata is estimated. Child reservations include a base, frame counts, workload and capture bounds. These estimates are not an OS memory limit or a guarantee of exact peak usage.

When additional decoding runs, FFmpeg decoder/encoder/filter and ffprobe decoding thread settings use one thread; auxiliary threads may still exist. Packet/audio evidence is streamed, while retained metadata still grows. Default captured stdout is 16 MiB and stderr 1 MiB. Over-limit output fails rather than verifying truncated evidence.

## Cancellation and cleanup

Generic convert/verify use SIGINT/SIGTERM on macOS/Linux and Ctrl+C/Ctrl+Break on Windows. Stop prevents new work, terminates owned processes and confirms cleanup. A Stop observed before publication leaves the destination unpublished.

Exit 130 means cooperative cancellation completed including process confirmation. `cleanup-unconfirmed` and `media-cleanup-unconfirmed` are failures, not successful Stop. Force-kill is a different operation and may leave private work behind.

Unix process groups and Windows job objects retain descendant ownership. Sending a signal alone does not prove locks are released. Unix final group checks and Windows job notifications have their own 30-second budgets; total wall-clock shutdown also includes I/O, name lookup and scheduling.

## Measurements

Historical complete-decoding measurements describe the additional `--decode` workload. They do not measure the current native default; compare only equal verification depths.

Use `--stats` for detailed observations in readable output or JSON. It does not change verification. Nested/parallel phases overlap; their times cannot simply be summed into wall time. Short clips are dominated by startup and cache effects. Sampled memory is not an exact peak. Compare the same original, tools, build settings, worker bounds and host load only after verification passes.

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

[Analysis and conversion measurements](https://github.com/kazuhide/drmr/blob/main/docs/performance.md) include before-and-after timings for changes that reduce repeated copying. These comparisons keep the original and verification depth fixed and record media-tool calls and the load from other host work.

# 11. Docker with media tools

## Build an image from committed source

The image includes drmr, FFmpeg/ffprobe and Shaka Packager. Build requires Git and BuildKit-capable Docker. Native Linux amd64/arm64 are supported build targets; select Linux containers in Docker Desktop.

```sh
revision=$(git rev-parse HEAD)
tree=$(git rev-parse "${revision}^{tree}")
git archive --output-format=tar "$revision" | docker build \
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
  --encryption cbcs --output-format dash --output-dir \
  /output/protected-dash \
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
  --source /input/manifest.mpd --keys-file /config/keys.json \
  --json
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
  --mount "type=bind,src=$PWD/media,dst=/media,readonly" \
  --mount "type=bind,src=$PWD/output,dst=/output" \
  --env DRMR_INPUT_ROOT=/media --env DRMR_OUTPUT_ROOT=/output \
  drmr:local serve
```

Docker Desktop requires its additional host-networking feature. Ordinary port publication cannot reach a service listening only on container loopback. Do not disable loopback/Host/Origin safeguards to work around this. The image does not contain a native Desktop GUI.

On Windows, use Docker Desktop WSL2 integration and a Linux shell. Keep private configuration on the Linux filesystem; Windows-drive mounts have different permission semantics from native Windows ACL workflows.

`/usr/share/drmr/build-manifest.json` records commit/tree and tool provenance. Notices are under `/usr/share/drmr/licenses`; corresponding selected media-tool sources are under `/usr/share/drmr/sources`, not the complete Debian source tree.

See [Docker usage and scope](https://github.com/kazuhide/drmr/blob/main/docs/docker.md).

# 12. Native Windows CLI

## Environment

Use a normal Windows x64 user on local NTFS. Elevated runs creating Administrators-owned media, network shares, junctions in private paths, alternate data streams and unsafe existing permissions are refused.

Extract the CLI archive and add it to your user PATH or invoke the absolute executable. Media tools remain separate. Builds use VCRUNTIME140.dll; install the Microsoft Visual C++ x64 Redistributable when required.

```powershell
cargo build --release --locked -p drmr
.\target\release\drmr.exe --help
.\target\release\drmr.exe doctor --probe --route all
```

## Owner-only key files

Windows uses an access-control list (ACL), not chmod. The protected ACL must disable inheritance, retain the current user as owner and grant that user full control. Run the following setup in a regular Windows PowerShell 5.1 terminal (`powershell.exe`). It creates a new private directory and empty key file before editing secret content:

```powershell
$ErrorActionPreference = 'Stop'
$sid = [System.Security.Principal.WindowsIdentity]::GetCurrent().User
$private = Join-Path $env:LOCALAPPDATA 'drmr-keys'
if (Test-Path -LiteralPath $private) { throw 'Choose a new directory' }
New-Item -ItemType Directory -Path $private | Out-Null

function Set-OwnerOnlyAcl($path, $security) {
    $directory = $security -is
        [System.Security.AccessControl.DirectorySecurity]
    $sections =
        [System.Security.AccessControl.AccessControlSections]'Owner, Access'
    $current = if ($directory) {
        [System.IO.Directory]::GetAccessControl($path, $sections)
    } else {
        [System.IO.File]::GetAccessControl($path, $sections)
    }
    if (-not $current.GetOwner(
        [System.Security.Principal.SecurityIdentifier]).Equals($sid)) {
        throw 'The current user must own the path'
    }
    $security.SetAccessRuleProtection($true, $false)
    $rule = [System.Security.AccessControl.FileSystemAccessRule]::new(
        $sid, 'FullControl', 'Allow')
    $security.AddAccessRule($rule)
    $checked = if ($directory) {
        [System.IO.Directory]::SetAccessControl($path, $security)
        [System.IO.Directory]::GetAccessControl($path, $sections)
    } else {
        [System.IO.File]::SetAccessControl($path, $security)
        [System.IO.File]::GetAccessControl($path, $sections)
    }
    $rules = @($checked.GetAccessRules($true, $true,
        [System.Security.Principal.SecurityIdentifier]))
    if (
        -not $checked.AreAccessRulesProtected -or
        $rules.Count -ne 1 -or
        -not $checked.GetOwner(
            [System.Security.Principal.SecurityIdentifier]).Equals($sid)
    ) {
        throw 'Private ACL verification failed'
    }
    $actual = $rules[0]
    if (
        -not $actual.IdentityReference.Equals($sid) -or
        $actual.IsInherited -or
        $actual.AccessControlType -ne
            [System.Security.AccessControl.AccessControlType]::Allow -or
        $actual.FileSystemRights -ne
            [System.Security.AccessControl.FileSystemRights]::FullControl -or
        $actual.InheritanceFlags -ne
            [System.Security.AccessControl.InheritanceFlags]::None -or
        $actual.PropagationFlags -ne
            [System.Security.AccessControl.PropagationFlags]::None
    ) {
        throw 'Private ACL verification failed'
    }
}

```

Apply the ACL to the directory and file, then enter keys:

```powershell
Set-OwnerOnlyAcl $private `
  ([Security.AccessControl.DirectorySecurity]::new())
$keyFile = Join-Path $private 'keys.json'
New-Item -ItemType File -Path $keyFile | Out-Null
Set-OwnerOnlyAcl $keyFile `
  ([Security.AccessControl.FileSecurity]::new())
notepad $keyFile
```

The function verifies that you already own each path and changes only its access rules, then checks the resulting owner and ACL. Assigning an owner requires a separate permission and is unnecessary for these new paths. If an editor replaces the file and changes its ACL, reapply the owner-only ACL while keeping it inside the private directory. drmr does not silently fix an unsafe existing key file.

## PowerShell conversion

```powershell
.\drmr.exe analyze --metadata-only 'C:\media\source.mp4' --json
.\drmr.exe plan 'C:\media\source.mp4' --encryption cbcs `
  --output-format mp4
$outDir = Join-Path $env:LOCALAPPDATA 'drmr-output'
New-Item -ItemType Directory -Path $outDir -Force | Out-Null
$result = Join-Path $outDir 'protected.mp4'
.\drmr.exe convert 'C:\media\source.mp4' --encryption cbcs `
  --output-format mp4 `
  --output-dir $result --keys-file $keyFile
.\drmr.exe verify $result --source 'C:\media\source.mp4' `
  --keys-file $keyFile --json
```

Create `drmr-output` first and choose a nonexistent `protected.mp4`. Manifest URIs and object keys continue to use `/`, unlike native Windows paths. Ctrl+C/Ctrl+Break return 130 only after descendant and file-lock confirmation. Arbitrary filesystem/tool configurations, including long paths, are not guaranteed merely because native Windows CI passes.

Keep output and temporary paths shallow: a media tool can create a longer temporary manifest filename even after its segments were written successfully. For HLS and DASH children of a multi-output request, drmr places media-output stages beside the private bundle stage under the original output parent, avoiding extra nesting. Verified HLS/DASH children move into the bundle on the same filesystem; MP4 retains its separate file-copy publication stage. Only the complete checked bundle is published. This preserves atomic publication but does not remove external tools' path-length limits.

See [Windows conditions](https://github.com/kazuhide/drmr/blob/main/docs/windows.md).

# 13. Local Web console

## Start with input and output folders

The console is an unauthenticated local operator interface. Use trusted loopback only; do not expose it with a reverse proxy or port forwarding. Host/Origin validation does not authenticate another local process.

```sh
DRMR_INPUT_ROOT=/absolute/path/media \
DRMR_OUTPUT_ROOT=/absolute/path/output \
drmr serve
```

Open `http://127.0.0.1:8080`. Both roots are required. `DRMR_BIND_PORT` changes the port; `DRMR_MAX_WORKERS` caps workers for saved-plan operations. Automatic allocation stays within that cap. Select media and saved plans from the allowed input folder.

Use the same-version server and CLI. `--drmr-bin` selects the actual executable; a wrapper launching a different CLI can be refused by the supervision contract.

For keys or fetch settings, set `DRMR_COMMON_CONFIG_ROOT` before startup to a private configuration folder separate from input, output and custom static folders.

The private configuration picker replaces file and folder names with numbered labels. Each page follows filename order; check the corresponding local folder before choosing a file. Configuration contents and original names are not shown in the console.

## Convert with a saved plan

**New conversion** runs the same saved-plan operations as the CLI, one action at a time:

1. Choose **Local media**, **Source URL** or **Saved CLI plan**, then **Open input**. URL input accepts the [supported MP4, static DASH and completed HLS profiles](#url-input). Files come from folders allowed by the server or Desktop; the page does not upload files or accept arbitrary host paths.
2. For new media, select a **Key file** when needed, set **Workers**, then choose **Analyze**. A URL can use a **Fetch policy file**. Keep keys and fetch policies in the private configuration folder; do not enter their contents into the page.
3. Add 1–8 named outputs. Choose format, encryption, DRM signaling and external manifest checks, then select a parent folder and a new output folder name. **Save plan** validates the input profile and supported combinations.
4. Use **Download original plan** to retain a private copy, then **Convert**. Existing destinations are refused. Editing output settings requires saving a new plan.
5. Choose **Verify again** to compare the saved plan with existing output without reconverting it. Later, open the same file as a **Saved CLI plan** to verify without repeating analysis or conversion.

Console plans use a named-output schema, even with one output. Select processing when saving a plan; DASH outputs also have **DASH segment references**, with the same four forms as the CLI. The additional-decoding checkbox has the same meaning as CLI `--decode`; the saved minimum still applies if the checkbox is clear. Result badges distinguish native checks, actual decoding and unrequested decoding. Analysis, new plans and imported plans all use schema-2 keys. Imported single-output plans select `outputs.default`; named plans select their output IDs. Chapter 17 documents the file formats.

Plan downloads preserve the original bytes and absolute paths. Keep them private because they identify local files and output locations. Do not rewrite paths to move a plan between environments. The page sends references rather than raw keys, configuration contents or executable paths, and never retrieves key-file contents.

URL sessions use a generic **Source URL** label. The server does not return the submitted URL to the page; credentials can occur in the path as well as in query parameters. The input field is cleared when submitted.

A session admits at most 32 operations. At the limit, download the plan, choose **Close this conversion**, then reopen the saved plan to verify existing output. If a launch outcome is unknown, **Check status** looks up that same operation without resubmitting it. A running or cleanup-unconfirmed session cannot close. Changing the selection or language does not release its execution slot.

## Job state and cleanup

A lost launch response remains unknown until the same request and job can be looked up. **Check status** only checks that operation; it does not submit another conversion. Selection and language changes do not release a running operation.

Stop requests termination of the owned job and its media tools. An acknowledgment alone does not establish cleanup. Unconfirmed cleanup retains the execution slot and prevents new work. Check status and logs before retrying. During shutdown the server closes submissions and waits for owned work to stop. Fix an incomplete shutdown and retry; force-exit bypasses this confirmation.

## Saved files and private configuration

Retained originals, saved plans, results and logs remain on disk. Closing the page does not erase them or prove that active work has stopped. Keep these files private. URL input is cleared after submission; key and fetch-policy file contents are not sent to the browser.

See [console operation](https://github.com/kazuhide/drmr/blob/main/docs/usage.md) and [local interface security](https://github.com/kazuhide/drmr/blob/main/docs/security.md).

# 14. Desktop host

## Open the same local console

The experimental native host wraps the local Web console. Saved plans, jobs, media tools and restrictions are shared.

1. Choose **File > Choose input folder…** for allowed media and saved plans.
2. Choose **File > Choose output location…**. A private `drmr-output` child is created or reopened there.
3. Optionally choose **File > Choose private configuration folder…** for keys and fetch policies, separate from input/output. Clear private configuration selection removes this selection before console startup.
4. Choose File > Check external tools in the actual application environment.
5. Choose File > Open console.

Use **New conversion** for the saved-plan steps in chapter 13. Without a configuration folder, clear flows without keys or fetch settings remain available. Key-file contents are not uploaded through the browser.

**Download original plan** opens a native save dialog for a new private JSON file. Choose a folder outside the input folder and `drmr-output`: the unchanged plan records absolute local paths. The private folder must not contain either media folder. The default is the private configuration folder, or an owner-only `drmr-plans` sibling of `drmr-output`. Unsafe existing folders and existing files are refused without changing them. To reopen the copy later, select its folder as **Private configuration folder** before console startup, then choose **Saved CLI plan**. Select `drmr-plans` in the later session if the copy was saved there; the private folder must remain separate from input/output media. CLI verification with `drmr verify --plan-file /path/to/private/drmr-plan.json` is also available, with runtime keys if needed. Other attachment downloads are unavailable in the Desktop window.

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

## Runtime qualification

Native package builds check the host and CLI pairing; they do not establish dialog, WebView or DRM playback behavior. Each operating system and native WebView needs its own runtime qualification. Linux GUI dependencies have documented maintenance limitations.

See [Desktop scope](https://github.com/kazuhide/drmr/blob/main/docs/desktop.md) and [dependency policy](https://github.com/kazuhide/drmr/blob/main/docs/dependency-security.md).

# 15. Results, exit values and troubleshooting

To retain readable output and save the same run's JSON, use `--report-file`; `--stats` adds detailed measurements:

```sh
drmr convert original.mp4 --encryption clear \
  --output-format mp4 --output-file result.mp4 \
  --stats --report-file result.json
```

The report must be a new file outside source/result trees and distinct from plan/configuration files. `--json` still controls stdout separately. Saving a report does not rerun media work.

## JSON results

Use `--json` for machine-readable output. Complete-media report schema 2 separates required depth, source admission versus comparison, three native checks and actual decode evidence. For example, a native comparison can report:

```json
{
  "schema_version": 2,
  "required_depth": "native",
  "scope": "comparison",
  "passed": true,
  "checks": {
    "stream_metadata": "pass",
    "packet_payloads_equal": true,
    "packet_timing_equal": true
  },
  "decode_evidence": {
    "status": "not-checked",
    "reason": "not-requested"
  },
  "license_playback_verified": false
}
```

`passed` means the required coverage was satisfied. `not-checked` is not decoded equality. With `--decode`, source admission records `scope: "source-admission"` and `status: "source-decoded"`; source-to-result decoding records `status: "content-compared"` with its actual `equal` boolean. New input-analysis version 2 nests this report under `verification`. New reports omit the old `media_verified` and `decoded_content_equal` fields. Legacy version-1 reports retain their original complete-decoding meaning; metadata-only and manifest-only schemas remain separate.

Operational failures before a media comparison completes retain the schema-version-1 command/error envelope. A completed mismatch retains its version-2 comparison evidence and adds a safe command/status/error description. A timing mismatch can report:

```json
{
  "schema_version": 2,
  "required_depth": "native",
  "scope": "comparison",
  "passed": false,
  "checks": {
    "stream_metadata": "pass",
    "packet_payloads_equal": true,
    "packet_timing_equal": false
  },
  "decode_evidence": {
    "status": "not-checked",
    "reason": "not-requested"
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

## Progress on standard error

`--progress` uses stderr; stdout carries final results for pipes.

| Mode | Behavior |
| --- | --- |
| `auto` | Default: text; with --json, text only if stderr is a terminal, otherwise off |
| `text` | Dashboard where supported; plain logs otherwise, also with --json |
| `json` | JSON Lines: one object per line, also with --json |
| `off` | No progress; results/errors unchanged |

With `--json`, `>result.json` keeps terminal progress; `2>errors.log` suppresses it. Explicit `text`, `json` or `off` overrides auto.

```sh
drmr analyze original.mp4 --metadata-only --json --progress text
drmr analyze original.mp4 --metadata-only --json --progress \
  json \
  >metadata.json 2>progress.jsonl
drmr analyze original.mp4 --metadata-only --progress off
```

Supported terminals update only progress rows below the command; prior logs remain. Resizing keeps the old view and starts new rows; live rows clear before results. Command/time, `Source` acquisition/checks, `Overall` phase/ETA and outputs use fixed label/bar/detail columns and per-row task counts; `Checked` stays until publication. Updates, including phase/output changes, are at most once a second; final success/failure/cancellation is immediate. Bars need 60 columns; details need space. No internal notices/tools; fallback terminals/redirection log milestones/time. Final checks overlap publication; completion follows commit.

Labels identify source copies/checks, clear preparation, output comparison and independent checks. Batch-local Rust task counts show `active` (started/unfinished, including CPU/memory waits), `waiting` (not started), completed/total and `stopped` (unsuccessful exit, not corrupt media). They are not CPU usage, Shaka jobs or track percentages.

Clear HLS TS to multiple `stitch` fMP4 HLS/DASH outputs, all clear/cenc/cbcs without decoding/external checks, estimates logical work excluding download. Preparation counts, generated bytes and comparisons contribute, not playback/check pass rates. `100%` follows publication.

ETA uses this run's stage rates, longest parallel path and later checks, including file integrity; no past/asset timings. `ETA ~4s` means about four seconds left. Unknown future rates give `ETA estimating` (overall JSON `null`); output `step ~4s` covers its current step and may appear first. Estimates vary by stage/load; other routes, `--decode` and required external checks have no overall ETA.

Known download totals enable counts/percentages. TS preparation counts the whole current playlist. Step ETA needs one second and two completions after totals are known. Later boundary checks follow playlist order without percentage/ETA.

Unknown totals show task/time. MP4 URL transfers show bytes, then `1/1`, without percentage/ETA. Totals count completed discovery and distinct media; bytes include retries and may differ from saved size. Workers limit independent jobs, not every stage.

Progress JSON keeps all task states and activity/resource/scope fields; `pipeline` adds phases, outputs, counts and estimates. Overlapping or failed scopes do not imply success. Help/version have no progress; `--stats` timings may overlap.

`convert` compares media and checks applicable native manifests and requested DRM declarations; external checks run only when requested. Successful human output adds one `Summary` of profiles, preserved source, output locations, processing, counts and elapsed time, including saved plans and `--progress off`. [Save the summary or JSON measurements](https://github.com/kazuhide/drmr/blob/main/docs/usage.md#completion-summary). `verify` independently rereads saved files.

Progress excludes URLs, keys, headers, paths and raw metadata/tool output. `--progress json` writes stderr JSON Lines with fixed failure/cancellation codes. Parsing errors and `serve` startup/address notices may be text. Final results/exit values determine success.

## Exit values

| Value | Meaning |
| --- | --- |
| 0 | The selected command succeeded; what it checked depends on the command |
| 1 | Processing, comparison, required diagnostics or cleanup confirmation failed |
| 2 | Invalid arguments/configuration |
| 130 | A supported CLI confirmed cooperative cancellation and owned-process cleanup |

`doctor` success is selected diagnostic success; `analyze --metadata-only` success is metadata reading, not complete media verification.

## Common failures

| Symptom/code | Action |
| --- | --- |
| Tool unavailable | Check the actual launch PATH, executable permission and Desktop diagnostics |
| `remote-metadata-not-supported` | Check the declaration profile; remote MP4 metadata requires a server that honors HTTP Range requests |
| Unsupported version/features | Inspect build options as well as version; confirm the required advertised options |
| Key file refused | Check owner, mode 600/ACL, link count, duplicate/unknown fields and schema |
| `output-spec-invalid` | Use output-configuration schema 4 with required fields, unique safe IDs and supported format/encryption/DRM combinations |
| `execution-plan-output-keys-mismatch` / `execution-plan-drm-mismatch` | Match the version-2 key file's encrypted output IDs and DRM selections to the plan |
| `manifest-validator-unavailable` / `manifest-validator-not-configured` | Install the selected Apple/DASH-IF dependencies and check executable paths; a required check cannot be skipped |
| `input-key-required` | Supply every source KID or exact HLS key-URI mapping |
| `output-key-required` / `output-key-not-used` | Supply the required output key; remove unnecessary output keys for clear output |
| `presentation-window-not-supported` | Check edit structure, start/end, normal rate and exact clock conversion; do not trim the original |
| `progressive-cenc-profile-not-supported` / `progressive-cbcs-profile-not-supported` | Check sample encryption, IV/auxiliary layout and input initialization headers |
| `unsupported-packet-timing` | Check complete PTS/DTS/duration evidence and required tools; do not invent missing clocks |
| `vp9-protection-scheme-not-supported` | Use clear/CENC VP9 samples; the VP9 MP4 binding does not define CBCS. Outer HLS AES-128 is separate. |
| `round-trip-verification-failed` | Inspect native metadata, encoded payloads, timing and any required decode evidence individually |
| `resource-budget` | Check observed CPU/memory, media size/dimensions; more workers may not help |
| `media-output-limit` / `media-probe-output-limit` | Inspect capture/record bounds; truncated evidence cannot prove success |
| `cleanup-unconfirmed` / `media-cleanup-unconfirmed` | Keep ownership and recover; do not treat locks/processes as released |

Choose a fresh destination for another conversion. A complete retained URL input can be reused locally, but an existing download directory cannot be acquired again. Confirm tools have stopped before deleting private work.

Share fixed error codes, tool versions, supported profile and a minimal sanitized reproduction. Redacted logs do not mean every URL, filename or signaling field is confidential-data-free.

# 16. Regression tests and evidence limits

The recorded complete-decoding results in this chapter retain that coverage. Their suites now request `--decode` explicitly; they do not qualify the native default of a later build.

## Short public media

Real-media regressions use the attributed ten-second Big Buck Bunny excerpt with 240 H.264 video packets and AAC audio. Video segmentation targets five two-second segments; audio follows complete frame boundaries rather than forcing every segment to exactly two seconds.

| Regression | Scope |
| --- | --- |
| 16 protection routes | Prepared clear/cenc/cbcs DASH and AES-128 fMP4 HLS, separate original/result keys, conversion and fresh verification |
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

The six MP4/DASH URL cases passed with ten-second fixtures: MP4 has one video track; DASH has separate video and audio tracks with two-second segment targets. Each case analyzes its URL, stops the source server, saves a plan from the retained local entrypoint, runs `convert --plan-file`, then independently runs `verify --plan-file`. Clear input produces CBCS DASH; cenc/cbcs input produces clear DASH. Packet content, timing, complete decoded content, source identity and output intent all match. Original media and raw manifests remain unchanged, and stdout, stderr and saved plans exclude the supplied credentials and keys. Run this regression with `cargo test --locked -p drmr --test url_formats_media -- --nocapture`. It does not establish DRM playback or execution of all 65 placeholder recipes.

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

The 54 finite combinations consist of audio27 plus joint27; CLI27 repeats the latter. The 16 protection routes use a fragmented reference, whereas finite tests use the original progressive file's own playback range. Do not mix sample counts across different references/decoding paths.

Docker HTTP tests do not qualify a browser UI; native bundle construction does not qualify playback. Known-key media checks do not establish license acquisition or device operation.

## Recorded three-output manifest checks

A local run on 2026-10-21 (Japan time), using source [3f8a9a89](https://github.com/kazuhide/drmr/commit/3f8a9a89d0fc84e9699939a193baf823cafb6127), completed the chapter 6 workflow with the ten-second clear HLS/TS fixture and two-second segment targets. The CLI ran on macOS ARM64. Apple mediastreamvalidator 1.25.36 ran natively in playlist-only mode. DASH used the official DASH-IF v3.1.0 XML schema and Schematron rules at revision `bdf0c27a8f198da7a4bf81634bb01faf7c7c3c06`, in a Linux ARM64 container with network access disabled.

| Final output | Protection and DRM declaration | Observed manifest result |
| --- | --- | --- |
| DASH fMP4 | CBCS; Widevine and PlayReady | Native Rust manifest checks, MPD XML schema and Schematron checks passed |
| HLS fMP4 | CBCS; FairPlay | Native Rust checks passed; Apple checked all three playlists; media playlists declared version 6, meeting minimum 6 |
| HLS TS | SAMPLE-AES; FairPlay | Native Rust checks passed; Apple checked all three playlists; media playlists declared version 6, meeting minimum 5 |

URL acquisition retained the original, and the local source server was stopped before saving the plan. Conversion and independent `verify --plan-file` both passed the complete metadata, encoded-packet, timing, decoded-content and current DRM checks. Separate manifest-only checks also passed for copies containing only MPD or M3U8 files, without media, initialization files or keys.

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
cargo test --locked -p drmr-core --test conversion_ts_output \
  -- --nocapture
cargo test --locked -p drmr-core --test conversion_ts_output \
  audio_hls -- --nocapture
cargo test --locked -p drmr --test conversion_ts_dash -- \
  --nocapture
cargo test --locked -p drmr-core --test \
  conversion_finite_outputs \
  -- --nocapture
cargo test --locked -p drmr --test conversion_finite_outputs \
  -- --nocapture
cargo test --locked -p drmr-core --test conversion_multi_drm \
  --test conversion_large_signaling -- --nocapture
cargo test --locked -p drmr-core --test \
  conversion_bundle_encryption \
  --test conversion_bundle_publication -- --nocapture
```

`DRMR_CONVERSION_REPORT` saves the 16-route `bbb_media` matrix's JSON to a new path outside the repository. It does not configure every TS/finite suite's report. Use their test logs and CI artifacts. Reports include actual fixture hashes, four comparisons, tool versions, elapsed/sampled-memory observations and measurement limits.

A separate long-clock regression repeats a tiny derived 32x18 source 417 times for 4,170 seconds and 100,080 packets:

```sh
cargo test --locked -p drmr --test long_conversion
```

This checks complete long packet evidence, not a benchmark for high-resolution throughput or an exact memory maximum. Neither this nor the short fixture establishes universal input or DRM-device compatibility.

See [fixture attribution](https://github.com/kazuhide/drmr/blob/main/crates/drmr/tests/fixtures/bbb/README.md), [encrypted fixtures](https://github.com/kazuhide/drmr/blob/main/crates/drmr/tests/fixtures/bbb/CENC.md) and [processing](https://github.com/kazuhide/drmr/blob/main/docs/media-processing.md).

# 17. CLI reference

Use `drmr help COMMAND` or `COMMAND --help` for command help. `drmr --version` and `drmr version` print the version. Version 1.2 replaces the option names listed in the [migration guide](https://github.com/kazuhide/drmr/blob/main/docs/releases.md#version-12-migration); old names are refused.

## Global progress option

| Option | Meaning/default |
| --- | --- |
| `--progress MODE` | `auto`, `text`, `json` or `off`; default `auto`. Progress uses stderr; results use stdout. Chapter 15 describes JSON Lines and terminal behavior |

The media commands accept `--json` for one versioned result, `--stats` for detailed numeric measurements, and `--report-file NEW_FILE` to save JSON while retaining readable stdout. Statistics do not change verification. Metadata-only analysis refuses statistics; planning statistics require `--save`.

## File and path values

`FILE` names a local file, not inline JSON. `NEW_FILE` and `NEW_DIR` must be new destinations under existing parents. CLI relative paths use the working directory; an output configuration's `output_dir` uses that JSON file's directory. An explicit direct `--output-dir` overrides it; a saved plan refuses destination overrides.

| Example | Content and reference |
| --- | --- |
| `--output-config config/outputs.json` | Schema-4 output conditions and optional `output_dir`; [chapter 6](#inspect-plan) |
| `--keys-file config/keys.json` | Private schema-2 input keys and named output keys; quick single-output commands use `outputs.default`; [chapter 5](#key-configuration) |
| `--fetch-config config/fetch.json` | Private origin/header/resource-limit JSON; [chapter 8](#url-input) |
| `--plan-file conversion.plan.json` | Generated by `plan --save`; not an output configuration or key file; [saved plans](#inspect-plan) |
| `--packager-bin /opt/media-tools/packager` | Installed Shaka Packager executable; [installation](#installation) |
| `--hls-validator-bin /opt/media-tools/mediastreamvalidator` | Apple executable on macOS; [external checks](#verification) |
| `--php-bin /opt/media-tools/php` | PHP executable for the local DASH-IF adapter; [adapter setup](#verification) |

Executable values accept one program name on PATH or one path, not a shell command or appended arguments. Quote spaces, for example `--packager-bin "C:\Media Tools\packager.exe"`. CLI archives do not bundle these tools. Keep keys and authentication outside media trees and version control.

## doctor

| Option | Meaning/default |
| --- | --- |
| `--json` | One diagnostic result |
| `--report-file NEW_FILE` | Save its JSON result while retaining readable stdout |
| `--probe` | Execute bounded version/help checks; default only discovers files |
| `--output-config FILE` | Select tool requirements from an output configuration |
| `--route PROFILE` | Advanced route selection; default `all` |
| `--decode` | Include dependencies for additional complete decoding |

Without a source profile, configured discovery conservatively checks native conversion requirements and requested external tools; it does not qualify a media route.

Routes are `all`, `progressive-mp4`, `fragmented-mp4`, `dash-presentation`, `fmp4-hls-presentation`, `fmp4-hls-ts`, `mpeg-ts-hls`, `mpeg-ts-sample-aes-hls`, `mpeg-ts-hls-ts`, `mpeg-ts-dash`, `fmp4-segments` and `mpeg-ts-segments`. Readiness covers tool setup, not media compatibility.

## analyze

| Argument/option | Meaning |
| --- | --- |
| `INPUT` | Local or HTTP(S) MP4, DASH or HLS; [URL conditions](#url-input) |
| `--metadata-only` | Read declarations without keys or complete media checks; no retained source |
| `--download-dir NEW_DIR` | Required for full URL analysis; refused with metadata-only |
| `--fetch-config FILE` | URL-only fetch settings; also accepted with metadata-only |
| `--keys-file FILE` | Source keys for full analysis; refused with metadata-only |
| `--decode` | Add complete source decoding; refused with metadata-only |
| `--max-workers N` | Upper bound 1–64; otherwise CPU/memory sizing; refused with metadata-only |
| `--packager-bin FILE` | Tool override for applicable preparation; refused with metadata-only |
| `--json`, `--stats`, `--report-file FILE` | Result and statistics options described above |

Local input refuses download/fetch options. Complete analysis checks the source; it does not verify a converted result.

## plan

| Argument/option | Meaning |
| --- | --- |
| `INPUT` | Local or supported URL source; saving a URL requires `--download-dir`; otherwise only bounded metadata is read |
| `--encryption MODE` | Quick output protection: `clear`, `cenc`, `cbcs`, `hls-aes128`, `hls-sample-aes` |
| `--output-format FORMAT` | `mp4`, `dash`, `hls-fmp4`, `hls-ts`; alternatives without `--save`, one resolved quick-output format when saving |
| `--output-config FILE` | Schema-4 output conditions instead of quick encryption/format; requires `--save` |
| `--output-dir NEW_DIR` | HLS/DASH destination or configured result root; may come from JSON |
| `--output-file NEW_FILE` | Quick MP4 file destination; cannot accompany an output configuration |
| `--save NEW_FILE` | Validate the complete source and save its fingerprints and resolved output conditions |
| `--download-dir NEW_DIR`, `--fetch-config FILE` | URL source acquisition while saving; [chapter 8](#url-input) |
| `--processing MODE` | `auto`, `segments`, `stitch`; requires `--save` |
| `--dash-addressing MODE` | DASH only: `template`, `template-timeline`, `list`, `base`; requires `--save` |
| `--manifest-profile PROFILE` | Native manifest rules: `base` or HLS-only `apple-vod` |
| `--external-manifest-check` | Require additional external final-manifest checks |
| `--keys-file FILE` | Source-check keys; requires `--save` |
| `--decode` | Save complete decoding as a minimum; requires `--save` |
| `--packager-bin FILE`, `--max-workers N` | Applicable tool override and worker cap; require `--save` |
| `--json`, `--stats`, `--report-file FILE` | Result options; statistics require `--save` |

A saved plan fixes its source and conversion conditions. Keys and runtime executable locations remain separate.

## convert

| Argument/option | Meaning |
| --- | --- |
| `INPUT` | Local original or supported URL; omitted with `--plan-file` |
| `--plan-file FILE` | Execute a saved plan without source/destination/intent overrides |
| `--output-config FILE` | One to eight named outputs; conflicts with quick encryption/format and a saved plan |
| `--encryption MODE`, `--output-format FORMAT` | Quick single-output conditions; formats and schemes as above |
| `--output-dir NEW_DIR` | New streaming destination or configured root; overrides JSON `output_dir` in direct conversion |
| `--output-file NEW_FILE` | New quick MP4 file; conflicts with an output configuration |
| `--download-dir NEW_DIR`, `--fetch-config FILE` | URL retention and optional HTTP settings |
| `--keys-file FILE` | Schema-2 input/output keys; `outputs.default` for a quick result |
| `--processing MODE`, `--dash-addressing MODE` | Direct output conditions; fixed by a saved plan |
| `--manifest-profile PROFILE` | Native manifest rules; HLS/DASH only |
| `--external-manifest-check` | Add required external manifest checks |
| `--decode` | Add complete decoded comparison; may strengthen a saved minimum |
| `--max-workers N` | Upper bound 1–64; automatic CPU/memory sizing otherwise |
| `--packager-bin FILE` | Shaka Packager executable; default `packager` |
| `--hls-validator-bin FILE`, `--php-bin FILE` | External-check tools; defaults `mediastreamvalidator` and `php` |
| `--json`, `--stats`, `--report-file FILE` | Result and statistics options described above |

Conversion already compares media with its original and checks applicable final manifests and DRM declarations. A later `verify` reads the saved files again for independent evidence.

## verify

| Argument/option | Meaning |
| --- | --- |
| `RESULT` | Candidate MP4/MPD/M3U8, or an existing root with `--output-config`; omitted with a saved plan |
| `--source INPUT` | Original media; required without a saved plan or manifest-only mode |
| `--output-config FILE` | Expected named outputs under `RESULT`; [configuration reference](https://github.com/kazuhide/drmr/blob/main/docs/configuration.md) |
| `--plan-file FILE` | Check fixed source fingerprints, output conditions and independent media correspondence |
| `--manifest-only` | Native manifest checks without media comparison or keys |
| `--manifest-profile PROFILE` | `base` or HLS-only `apple-vod` |
| `--external-manifest-check` | Add required external manifest checks |
| `--keys-file FILE` | Schema-2 source/result keys; `outputs.default` for a quick result |
| `--decode` | Add complete decoding; refused with manifest-only; may strengthen a saved minimum |
| `--packager-bin FILE`, `--hls-validator-bin FILE`, `--php-bin FILE` | Applicable tool overrides |
| `--max-workers N` | Worker upper bound; admission can reduce concurrency |
| `--json`, `--stats`, `--report-file FILE` | Result and statistics options described above |

Verification reads actual source and result files. It does not rebuild the result. Media equality, requested encryption/DRM declarations, native manifest coverage and requested external checks remain separate evidence; none proves license playback.

## version

`drmr version` and `drmr --version` print the product version.

## serve

| Option/environment | Meaning/default |
| --- | --- |
| `--static-dir DIR` | Trusted replacement for embedded UI files |
| `--drmr-bin FILE` | Same-version drmr job executable, `drmr.exe` on Windows |
| `DRMR_INPUT_ROOT` | Required allowed source/plan folder |
| `DRMR_OUTPUT_ROOT` | Required folder for results, retained originals and logs |
| `DRMR_BIND_HOST`, `DRMR_BIND_PORT` | Default loopback and port 8080; remote exposure is not intended |
| `DRMR_MAX_WORKERS` | Saved-plan worker cap |
| `DRMR_COMMON_CONFIG_ROOT` | Private key/fetch configuration folder, separate from media roots |

See [Web console setup](#web-console). Web and Desktop remain experimental.

# 18. References and maintenance

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
