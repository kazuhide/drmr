# Conversion support

Complete media admission requires the implemented AVC (H.264) or typed AAC
profiles. Metadata-only recognition of HEVC, AV1, VP9, AC-3, E-AC-3, Opus or FLAC
configuration is separate: complete operations refuse these unimplemented native
profiles with `native-codec-profile-not-supported`, including with `--decode`.
The container and encryption tables below remain subject to this codec/profile
requirement; they do not qualify every codec the formats can contain.

One qualified source can also produce [multiple named outputs](multiple-outputs.md)
through the common CLI. The `auto` and `stitch` multi-DRM routes support qualified
clear HLS transport streams and the qualified clear MP4 profile: CBCS DASH with
Widevine/PlayReady, CBCS fMP4 HLS with FairPlay, or SAMPLE-AES TS HLS with FairPlay.
These use separate runtime DRM settings and independently checked final
manifests. Requested [external manifest validation](manifest-validation.md) is
additional to native Rust manifest linting and the complete internal media checks.
Native `segments` routes also accept qualified known-key encrypted TS or fMP4
for the corresponding same-container DRM outputs. See the
[processing-specific DRM conditions](drm-signaling.md).

`drmr convert` processes local, completed on-demand media with keys supplied by its operator. It preserves encoded video/audio and verifies the result against the original source. The tables distinguish supported input profiles from tested protection routes. Native Rust verification reads every selected sample, codec configuration, clock and supported playback window. Add `--decode` for complete source decoding and decoded comparison; it cannot bypass native refusals. Historical results below retain their recorded complete-decoding scope and do not establish qualification of a later source build. See [verification depth and evidence](verification.md). DRM license and device playback support are separate.

For public-URL command examples, select an output protection in the
[linked input/output matrix](https://kazuhide.github.io/drmr/#support). Each example
covers acquisition, analysis, a saved execution plan, conversion and independent
verification. All examples use `analyze URL --download-dir` to retain the source
before planning; no separate download command is needed.

## HLS inputs and containers

Clear HLS input is supported. HLS describes playlists; their media can use either
fragmented MP4 (fMP4) or MPEG transport stream (MPEG-TS) segments. Clear means
unencrypted media. The two containers have different conversion limits:

| HLS input | Input container | DASH output protection | fMP4 HLS output protection | MPEG-TS HLS output protection |
| --- | --- | --- | --- | --- |
| Clear HLS | fMP4 | Clear, `cenc`, `cbcs` | Clear, `cbcs`, AES-128 | Not supported |
| `cenc` HLS | fMP4 | Clear, `cenc`, `cbcs` | Clear, `cbcs`, AES-128 | Not supported |
| `cbcs` HLS | fMP4 | Clear, `cenc`, `cbcs` | Clear, `cbcs`, AES-128 | Not supported |
| AES-128 HLS | fMP4 | Clear, `cenc`, `cbcs` | Clear, `cbcs`, AES-128 | Not supported |
| Clear HLS | MPEG-TS | Clear, `cenc`, `cbcs` | Clear, `cbcs`, AES-128 | Clear, AES-128, SAMPLE-AES |
| AES-128 HLS | MPEG-TS | Clear, `cenc`, `cbcs` | Clear, `cbcs`, AES-128 | Clear, AES-128, SAMPLE-AES |
| SAMPLE-AES HLS | MPEG-TS | Clear, `cenc`, `cbcs` | Clear, `cbcs`, AES-128 | Clear, AES-128, SAMPLE-AES |

Choose `--format hls` for fMP4 HLS or `--format hls-ts` for MPEG-TS HLS.
For FairPlay transport-stream encryption, use `--to hls-sample-aes`.
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
produce clear, `cenc`, or `cbcs` MP4. Combining an HLS presentation's separate
audio and video tracks into one MP4 is outside the current profile. MPEG-TS has
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
| DASH | Static MPD with one `Period`, `SegmentTemplate` addressing and an explicit finite `SegmentTimeline`; all referenced initialization and media files are saved | `input.mpd` |
| HLS | Completed TS/fMP4 playlists within the profiles above | `input.m3u8` |

HLS acquisition accepts clear TS/fMP4, AES-128 TS/fMP4, SAMPLE-AES TS, and CENC/CBCS
fMP4. MP4 and DASH retain clear or encrypted bytes for the same local source
checks. Supply input keys using `--keys-file`;
encrypted media and initialization bytes are retained unchanged. Key URI
resolution and input-key mapping are described in the [usage guide](usage.md#convert-a-url).
The acquisition step does not fetch key or license URLs.

For DASH, drmr retains the original MPD and creates a local MPD whose media
references point at the saved resources. Protection declarations, PSSH DRM
initialization data and the explicit timeline are retained. Downloading the MPD
alone is insufficient. The same origin, authentication and resource policies
apply to every fetched format and to redirects.

Live input, byte ranges, multiple DASH periods, `SegmentList`, `SegmentBase`,
duration-only DASH templates and negative timeline repeats remain outside the
current acquisition profile. These are implementation limits, rather than
technical impossibilities of those formats.

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
| MP4 file | Clear, `cenc`, `cbcs` | One track, or the audio and video from one qualified progressive source below |
| DASH directory | Clear, `cenc`, `cbcs` | Separate fragmented MP4 tracks in one static presentation |
| HLS directory (`hls`) | Clear, `cbcs`, AES-128 | fMP4 resources and completed playlists |
| HLS directory (`hls-ts`) | Clear, AES-128, SAMPLE-AES | Supported local TS input; separate TS audio/video playlists when both are present |

### Progressive MP4 with a finite audio range

MP4 can use an edit list to specify which part of decoded audio should play. This commonly excludes encoder warm-up samples at the start and padding at the end; it is playback metadata, rather than a different audio format. drmr preserves a qualified finite range across these output choices:

| Progressive MP4 input | MP4 output | DASH output | fMP4 HLS output |
| --- | --- | --- | --- |
| Clear | Clear, `cenc`, `cbcs` | Clear, `cenc`, `cbcs` | Clear, `cbcs`, AES-128 |
| `cenc` | Clear, `cenc`, `cbcs` | Clear, `cenc`, `cbcs` | Clear, `cbcs`, AES-128 |
| `cbcs` | Clear, `cenc`, `cbcs` | Clear, `cenc`, `cbcs` | Clear, `cbcs`, AES-128 |

The source must contain one AAC-LC (AAC Low Complexity) audio track stored as `mp4a`, with optional H.264 video stored as `avc1`, from the same complete progressive file. A single normal-rate audio edit must have a nonnegative start and a finite duration that converts exactly into audio clock units. Encrypted input requires every track key by identifier. The supported encryption layouts and presentation checks are described in [processing and verification](media-processing.md#finite-audio-presentation-windows). Multiple edits, initial empty edits, rate changes, extra tracks, combining different source files, key rotation, and input DRM initialization headers remain outside this profile.

Clear MP4 output retains progressive sample tables. Encrypted MP4 output contains fragments and can retain both tracks from this qualified source in one file. DASH and fMP4 HLS use separate audio and video tracks with a two-second segment target. Each track keeps its original finite playback range; packet payloads, absolute clocks and playback settings must match before publication. Complete decoded media must also match when `--decode` is required. Separate DASH/HLS source tracks are not combined into one MP4 by this feature.

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

The ten-second Big Buck Bunny regressions cover all nine output choices from each input protection, with audio-only and joint video/audio sources: 54 routes. Every result must pass conversion and fresh verification against the original protected source, including the exact playback range. Additional cases change the audio end by one sample, use reordered video pictures, and reject modified keys, payloads, playback settings, display properties, and manifest declarations. The [public encryption fixtures](https://github.com/kazuhide/drmr/blob/main/crates/drmr/tests/fixtures/bbb/CENC.md) use distinct video/audio keys. GPAC is used only for offline fixture generation, not by tests or conversion. These media checks do not qualify a license service or device playback.

```sh
cargo test --locked -p drmr-core --test conversion_finite_outputs -- --nocapture
cargo test --locked -p drmr --test conversion_finite_outputs -- --nocapture
```

Earlier clear-output finite-edit regressions also cover eight- and sixteen-byte CENC sample vectors and known-key verification of existing CENC/CBCS files carrying DRM initialization metadata. Conversion's stricter input rules still apply. Use CI artifacts for the exact revision, platforms, and tool versions tested.

The backend also tests joint video/audio HLS with clear, `cbcs`, and AES-128 output. `cenc` HLS output is refused. HLS AES-128 applies to HLS resources, so it cannot be selected as a standalone MP4 or DASH protection scheme. `cens` and `cbc1` can be identified but are not conversion targets.

Local manifests must stay inside the source directory tree. Key URI mappings come from the protected operator configuration; no key URI or remote media is retrieved. Live input, discontinuities, byte ranges, multiple DASH periods, unsupported addressing, and rendition metadata that cannot be retained are rejected. Local MPEG transport stream input has a separate, narrower profile described below. See [processing and verification](media-processing.md) for the exact source profile, resource limits, audio trimming, segmentation, and verification behavior.

For example, to remove CBCS protection from a qualified local DASH source using keys in the input section of `source-keys.json`:

```sh
drmr convert protected/manifest.mpd --to clear --format dash --output clear-dash --keys-file source-keys.json
drmr verify clear-dash/manifest.mpd --source protected/manifest.mpd --keys-file source-keys.json --json
```

Clear output needs no output-key section. Keep the original media and keys until verification succeeds. License acquisition, DRM system qualification, and real device playback remain separate checks.

## MPEG transport stream input

Completed local HLS playlists containing clear, AES-128, or SAMPLE-AES transport stream segments can produce DASH with clear, `cenc`, or `cbcs` protection, or fragmented MP4 HLS with clear, `cbcs`, or AES-128 protection. Encrypted input requires operator-supplied keys. The profile accepts one progressive H.264 video stream, one 44.1 or 48 kHz AAC-LC audio stream, or both, in a single program. Audio-only input supports DASH, fMP4 HLS, and TS output with the strict native audio timing checks described in [processing and verification](media-processing.md#mpeg-transport-stream-input). SAMPLE-AES-CTR transport streams and standalone MP4 output from this input are refused.

The 1.0.1 source includes 44.1 kHz AAC support for this TS profile. The unchanged v1.0.0 archives accept only 48 kHz AAC from TS. See [installation](../README.md#install) and [release status](releases.md) when choosing a build.

A supported AAC-only rendition may declare a playback end inside its final frame. drmr retains the complete encoded frame and preserves the shorter playback range in HLS or DASH. The declaration must identify one exact audio-sample boundary; ambiguous precision, clipping a nonfinal segment, or omitting a whole frame is refused. Standalone MP4 output from TS remains outside this profile. See [AAC playback boundaries](media-processing.md#playback-ending-inside-the-final-aac-frame).

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

## MPEG transport stream output

Clear, AES-128, and SAMPLE-AES TS inputs can produce all three TS output
protections. Use `--format hls-ts`, with `--to clear`, `--to hls-aes128`, or
`--to hls-sample-aes`. `cenc` and `cbcs` apply to MP4 samples and are refused
with this output container. MP4, fragmented MP4 HLS, and DASH inputs are outside
the current TS output profile.

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
can carry FairPlay declarations. TS output requires a supported TS source.
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
