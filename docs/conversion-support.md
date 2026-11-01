# Conversion support

MP4 and fragmented MP4 presentations accept the implemented AVC (H.264),
[HEVC Main/Main10](verification.md#hevc-in-mp4-and-fmp4) and typed AAC profiles.
HEVC supports clear, `cenc` and `cbcs` media in MP4, fMP4 HLS and DASH; transport
stream video still requires the H.264 profile. These codec conditions apply to
every container/encryption table below. A format can contain other codecs that
drmr does not completely validate.

HEVC support is included in the current source, not the v1.0.3 release archives.
Recognizing AV1, VP9, AC-3, E-AC-3, Opus or FLAC configuration remains separate
from complete native validation. Full operations refuse unimplemented codec
profiles, even with `--decode`. See the [HEVC limits and regression evidence](verification.md#hevc-in-mp4-and-fmp4).

One qualified source can also produce [multiple named outputs](multiple-outputs.md)
through the common CLI. The `auto` and `stitch` multi-DRM routes support qualified
HLS transport streams, fMP4 HLS, DASH and qualified MP4 sources with known keys.
CENC or CBCS DASH can carry Widevine and PlayReady declarations; CBCS fMP4 HLS
can carry FairPlay, Widevine and PlayReady together, while CENC fMP4 HLS carries
Widevine and PlayReady. SAMPLE-AES TS HLS carries FairPlay declarations.
These use separate runtime DRM settings and independently checked final
manifests. Requested [external manifest validation](manifest-validation.md) is
additional to native Rust manifest linting and the complete internal media checks.
Native `segments` routes also accept qualified known-key encrypted TS or fMP4
for the corresponding same-container DRM outputs. See the
[processing-specific DRM conditions](drm-signaling.md).

`drmr convert` processes local, completed on-demand media with keys supplied by its operator. It preserves encoded video/audio and verifies the result against the original source. The tables distinguish supported input profiles from tested protection routes. Native Rust verification reads every selected sample, codec configuration, clock and supported playback window. Add `--decode` for complete source decoding and decoded comparison; it cannot bypass native refusals. Historical results below retain their recorded complete-decoding scope and do not establish qualification of a later source build. See [verification depth and evidence](verification.md). DRM license and device playback support are separate.

For public-URL examples, select an output protection in the
[linked input/output matrix](https://kazuhide.github.io/drmr/#support). Each recipe
links complete output and input-specific key files and provides one direct
`convert` command for the selected input. Read the
[configuration reference](configuration.md) for file formats and defaults.
Separate analysis, saved planning and later independent verification are optional
workflows; they are not prerequisites for direct conversion.

## Choose the output

The admitted input and output container can be selected independently within the
conditions below. This avoids repeating the same output choices for each input
protection scheme:

| `format` | `target` choices | Entry point under `result/ID/` |
| --- | --- | --- |
| `mp4` | `clear`, `cenc`, `cbcs` | `media.mp4` |
| `dash` | `clear`, `cenc`, `cbcs` | `manifest.mpd` |
| `hls` (fMP4) | `clear`, `cenc`, `cbcs`, `hls-aes128` | `master.m3u8` |
| `hls-ts` (MPEG-TS) | `clear`, `hls-aes128`, `hls-sample-aes` | `master.m3u8` |

These are container/protection capabilities, not qualification of every codec,
track layout or playback window. MP4 input must be fragmented for TS output.
DASH/fMP4 HLS to one MP4 requires a single track; the TS-to-MP4 profile can retain
one video and one associated audio. See the corresponding conditions below.

## HLS inputs and containers

Clear HLS input is supported. HLS describes playlists; their media can use either
fragmented MP4 (fMP4) or MPEG transport stream (MPEG-TS) segments. Clear means
unencrypted media. The two containers have different conversion limits:

| HLS input container | Accepted input protection | Key mapping |
| --- | --- | --- |
| fMP4 | Clear, `cenc`, `cbcs`, AES-128 | cenc/cbcs by KID; AES-128 by exact resolved key URI |
| MPEG-TS | Clear, AES-128, SAMPLE-AES | Encrypted input by exact resolved key URI; SAMPLE-AES needs its associated IV |

Choose `--format hls` for fMP4 HLS or `--format hls-ts` for MPEG-TS HLS.
For FairPlay transport-stream encryption, use `--to hls-sample-aes`.
CENC fMP4 HLS uses `SAMPLE-AES-CTR`; it is a base-HLS profile and is not compatible
with Apple's authoring requirements or FairPlay. CBCS fMP4 HLS uses `SAMPLE-AES`
and can carry all three DRM declarations. These declarations do not establish
license-server or device playback support. See [DRM signaling](drm-signaling.md).
SAMPLE-AES TS output is technically supported by HLS; the supported drmr profile
is described below. Restrictions in this table describe drmr's current
implementation, rather than an impossibility of the media formats.

These are supported profiles for local, completed playlists, subject to the
[media and verification restrictions](media-processing.md). Each fMP4 media
playlist must contain one audio or video track; a master playlist can reference
several video qualities and one shared audio playlist, including its supported name,
language and HLS selection declarations. DASH maps that sole audio rendition to
standard language, label and author-priority fields; identical automatic player
selection is not claimed. See [rendition metadata and its mapping](media-processing.md#multiple-qualities-and-audio-declarations).
Metadata outside that mapping is rejected rather than discarded. A single-track fMP4 HLS source can also
produce clear, `cenc`, or `cbcs` MP4. Combining separate fMP4 HLS audio and video
tracks into one MP4 is outside the current profile. TS HLS can retain one video
and one audio track in the [single-MP4 profile](#mpeg-transport-streams-to-one-mp4). MPEG-TS has
the [narrower input profile below](#mpeg-transport-stream-input).

## URL input

`analyze` can retain and fully check a supported HLS, DASH or MP4 URL before you save
an execution plan. `convert` can also acquire the URL and convert directly. The
response content determines the format; a filename extension is not required.
This does not extend the local codec, track, or output limits. Full URL acquisition requires
`--download-dir`, and the completed source is retained for offline conversion
and independent verification. See the [four-stage saved-plan workflow](executable-plans.md)
or [direct URL conversion and fetch settings](usage.md#convert-a-url).

For declarations without a retained source, use `analyze --metadata-only URL`.
This needs neither keys nor `--download-dir` and does not establish conversion
support. The acquisition profiles below apply to complete-source analysis and
conversion, not temporary metadata reading. See [metadata analysis](usage.md#read-metadata-and-plan-a-conversion)
for the distinct reads and limits.

| URL input | Acquisition profile | Retained local entrypoint |
| --- | --- | --- |
| MP4 | One complete MP4 file within the local media profiles | `input.mp4` |
| DASH | Static MPD with one `Period`; finite `SegmentTemplate` with nominal duration or timeline, `SegmentList`, or one indexed file per representation with `SegmentBase` | `input.mpd` |
| HLS | Completed TS/fMP4 playlists within the profiles above | `input.m3u8` |

HLS acquisition accepts clear TS/fMP4, AES-128 TS/fMP4, SAMPLE-AES TS, and CENC/CBCS
fMP4. MP4 and DASH retain clear or encrypted bytes for the same local source
checks. Supply input keys using `--keys-file`;
encrypted media and initialization bytes are retained unchanged. Key URI
resolution and input-key mapping are described in the [usage guide](usage.md#convert-a-url).
The acquisition step does not fetch key or license URLs.

For DASH, drmr retains the original MPD and creates a local MPD whose media
references point at the saved resources. Protection declarations, PSSH DRM
initialization data and finite timing are retained. Referenced objects are saved
in full, including indexed files used through byte ranges. Downloading the MPD
alone is insufficient. The same origin, authentication and resource policies
apply to every fetched format and to redirects.

Live input, HLS byte ranges, multiple DASH periods, hierarchical or external DASH
indexes, and negative timeline repeats remain outside the acquisition profile.
See [DASH references](dash-addressing.md) for range and index conditions. Downloads
and segmented outputs retain the [source-relative directories](source-layout.md).
These are implementation limits, rather than technical impossibilities of the formats.

The URL regressions use ten-second Big Buck Bunny fixtures. The HLS cases use
240 H.264 video packets in five two-second segments; MP4 uses one video track,
and DASH uses separate video and audio tracks with two-second segment targets.

| Downloaded input | Output | Result |
| --- | --- | --- |
| Clear HLS with MPEG-TS segments | CBCS fragmented MP4 HLS | Content and timing verified; offline retry verified |
| Clear HLS with fragmented MP4 segments | CBCS fragmented MP4 HLS | Content and timing verified; offline retry verified |
| MP4: clear, `cenc`, `cbcs` | CBCS DASH from clear input; clear DASH from encrypted input | All three cases passed analysis, offline saved-plan conversion and independent verification |
| Static DASH: clear, `cenc`, `cbcs` | CBCS DASH from clear input; clear DASH from encrypted input | All three cases passed analysis, offline saved-plan conversion and independent verification |

The HLS cases compare the original playlist and resource bytes, preserve sequence
numbers and duration text, and independently verify the output. They stop the
source server before converting the saved input again. A separate failure case
checks that a completed download survives a missing packaging tool without
publishing output. These results do not establish DRM license or device playback.

The six MP4/DASH cases require authentication and a query string for their local
HTTP source. Each case runs `analyze URL --download-dir`, stops the server, saves
a plan from `input.mp4` or `input.mpd`, executes `convert --plan`, and runs
`verify --plan`. Encoded packets, timing and complete decoded content match the
source. Saved-source fingerprints and output intent also match; original media
and raw manifests remain unchanged. The cases check that credentials and keys
are absent from stdout, stderr and the saved plan. `license_playback_verified`
remains `false`. These six cases do not claim execution of every placeholder
recipe or support for other DASH addressing profiles.

```sh
cargo test --locked -p drmr --test url_formats_media -- --nocapture
```

## Sixteen-route protection regression

The following table records a specific regression fixture, rather than every
supported input/container combination. Clear HLS inputs are covered by the
profiles above; their absence from this fixture matrix does not mean they are
unsupported.


The attributed [ten-second Big Buck Bunny fixture](https://github.com/kazuhide/drmr/blob/main/crates/drmr/tests/fixtures/bbb/README.md) has H.264 video and stereo AAC audio. A clear fragmented DASH adapter supplies five two-second video segments; audio boundaries follow complete audio frames. Encrypted source presentations are generated from that adapter with public test keys. Source and result keys differ, including conversions that retain the same protection scheme.

| Input presentation | Clear DASH | `cenc` DASH | `cbcs` DASH | AES-128 HLS (fMP4) |
| --- | --- | --- | --- | --- |
| Clear DASH | Pass | Pass | Pass | Pass |
| `cenc` DASH | Pass | Pass | Pass | Pass |
| `cbcs` DASH | Pass | Pass | Pass | Pass |
| AES-128 HLS (fMP4) | Pass | Pass | Pass | Pass |

These sixteen routes passed locally on macOS ARM64 with FFmpeg 9.0.2 and Shaka Packager 3.8.0. Automated tests also passed on macOS ARM64 with FFmpeg 9.0.1, and on Linux x86_64 and Windows x64 with FFmpeg 9.0.2; all used Shaka Packager 3.8.0. Each result passed both conversion's checks and a separate `drmr verify` invocation: stream metadata, encoded packet content, complete decoded media, and every packet's presentation/decode time and duration matched the source. They do not establish support for every codec, manifest profile, input produced by other tools, or playback device.

In this single local run, conversion took 1.24–2.39 seconds per route and the separate verification took 0.59–1.05 seconds. The CLI's sampled resident memory ranged from 11.8 to 15.0 MB; living descendants' sampled peaks ranged from 1.6 to 77.4 MB, measured separately every 100 milliseconds. These are sampled observations from a short fixture using a debug CLI, not exact memory maxima or throughput guarantees. A long or high-resolution presentation needs its own measurements.

Run the matrix with:

```sh
cargo test --locked -p drmr --test bbb_media the_real_cli_verifies_all_sixteen_bbb_protection_routes -- --nocapture
```

Set `DRMR_CONVERSION_REPORT` to a path outside the repository to save the JSON results. The report records the fixture hash, each actual route's checks, elapsed time, sampled memory and measurement limitations, platform, and tool versions. CI uploads its own results; use those artifacts for a particular revision and platform.

## Output formats and input limits

| Output format | Available protection | Initial profile |
| --- | --- | --- |
| MP4 file | Clear, `cenc`, `cbcs` | One track, qualified joint MP4, or one video and one audio from supported TS HLS |
| DASH directory | Clear, `cenc`, `cbcs` | Separate fragmented MP4 tracks in one static presentation |
| HLS directory (`hls`) | Clear, `cenc`, `cbcs`, AES-128 | fMP4 resources and completed playlists |
| HLS directory (`hls-ts`) | Clear, AES-128, SAMPLE-AES | Qualified local TS or fMP4 input; separate TS audio/video playlists when both are present |

### DRM declarations in fMP4 HLS

Encryption and DRM declarations are separate output conditions. A CBCS fMP4 HLS
output can advertise FairPlay, Widevine and PlayReady for the same encrypted
samples. A CENC fMP4 HLS output can advertise Widevine and PlayReady. Configure
the required systems through the protected key file's `multi-drm` setting;
see [DRM signaling](drm-signaling.md). Each selected system receives an
`EXT-X-KEY` declaration in every media playlist. The generated initialization
data and declarations are independently checked with the known media key.
These checks do not obtain licenses or qualify a playback device.

| HLS output protection | Segment container | Method | Available DRM declarations |
| --- | --- | --- | --- |
| `cenc` | fMP4 | `SAMPLE-AES-CTR` | Widevine, PlayReady |
| `cbcs` | fMP4 | `SAMPLE-AES` | FairPlay, Widevine, PlayReady |
| `hls-sample-aes` | MPEG-TS | `SAMPLE-AES` | FairPlay |

The HLS second-edition draft defines optional `SAMPLE-AES-CTR` for fMP4 using
CENC, with no playlist IV attribute. It does not define that method for TS.
This is draft-based HLS support, not a FairPlay output. [HLS draft section
4.4.4.4](https://datatracker.ietf.org/doc/html/draft-pantos-hls-rfc8216bis-22#section-4.4.4.4).
Apple's authoring requirements separately prohibit CTR on Apple devices.
The base HLS linter and Apple-specific profile retain that distinction.
[Apple content-protection requirements](https://developer.apple.com/streaming/hls-authoring-specification-for-apple-devices.html#Content-Protection).

### Progressive MP4 with a finite audio range

MP4 can use an edit list to specify which part of decoded audio should play. This commonly excludes encoder warm-up samples at the start and padding at the end; it is playback metadata, rather than a different audio format. drmr preserves a qualified finite range across these output choices:

| Progressive MP4 input | MP4 output | DASH output | fMP4 HLS output |
| --- | --- | --- | --- |
| Clear | Clear, `cenc`, `cbcs` | Clear, `cenc`, `cbcs` | Clear, `cenc`, `cbcs`, AES-128 |
| `cenc` | Clear, `cenc`, `cbcs` | Clear, `cenc`, `cbcs` | Clear, `cenc`, `cbcs`, AES-128 |
| `cbcs` | Clear, `cenc`, `cbcs` | Clear, `cenc`, `cbcs` | Clear, `cenc`, `cbcs`, AES-128 |

The source must contain one AAC-LC (AAC Low Complexity) audio track stored as `mp4a`, with optional H.264 video (`avc1`) or qualified HEVC Main/Main10 video (`hvc1`/`hev1`), from the same complete progressive file. A single normal-rate audio edit must have a nonnegative start and a finite duration that converts exactly into audio clock units. Encrypted input requires every track key by identifier. The supported encryption layouts and presentation checks are described in [processing and verification](media-processing.md#finite-audio-presentation-windows). Multiple edits, initial empty edits, rate changes, extra tracks, combining different source files, key rotation, and input DRM initialization headers remain outside this profile.

Clear MP4 output retains progressive sample tables. Encrypted MP4 output contains fragments and can retain both tracks from this qualified source in one file. DASH and fMP4 HLS use separate audio and video tracks with a two-second segment target. Each track keeps its original finite playback range; packet payloads, absolute clocks and playback settings must match before publication. Complete decoded media must also match when `--decode` is required. Separate DASH/HLS source tracks are not combined into one MP4 by this feature.

The resulting joint fragmented MP4 can also be converted again to clear, `cenc` or `cbcs` MP4. This reinput path requires exactly one supported video and one AAC-LC audio track, matching protection, and one positive normal-rate finite edit per track in the same complete file. It packages the tracks separately, restores each original window, combines them, and independently checks every sample, clock and playback setting before publication. Unlike progressive-to-clear copying, clear output from this path remains fragmented. Other multi-track fragmented layouts are not admitted by this condition.

For clear input and output, no key configuration is needed:

```sh
drmr convert original.mp4 --to clear --format mp4 --output clear.mp4
drmr verify clear.mp4 --source original.mp4 --json
```

For encrypted input, supply every track's identifier and key in the protected configuration's `input.keys` array. Clear output needs no output key:

```sh
drmr convert encrypted.mp4 --to clear --format mp4 --output clear.mp4 --keys-file source-keys.json
drmr verify clear.mp4 --source encrypted.mp4 --keys-file source-keys.json --json
```

For CBCS DASH, provide a separate output key as well:

```sh
drmr convert original.mp4 --to cbcs --format dash --output result-dash --keys-file keys.json
drmr verify result-dash/manifest.mpd --source original.mp4 --keys-file keys.json --json
```

The existing finite-output Big Buck Bunny regression covers its original nine output choices from each input protection, with audio-only and joint video/audio sources: 54 routes. The release-only round-trip matrix additionally covers CENC HLS under its stated fixture conditions. Every result must pass conversion and fresh verification against the original protected source, including the exact playback range. Additional cases change the audio end by one sample, use reordered video pictures, and reject modified keys, payloads, playback settings, display properties, and manifest declarations. The [public encryption fixtures](https://github.com/kazuhide/drmr/blob/main/crates/drmr/tests/fixtures/bbb/CENC.md) use distinct video/audio keys. GPAC is used only for offline fixture generation, not by tests or conversion. These media checks do not qualify a license service or device playback.

```sh
cargo test --locked -p drmr-core --test conversion_finite_outputs -- --nocapture
cargo test --locked -p drmr --test conversion_finite_outputs -- --nocapture
```

Earlier clear-output finite-edit regressions also cover eight- and sixteen-byte CENC sample vectors and known-key verification of existing CENC/CBCS files carrying DRM initialization metadata. Conversion's stricter input rules still apply. Use CI artifacts for the exact revision, platforms, and tool versions tested.

The HLS backend supports joint video/audio with clear, `cenc`, `cbcs`, and AES-128 output. CENC and CBCS operate on fragmented MP4 samples; AES-128 encrypts complete HLS resources. HLS AES-128 applies to HLS resources, so it cannot be selected as a standalone MP4 or DASH protection scheme. `cens` and `cbc1` can be identified but are not conversion targets.

Local manifests must stay inside the source directory tree. Key URI mappings come from the protected operator configuration; no key URI or remote media is retrieved. Live input, discontinuities, HLS byte ranges, multiple DASH periods, unsupported addressing or index layouts, and rendition metadata that cannot be retained are rejected. Local MPEG transport stream input has a separate, narrower profile described below. See [DASH references](dash-addressing.md) and [processing and verification](media-processing.md) for the exact source profiles, resource limits, audio trimming, segmentation, and verification behavior.

For example, to remove CBCS protection from a qualified local DASH source using keys in the input section of `source-keys.json`:

```sh
drmr convert protected/manifest.mpd --to clear --format dash --output clear-dash --keys-file source-keys.json
drmr verify clear-dash/manifest.mpd --source protected/manifest.mpd --keys-file source-keys.json --json
```

Clear output needs no output-key section. Keep the original media and keys until verification succeeds. License acquisition, DRM system qualification, and real device playback remain separate checks.

## MPEG transport stream input

Completed local HLS playlists containing clear, AES-128, or SAMPLE-AES transport stream segments can produce DASH with clear, `cenc`, or `cbcs` protection, or fragmented MP4 HLS with clear, `cenc`, `cbcs`, or AES-128 protection. Encrypted input requires operator-supplied keys. The profile accepts one progressive H.264 video stream, one 44.1 or 48 kHz AAC-LC audio stream, or both, in a single program. Audio-only input supports MP4, DASH, fMP4 HLS, and TS output with the strict native audio timing checks described in [processing and verification](media-processing.md#mpeg-transport-stream-input). SAMPLE-AES-CTR transport streams are refused. Standalone MP4 has the selection conditions described [below](#mpeg-transport-streams-to-one-mp4).

The 1.0.1 source includes 44.1 kHz AAC support for this TS profile. The unchanged v1.0.0 archives accept only 48 kHz AAC from TS. See [installation](../README.md#install) and [release status](releases.md) when choosing a build.

A supported AAC-only rendition may declare a playback end inside its final frame. drmr retains the complete encoded frame and preserves the shorter playback range in MP4, HLS or DASH. The declaration must identify one exact audio-sample boundary; ambiguous precision, clipping a nonfinal segment, or omitting a whole frame is refused. See [AAC playback boundaries](media-processing.md#playback-ending-inside-the-final-aac-frame).

For example, create CBCS DASH from a supported local TS playlist:

```sh
drmr convert source/media.m3u8 --to cbcs --format dash --output result-dash --keys-file keys.json
drmr verify result-dash/manifest.mpd --source source/media.m3u8 --keys-file keys.json --json
```

Use separate input and output keys in the [protected configuration](usage.md#separate-input-and-output-keys). Clear output needs no output key. The conversion decrypts when required, separates the original tracks, and repackages them into fMP4 segments and a static DASH manifest. It preserves the encoding and original packet clocks; it does not re-encrypt each source segment in place. See [DASH timing from transport streams](media-processing.md#dash-output-from-transport-streams) for the manifest's time mapping and duration representation.

For 44.1 kHz AAC, the private MP4 preparation retains the original 90 kHz media clock and declares the actual audio sample rate separately. Every original PTS, DTS, AAC payload, frame count, decoded sample, and final playback end must match. Nominal TS packet duration labels can differ from MP4 sample durations without changing those clocks; see [AAC sample rates and transport clocks](media-processing.md#aac-sample-rates-and-transport-clocks) for the exact comparison.

The ten-second BBB regression covers all nine input/output protection pairs for each of video-only, joint video/audio, and audio-only TS: 27 routes. Each result must pass conversion checks and a fresh verification against both its original protected source and a matching clear reference. Checks cover every original frame, complete decoded media, packet timing, the shared presentation origin, source immutability, and rejected manifest changes. Additional cases cover reordered video pictures. These tests qualify the bounded media profile; they do not establish DRM licenses or device playback.

```sh
cargo test --locked -p drmr-core --test conversion_ts_output dash_ -- --nocapture
cargo test --locked -p drmr --test conversion_ts_dash -- --nocapture
```

Audio-only TS to fMP4 HLS has a separate nine-pair regression: clear, AES-128,
and SAMPLE-AES input, each converted to clear, `cbcs`, and AES-128 output.
The complete ten-second 48 kHz fixture contains 470 AAC frames; every frame and all
decoded samples are compared with the original TS. Input and output use separate
keys. Conversion checks and fresh verification include the actual master and
media playlists, initialization segment, every media segment, packet clocks,
and final playback end. Altered media, times, durations, playlist endings, and
keys must fail. Run this regression with:

```sh
cargo test --locked -p drmr-core --test conversion_ts_output audio_hls -- --nocapture
```

Earlier fMP4 HLS regressions passed clear, `cbcs`, and AES-128 output from clear MPEG-TS input, with separate verification on Linux x86_64 and Windows x64 with FFmpeg 9.0.2, and macOS ARM64 with FFmpeg 9.0.1; all used Shaka Packager 3.8.0. Separate AES-128 MPEG-TS input regressions verify clear HLS output with key rotation, explicit and sequence-derived initialization vectors, and transitions to clear segments. Additional cases cover reordered pictures and video-only input. These results qualify the tested profile, tools, and platforms. They do not establish other codecs or arbitrary broadcast streams. See [the exact input and verification limits](media-processing.md#mpeg-transport-stream-input).

SAMPLE-AES TS is the sample encryption used for FairPlay transport-stream media.
Its AES-CBC protection applies to specified parts of encoded frames; it differs
from encrypting a complete segment with HLS AES-128 and from storing `cbcs` in MP4.
Supply each key and its associated initialization vector (IV) through the
[protected key configuration](usage.md#separate-input-and-output-keys).
An explicit playlist IV can supply that value; when both are present, they must
match. drmr does not infer a SAMPLE-AES IV from the segment number.

The SAMPLE-AES profile keeps one encryption method and a fixed program and codec
configuration throughout the presentation. Use one key format: `identity` (the
default) or `com.apple.streamingkeydelivery`, with format version 1 when supplied.
Simultaneous declarations in different key formats are refused. AAC uses seven-byte transport headers
without a checksum and declares no initial audio priming (decoder warm-up
samples). Changes to other
encryption methods, unsupported audio setup, and malformed or oversized encoded
frames are refused. See [how the original encrypted streams are verified](media-processing.md#sample-aes-transport-streams).
Known-key media conversion does not acquire a FairPlay license or qualify device playback.

The generated SAMPLE-AES regression uses the attributed ten-second fixture with
two-second video segments. It checks video-only and joint video/audio input
against clear, `cbcs`, and AES-128 fMP4 HLS output. Conversion and separate
verification compare the original encrypted source and an independently clear
reference, including the final video picture and all decoded audio samples.
Run it with:

```sh
cargo test -p drmr-core --test conversion_sample_aes --locked
```

## MPEG transport streams to one MP4

Use `--format mp4` with `--to clear`, `--to cenc` or `--to cbcs` to produce one
fragmented MP4 file from qualified TS HLS. Clear, AES-128 and SAMPLE-AES inputs
use the same known-key input checks as other TS routes. Audio only, video only,
or one video and one audio track are supported. A single multiplexed TS
playlist or one selected video playlist with its associated audio can supply
the tracks. `auto` and `stitch` prepare complete tracks without decoding;
`segments` refuses this container change. MP4 output has no HLS or DASH manifest.

Restrict a master with several video qualities or audio choices to the desired
video and associated audio before analysis. drmr refuses ambiguous selections;
it does not choose a quality or discard tracks. A video-only media playlist
produces video-only MP4. Supported audio-language metadata is retained, including
equivalent `en` and `eng` spellings. HLS group identifiers, rendition names and
automatic-selection flags describe playlist choices; they are not carried as
MP4 player-selection declarations. Unsupported language mappings are refused.

The MP4 retains all encoded samples and their absolute clocks. Standard MP4
playback edits preserve a supported shorter final AAC range, including with
video. The generated file can be analyzed and independently compared with the
original TS. The qualified canonical edits can also enter DASH or fMP4 HLS
conversion, and audio-only input can be re-encrypted as MP4. Every edit is bound
to all coded AAC frames and absolute clocks before conversion; this does not
admit arbitrary edit lists. Optional `--decode` adds complete decoded-media comparison.

The regression covers all nine input/output protection pairs for video only,
joint audio/video, audio only at 48 kHz, and audio only at 44.1 kHz. Additional
cases check exact partial AAC endings, language mapping, reordered video,
ambiguous-selection refusal and saved-plan integrity. The real CLI checks
analysis, saved planning, direct and saved-plan conversion, fresh verification,
and a named MP4/DASH output pair. These qualify the stated fixtures and profiles,
not license acquisition or device playback.

```sh
cargo test --locked -p drmr-core --test conversion_ts_output ts_mp4 -- --nocapture
cargo test --locked -p drmr --test conversion_ts_mp4 -- --nocapture
```

## MPEG transport stream output

Clear, AES-128, and SAMPLE-AES TS inputs can produce all three TS output
protections. Use `--format hls-ts`, with `--to clear`, `--to hls-aes128`, or
`--to hls-sample-aes`. `cenc` and `cbcs` apply to MP4 samples and are refused
with this output container. Qualified fragmented MP4 files, fragmented MP4 HLS,
and static DASH can also produce these TS outputs, with supplied keys for
encrypted input. Progressive MP4 remains outside the TS output profile.

Changing from fragmented MP4 to TS uses `--processing stitch` (or `auto`). It
prepares complete tracks, repackages them into TS segments, then independently
reads the final playlists and compares every encoded frame and timestamp with
the source. `--processing segments` refuses this container change. Existing
TS-to-TS segment processing remains available.

MP4 edits that exclude initial audio decoder warm-up samples cannot be carried
into this TS profile and are refused with
`ts-output-presentation-not-representable`, without publishing an output. A
qualified final playback end inside the last AAC frame is retained through the
HLS duration declaration. The input must have the supported exact transport
clock mapping; conversion does not apply a broad timestamp tolerance. See
[verification](verification.md) for the evidence provided by native checks.

The profile supports progressive H.264 video, 44.1 or 48 kHz AAC-LC audio, or both.
When both are present, output uses separate video and audio TS playlists joined
by an HLS master playlist. A single TS segment containing both tracks is not
produced by the current packager integration; HLS itself allows that layout.
The supported AAC transport headers contain seven bytes, no checksum, and one
raw data block. Unsupported configurations and audio priming are refused.

SAMPLE-AES output requires a key, its associated explicit IV, and the key URI
to advertise to FairPlay clients. The same output key and IV are used for both
roles in this profile. See the [configuration example](usage.md#separate-input-and-output-keys).
These are known-key media checks; license acquisition and device playback remain
separate.

The reproducible regression covers three input protections, three output
protections, and video-only, joint video/audio, and audio-only layouts: 27
combinations. It uses the attributed ten-second Big Buck Bunny fixture, with a
two-second video segment target and complete audio-frame boundaries. Each
conversion is followed by fresh verification from the original input;
encrypted inputs also have an independently clear reference. Run it with:

```sh
cargo test --locked -p drmr-core --test conversion_ts_output -- --nocapture
```

Refer to the CI results for the exact revision, platforms, and tool versions.
A passing short fixture does not qualify arbitrary broadcast streams or codecs.

## Explicit DRM signaling

The default `auto` and explicit `stitch` converter support a `multi-drm` profile for qualified clear
MPEG-TS HLS and clear MP4 sources. Fragmented MP4 DASH can carry Widevine,
PlayReady, or both with CBCS encryption; fragmented MP4 HLS and SAMPLE-AES TS HLS
can carry FairPlay declarations. TS output accepts supported TS or fragmented
MP4 sources, including known-key encrypted fragmented MP4.
With `segments`, supported clear or known-key encrypted TS/fMP4 sources can
generate the corresponding DRM output while retaining the source container and
sample boundaries.
See [DRM signaling](drm-signaling.md) for exact source restrictions, protected
runtime settings, native output checks, and license/playback boundaries.

## Explicit PlayReady signaling

With `auto` or `stitch`, generic `convert` supports an opt-in PlayReady profile: qualified clear MP4 with
H.264 video and optional AAC audio, from one complete source file, to CBCS DASH
with one output KID. Set `output.signaling` in the protected operator JSON as
shown in [usage](usage.md#separate-input-and-output-keys). Other layouts and
protection combinations refuse this setting. These are qualification boundaries
of this implementation, not limitations of the formats or DRM systems.

The PlayReady Object (PRO) contains a v4.3.0.0 WRMHEADER with `ALGID="AESCBC"`,
the output KID in GUID byte order, and no CHECKSUM. One generated Protection
System Specific Header (PSSH) supplies the initialization segments and the MPD's
`cenc:pssh` and `mspr:pro`. Every track's actual init PSSH and both manifest
records must match those generated bytes before publication. The final MPD is
then independently reopened and compared with the immutable original MP4 for
metadata, every packet payload and absolute clock, and complete decoded media.
A declaration whose presentation window cannot be verified is refused without
publishing an output, rather than rounded or trimmed to fit.

The source is separately checked for existing PSSH/protection and unknown
initialization metadata. This first profile refuses such metadata instead of
silently removing or replacing it. No license URL is fetched. This is a media
and signaling consistency check, not a PlayReady license or device qualification.
Existing conversions without this explicit setting retain their prior behavior.

`conversion_signaling.rs` uses the attributed ten-second BBB fixture, a two-second
segment target, all 240 video packets and, for the joint case, 470 AAC packets.
An independent FFmpeg fragmented remux provides the complete clear MP4 source;
all coded packet hashes, PTS, DTS, and durations are checked against the fixture.
As in the existing fragmented baseline, the progressive clip's final AAC
discard-padding declaration is not a claim of this derived source: its decoder
may present final padding. The conversion and fresh verification compare the
whole derived source without trimming, dropping packets, or applying timestamp
tolerances. This signaling regression uses the derived fragmented source; the
qualified finite progressive AAC profile is described above. Run the real CLI
regression with:

```sh
cargo test --locked -p drmr --test conversion_signaling -- --nocapture
```

## Fragmented MP4 to transport-stream regression

The ten-second Big Buck Bunny regression covers clear, CENC and CBCS DASH/HLS
inputs converted to clear or FairPlay SAMPLE-AES TS, including separate video
and audio. Audio-only cases retain 44.1 and 48 kHz frame clocks. Additional
cases cover a standalone fragmented MP4 and whole-segment AES-128 TS output.
The normal conversion and independent verification run with unavailable decoder
executables; one short route additionally checks complete decoded media.
Manifest, key/IV, resource-content and processing-mode changes must be rejected
by their corresponding checks. The initial-audio-edit refusal is tested separately.

```sh
cargo test --locked -p drmr-core --test conversion_fmp4_ts -- --nocapture
```
