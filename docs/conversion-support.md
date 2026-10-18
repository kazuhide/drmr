# Conversion support

`drmr convert` processes local, completed on-demand media with keys supplied by its operator. It preserves encoded video/audio and verifies the result against the original source. The tables distinguish supported input profiles from tested protection routes. DRM license and device playback support are separate.

## HLS inputs and containers

Clear HLS input is supported. HLS describes playlists; their media can use either
fragmented MP4 (fMP4) or MPEG transport stream (MPEG-TS) segments. Clear means
unencrypted media. The two containers have different conversion limits:

| HLS input | Input container | DASH output protection | HLS output protection |
| --- | --- | --- | --- |
| Clear HLS | fMP4 | Clear, `cenc`, `cbcs` | Clear, `cbcs`, AES-128 |
| `cbcs` HLS | fMP4 | Clear, `cenc`, `cbcs` | Clear, `cbcs`, AES-128 |
| AES-128 HLS | fMP4 | Clear, `cenc`, `cbcs` | Clear, `cbcs`, AES-128 |
| Clear HLS | MPEG-TS | Not supported | Clear, `cbcs`, AES-128 |
| AES-128 HLS | MPEG-TS | Not supported | Clear, `cbcs`, AES-128 |

**Every HLS output uses fMP4.** MPEG-TS is supported only as input; drmr does not
produce MPEG-TS HLS.

These are supported profiles for local, completed playlists, subject to the
[media and verification restrictions](media-processing.md). Each fMP4 media
playlist must contain one audio or video track; a master playlist can reference
separate video and audio playlists. Rendition names, language and selection metadata
that cannot be preserved are rejected rather than discarded. A single-track fMP4 HLS source can also
produce clear, `cenc`, or `cbcs` MP4. Combining an HLS presentation's separate
audio and video tracks into one MP4 is outside the current profile. MPEG-TS has
the [narrower input profile below](#mpeg-transport-stream-input).

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
| HLS directory | Clear, `cbcs`, AES-128 | fMP4 resources and completed playlists; MPEG-TS output is not supported |

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

Completed local HLS playlists containing clear or operator-keyed AES-128 transport stream segments can produce fragmented MP4 HLS with clear, `cbcs`, or AES-128 protection. The initial profile accepts one H.264 video stream, optionally accompanied by one 48 kHz AAC-LC audio stream, in a single program. Video must be progressive. Audio-only input, 44.1 kHz audio, sample-encrypted transport streams, DASH/MP4 output from this input, and transport stream output are refused.

The ten-second public fixture passed all three output protections from clear MPEG-TS input, with separate verification on Linux x86_64 and Windows x64 with FFmpeg 9.0.2, and macOS ARM64 with FFmpeg 9.0.1; all used Shaka Packager 3.8.0. Separate AES-128 MPEG-TS input regressions verify clear HLS output with key rotation, explicit and sequence-derived initialization vectors, and transitions to clear segments. Additional cases cover reordered pictures and video-only input. These results qualify the tested profile, tools, and platforms. They do not establish other codecs or arbitrary broadcast streams. See [the exact input and verification limits](media-processing.md#mpeg-transport-stream-input).
