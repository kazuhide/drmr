# VP9 regression seeds

These video-only adaptations use the same ten-second [Big Buck Bunny excerpt
and Creative Commons Attribution 3.0 license](README.md) as `clip.mp4`:

> (c) copyright 2008, Blender Foundation / www.bigbuckbunny.org

Keep that attribution and the [license link](https://creativecommons.org/licenses/by/3.0/)
when redistributing the seeds or their converted outputs.

| File | Profile | Depth | Chroma | Bytes | SHA-256 |
| --- | --- | --- | --- | --- | --- |
| `vp9-p0-8.mp4` | 0 | 8 | 4:2:0 | 108,980 | `d9bb653ddf7b7011b4739973f5d6bc17009057d4dac46d68c84e0b006bdc0314` |
| `vp9-p1-8.mp4` | 1 | 8 | 4:4:4 | 107,004 | `cb49f95665a8e345a27e29d04c6ce0c16e5d61f75968981768c0720e1e7b3025` |
| `vp9-p2-10.mp4` | 2 | 10 | 4:2:0 | 106,046 | `7e58323555be2e6e04eaf630da55ef7edc7b81a06fd5d13e96514b27d1cba44d` |
| `vp9-p2-12.mp4` | 2 | 12 | 4:2:0 | 105,708 | `f021996104adc0d35370a8ad469a873d39b77bef4237aaad1038dac516128e21` |
| `vp9-p3-10.mp4` | 3 | 10 | 4:4:4 | 107,709 | `abfd16dbd14f30acfce852e0e8f59776e9c47287725ce4b2fdd5027ed65544e4` |
| `vp9-p3-12.mp4` | 3 | 12 | 4:4:4 | 106,946 | `96c93b12834b2997c17d39f5da73a89154d20c932bfaff322c2276b292457d02` |

Each seed contains 240 MP4 samples and 240 displayed pictures at 24 frames per
second and 320×180. Two-pass libvpx encoding uses ordinary hidden alternate
reference pictures (altrefs). These pictures help later prediction and share a
sample with a displayed picture through a superframe index. Profiles 0 and 2
have 19 such samples and 259 coded frames; profiles 1 and 3 have 18 and 258.
Key samples occur at 0, 2, 4, 6, and 8 seconds. Each sample displays one picture.

The higher bit depths and 4:4:4 chroma are conversions of the existing 8-bit
4:2:0 source. They exercise codec and encryption paths, not higher-depth source
quality, native 4:4:4 capture, HDR, or a wider colour range. Audio is omitted;
the existing AAC fixtures retain their separate presentation-window coverage.

`vp9-provenance.json` records the original clip hash, changes, argument templates
and profile substitutions, seed hashes, independent frame/index observations,
and complete decoded-picture hashes. The original AVC and HEVC seeds are
unchanged.

## Independent checks and offline rebuild

Verify the committed bytes, all six decoded streams, configuration records,
sample count, hidden frames, superframe indexes, and key-sample clocks:

```sh
python3 crates/drmr/tests/fixtures/bbb/vp9_fixture.py
```

Rebuild into a separate directory using FFmpeg with libvpx-vp9:

```sh
python3 crates/drmr/tests/fixtures/bbb/vp9_fixture.py --rebuild /tmp/drmr-vp9-seeds
```

The generator uses one encoder thread and removes its owned two-pass statistics.
The recorded build used FFmpeg 9.0.2; the installed libvpx package reported 1.17.0.
Other tool versions may produce different bytes. Rebuild output reports observed
hashes and does not replace the committed provenance. Ordinary tests use the
committed seeds, so libvpx encoding is not a runtime or CI requirement.

The [VP9 MP4 binding](https://www.webmproject.org/vp9/mp4/#common-encryption)
specifies `cenc` encryption with clear uncompressed headers and a clear superframe
index. CBCS is outside that binding. Generic fMP4 HLS structure, Apple authoring,
DRM declarations, license acquisition, and device playback are separate claims;
these fixtures do not qualify Apple/FairPlay playback.
