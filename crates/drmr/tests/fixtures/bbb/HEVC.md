# HEVC regression seeds

These two video-only adaptations use the same ten-second [Big Buck Bunny
excerpt and Creative Commons Attribution 3.0 license](README.md) as `clip.mp4`:

> (c) copyright 2008, Blender Foundation / www.bigbuckbunny.org

Keep that attribution and the [license link](https://creativecommons.org/licenses/by/3.0/)
when redistributing these seeds or their converted outputs. No customer or
private media is used.

| File | Coding profile | Bytes | SHA-256 |
| --- | --- | --- | --- |
| `hevc-main.mp4` | Main, 8-bit 4:2:0, `hvc1` | 94,189 | `bb6264c4a9a6aaa74b5afc00e157eea8129a3d38f4d2263b0226c091ccbfbf3a` |
| `hevc-main10.mp4` | Main 10, 10-bit 4:2:0, `hev1` | 101,630 | `5cd4bf734245767211030e735f70f0bbcd03c5e070b138d1a99fe76f2234d3fb` |

Both retain 240 pictures at 24 frames per second, 320×180 display dimensions,
reordered B pictures, and nominal two-second random-access intervals. The Main
seed uses closed groups of pictures, two slices per picture, and wavefront
parallel processing. The Main 10 seed uses open groups, clean random access
(CRA) pictures, leading pictures, and repeated in-band VPS/SPS/PPS parameter
sets. Both retain the ordinary x265 prefix supplemental enhancement information
(SEI) in their decoder configuration; Main 10 also carries it in samples.

The Main 10 seed converts the existing 8-bit source to 10-bit coding. It tests
that codec and encryption path; it is not a reference for native 10-bit source
quality, HDR, or a wider colour range. These seeds contain no audio. Existing
AAC fixtures and their finite-presentation tests remain separate.

`hevc-provenance.json` records the original clip hash, changes, exact encoding
arguments, tool versions, seed hashes, independently observed packet counts,
and NAL-unit types. The original clip and its provenance are unchanged.

## Independent fixture checks and offline rebuild

Verify the committed identities and packet/NAL structure using FFprobe:

```sh
python3 crates/drmr/tests/fixtures/bbb/hevc_fixture.py
```

Rebuild into a separate directory with FFmpeg including libx265:

```sh
python3 crates/drmr/tests/fixtures/bbb/hevc_fixture.py --rebuild /tmp/drmr-hevc-seeds
```

The generator first verifies the original clip hash. The recorded seeds used
FFmpeg 9.0.2 and x265 4.3+1-e9b8812 (build 217), with one pool worker and one
frame thread. Different encoder builds or CPU implementations may produce
different bytes; the script reports the observed identities rather than
replacing the committed provenance. libx265 is needed only for offline rebuild,
not for ordinary Rust tests or drmr processing.

Native HEVC regression tests use the committed seeds, generate small encrypted
adapters with public test keys, and compare complete sample hashes and clocks
against an independent FFprobe oracle. Explicit decode tests separately compare
all decoded pictures. Such evidence does not establish DRM license acquisition,
device playback, or support for every HEVC profile and extension.
