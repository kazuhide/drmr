# Conversion support

`drmr convert` processes local, completed on-demand media with keys supplied by its operator. It preserves encoded video/audio and verifies the result against the original source. The table describes tested media protection routes, separate from DRM license or device playback support.

## Protection routes

The attributed [ten-second Big Buck Bunny fixture](../crates/drmr/tests/fixtures/bbb/README.md) has H.264 video and stereo AAC audio. A clear fragmented DASH adapter supplies five two-second video segments; audio boundaries follow complete audio frames. Encrypted source presentations are generated from that adapter with public test keys. Source and result keys differ, including conversions that retain the same protection scheme.

| Input protection | Clear DASH | `cenc` DASH | `cbcs` DASH | HLS AES-128 |
| --- | --- | --- | --- | --- |
| Clear DASH | Pass | Pass | Pass | Pass |
| `cenc` DASH | Pass | Pass | Pass | Pass |
| `cbcs` DASH | Pass | Pass | Pass | Pass |
| HLS AES-128 | Pass | Pass | Pass | Pass |

These sixteen routes passed locally on macOS ARM64 with FFmpeg 9.0.2 and Shaka Packager 3.8.0. Each result passed both conversion's checks and a separate `drmr verify` invocation: encoded packet content, complete decoded media, and every packet's presentation/decode time and duration matched the source. They do not establish support for every codec, manifest profile, input produced by other tools, or playback device.

In this single local run, conversion took 1.28–2.58 seconds per route and the separate verification took 0.60–0.99 seconds. The CLI's sampled resident memory ranged from 11.8 to 15.1 MB; living descendants' sampled peaks ranged from 9.3 to 21.5 MB, measured separately every 100 milliseconds. These are sampled observations from a short fixture using a debug CLI, not exact memory maxima or throughput guarantees. A long or high-resolution presentation needs its own measurements.

Run the matrix with:

```sh
cargo test --locked -p drmr --test bbb_media the_real_cli_verifies_all_sixteen_bbb_protection_routes -- --nocapture
```

Set `DRMR_CONVERSION_REPORT` to a path outside the repository to save the JSON results. The report records the fixture hash, each actual route's checks, elapsed time, sampled memory and measurement limitations, platform, and tool versions. CI uploads its own results; use those artifacts for a particular revision and platform.

## Output formats and input limits

| Output format | Available protection | Initial profile |
| --- | --- | --- |
| MP4 file | Clear, `cenc`, `cbcs` | One audio or video track |
| DASH directory | Clear, `cenc`, `cbcs` | Separate fragmented MP4 tracks in one static presentation |
| HLS directory | Clear, `cbcs`, AES-128 | Local fragmented MP4 resources and completed playlists |

The backend also tests joint video/audio HLS with clear, `cbcs`, and AES-128 output. `cenc` HLS output is refused. HLS AES-128 applies to HLS resources, so it cannot be selected as a standalone MP4 or DASH protection scheme. `cens` and `cbc1` can be identified but are not conversion targets.

Local manifests must stay inside the source directory tree. Key URI mappings come from the protected operator configuration; no key URI or remote media is retrieved. Live input, discontinuities, byte ranges, MPEG transport streams, multiple DASH periods, unsupported addressing, and rendition metadata that cannot be retained are rejected. See [processing and verification](media-processing.md) for the exact source profile, resource limits, audio trimming, segmentation, and verification behavior.

For example, to remove CBCS protection from a qualified local DASH source using keys in the input section of `source-keys.json`:

```sh
drmr convert protected/manifest.mpd --to clear --format dash --output clear-dash --keys-file source-keys.json
drmr verify clear-dash/manifest.mpd --source protected/manifest.mpd --keys-file source-keys.json --json
```

Clear output needs no output-key section. Keep the original media and keys until verification succeeds. License acquisition, DRM system qualification, and real device playback remain separate checks.
