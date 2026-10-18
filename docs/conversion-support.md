# Conversion support

`drmr convert` processes local, completed on-demand media with keys supplied by its operator. It preserves encoded video/audio and verifies the result against the original source. The tables distinguish supported input profiles from tested protection routes. DRM license and device playback support are separate.

## HLS inputs and containers

Clear HLS input is supported. HLS describes playlists; their media can use either
fragmented MP4 (fMP4) or MPEG transport stream (MPEG-TS) segments. Clear means
unencrypted media. The two containers have different conversion limits:

| HLS input | Input container | DASH output protection | fMP4 HLS output protection | MPEG-TS HLS output protection |
| --- | --- | --- | --- | --- |
| Clear HLS | fMP4 | Clear, `cenc`, `cbcs` | Clear, `cbcs`, AES-128 | Not supported |
| `cbcs` HLS | fMP4 | Clear, `cenc`, `cbcs` | Clear, `cbcs`, AES-128 | Not supported |
| AES-128 HLS | fMP4 | Clear, `cenc`, `cbcs` | Clear, `cbcs`, AES-128 | Not supported |
| Clear HLS | MPEG-TS | Not supported | Clear, `cbcs`, AES-128 | Clear, AES-128, SAMPLE-AES |
| AES-128 HLS | MPEG-TS | Not supported | Clear, `cbcs`, AES-128 | Clear, AES-128, SAMPLE-AES |
| SAMPLE-AES HLS | MPEG-TS | Not supported | Clear, `cbcs`, AES-128 | Clear, AES-128, SAMPLE-AES |

Choose `--format hls` for fMP4 HLS or `--format hls-ts` for MPEG-TS HLS.
For FairPlay transport-stream encryption, use `--to hls-sample-aes`.
SAMPLE-AES TS output is technically supported by HLS; the supported drmr profile
is described below. Restrictions in this table describe drmr's current
implementation, rather than an impossibility of the media formats.

These are supported profiles for local, completed playlists, subject to the
[media and verification restrictions](media-processing.md). Each fMP4 media
playlist must contain one audio or video track; a master playlist can reference
separate video and audio playlists. Rendition names, language and selection metadata
that cannot be preserved are rejected rather than discarded. A single-track fMP4 HLS source can also
produce clear, `cenc`, or `cbcs` MP4. Combining an HLS presentation's separate
audio and video tracks into one MP4 is outside the current profile. MPEG-TS has
the [narrower input profile below](#mpeg-transport-stream-input).

## URL input

`convert` can acquire a completed clear HLS URL before conversion. This URL
profile supports the same qualified TS/fMP4 media profiles described above;
it does not extend their codec, track, or output limits. URL input requires
`--download-dir`, and the completed source is retained for offline conversion
and independent verification. See [URL conversion and optional fetch settings](usage.md#convert-a-url).

Remote DASH, standalone MP4, encrypted HLS, live playlists, and byte ranges are
outside the initial acquisition profile. The local input rows above do not imply
that every matching URL input is implemented. These restrictions describe drmr's
current acquisition implementation.

The URL regression uses ten seconds of Big Buck Bunny H.264 video: 240 encoded
packets in five two-second segments. It checks both source containers:

| Downloaded input | Output | Result |
| --- | --- | --- |
| Clear HLS with MPEG-TS segments | CBCS fragmented MP4 HLS | Content and timing verified; offline retry verified |
| Clear HLS with fragmented MP4 segments | CBCS fragmented MP4 HLS | Content and timing verified; offline retry verified |

The tests compare the original playlist and resource bytes, preserve sequence
numbers and duration text, and independently verify the output. They stop the
source server before converting the saved input again. A separate failure case
checks that a completed download survives a missing packaging tool without
publishing output. These results do not establish DRM license or device playback.

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
| MP4 file | Clear, `cenc`, `cbcs` | One audio or video track; finite audio edits use the progressive profile below |
| DASH directory | Clear, `cenc`, `cbcs` | Separate fragmented MP4 tracks in one static presentation |
| HLS directory (`hls`) | Clear, `cbcs`, AES-128 | fMP4 resources and completed playlists |
| HLS directory (`hls-ts`) | Clear, AES-128, SAMPLE-AES | Supported local TS input; separate TS audio/video playlists when both are present |

Clear or qualified `cenc` progressive MP4 input can also produce clear progressive MP4 with one AAC-LC audio track (`mp4a`) and optionally one H.264 video track (`avc1`) from the same original file. Encrypted input requires every source track key by identifier and refuses DRM initialization headers (PSSH), key rotation, and other encryption schemes. This profile preserves a simple finite audio edit, including initial priming and final padding, and verifies every original track before publishing. It selects the layout automatically; no layout option or output key is required:

```sh
drmr convert original.mp4 --to clear --format mp4 --output clear.mp4
drmr verify clear.mp4 --source original.mp4 --json
```

For qualified `cenc` input, supply every track's identifier and key in the protected configuration's `input.keys` array. Clear output needs no output key:

```sh
drmr convert encrypted.mp4 --to clear --format mp4 --output clear.mp4 --keys-file source-keys.json
drmr verify clear.mp4 --source encrypted.mp4 --keys-file source-keys.json --json
```

The existing clear-input finite-edit regressions use the attributed BBB fixture and pass automated tests on Linux x86_64 and Windows x64 with FFmpeg 9.0.2, and macOS ARM64 with FFmpeg 9.0.1. Cases cover audio alone, joint video/audio, reordered pictures, exact presentation windows, and rejected changes to timing and display properties. Encrypted output, complex edits, additional tracks, and combining different MP4 files are outside this progressive profile. See [processing and verification](media-processing.md) for the exact restrictions and why this route keeps progressive sample tables.

The finite `cenc` input profile also passes automated conversion and independent verification on those three platforms and FFmpeg versions. Public fixtures cover both eight- and sixteen-byte sample vectors, distinct video/audio keys, complete packet and decoded-media equality, and finite presentation windows. Additional regressions preserve existing known-key CENC/CBCS verification with DRM initialization metadata. GPAC is used only for offline fixture generation, not by tests or conversion. These results do not qualify license services or device playback.

The backend also tests joint video/audio HLS with clear, `cbcs`, and AES-128 output. `cenc` HLS output is refused. HLS AES-128 applies to HLS resources, so it cannot be selected as a standalone MP4 or DASH protection scheme. `cens` and `cbc1` can be identified but are not conversion targets.

Local manifests must stay inside the source directory tree. Key URI mappings come from the protected operator configuration; no key URI or remote media is retrieved. Live input, discontinuities, byte ranges, multiple DASH periods, unsupported addressing, and rendition metadata that cannot be retained are rejected. Local MPEG transport stream input has a separate, narrower profile described below. See [processing and verification](media-processing.md) for the exact source profile, resource limits, audio trimming, segmentation, and verification behavior.

For example, to remove CBCS protection from a qualified local DASH source using keys in the input section of `source-keys.json`:

```sh
drmr convert protected/manifest.mpd --to clear --format dash --output clear-dash --keys-file source-keys.json
drmr verify clear-dash/manifest.mpd --source protected/manifest.mpd --keys-file source-keys.json --json
```

Clear output needs no output-key section. Keep the original media and keys until verification succeeds. License acquisition, DRM system qualification, and real device playback remain separate checks.

## MPEG transport stream input

Completed local HLS playlists containing clear, AES-128, or SAMPLE-AES transport stream segments can produce fragmented MP4 HLS with clear, `cbcs`, or AES-128 protection. Encrypted input requires operator-supplied keys. The initial profile accepts one H.264 video stream, optionally accompanied by one 48 kHz AAC-LC audio stream, in a single program. Video must be progressive. The TS output profile additionally accepts audio-only input with the strict native audio timing checks described in [processing and verification](media-processing.md#mpeg-transport-stream-input). 44.1 kHz audio, SAMPLE-AES-CTR transport streams, and DASH/MP4 output from this input are refused.

The ten-second public fixture passed all three output protections from clear MPEG-TS input, with separate verification on Linux x86_64 and Windows x64 with FFmpeg 9.0.2, and macOS ARM64 with FFmpeg 9.0.1; all used Shaka Packager 3.8.0. Separate AES-128 MPEG-TS input regressions verify clear HLS output with key rotation, explicit and sequence-derived initialization vectors, and transitions to clear segments. Additional cases cover reordered pictures and video-only input. These results qualify the tested profile, tools, and platforms. They do not establish other codecs or arbitrary broadcast streams. See [the exact input and verification limits](media-processing.md#mpeg-transport-stream-input).

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

The profile supports progressive H.264 video, 48 kHz AAC-LC audio, or both.
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

## Explicit PlayReady signaling

Generic `convert` supports an opt-in PlayReady profile: qualified clear MP4 with
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
tolerances. Finite progressive AAC end edits retain their existing refusals for
fragmented output. Run the real CLI regression with:

```sh
cargo test --locked -p drmr --test conversion_signaling -- --nocapture
```
