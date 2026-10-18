# Progressive CENC test fixtures

`cenc-iv8.mp4` and `cenc-iv16.mp4` encrypt the complete `clip.mp4` presentation with Common Encryption in counter mode (CENC). They retain 240 H.264 video frames, 470 AAC packets, and the finite audio presentation of 480,000 samples per channel. These media adaptations have the same [Big Buck Bunny attribution and Creative Commons Attribution 3.0 license](README.md) as the clear clip.

| File | SHA-256 |
| --- | --- |
| `cenc-iv8.mp4` | `566f39a597b72df0cf7aeaec1fc3f71b1fe64c2ec20b2235996b784be9d8f945` |
| `cenc-iv16.mp4` | `41165be29c041a61f7d95c7384e225d4f0ac491dfb729358332ef79478f5ba26` |

The XML files contain intentionally public test keys, key identifiers, and fixed initialization vectors. Never use these values or fixed vectors to protect other media. The two tracks have different keys and identifiers; neither key equals either identifier. There is no protection-system initialization data (PSSH), license service, or device playback qualification.

## Offline generation

GPAC MP4Box generated these files directly from the committed clear clip, without an intermediate remux or manual box editing. GPAC is needed only to regenerate fixtures; it is not a build, test, or runtime requirement.

The source was the official GPAC `v26.07.0` release, commit `a07cbfff238a331233e11e916f9fb185d5da8604`. The [source archive](https://codeload.github.com/gpac/gpac/tar.gz/refs/tags/v26.07.0) has SHA-256 `57822c1a74dcb83d76ff1f671e1b4fae2e7614e8194a5adb9f20661e0e9421dd`. A local arm64 macOS static MINI build reported `26.07-revrelease`. It enabled the ISO media reader/writer, parsers, encryption, decryption, file input/output, and documentation features; optional system libraries and network support were disabled. No GPAC code or binary is redistributed here.

From this fixture directory, with MP4Box available:

```sh
MP4Box -p=0 -no-save -crypt cenc-iv8.xml clip.mp4 -out rebuilt-iv8.mp4
MP4Box -p=0 -no-save -crypt cenc-iv16.xml clip.mp4 -out rebuilt-iv16.mp4
```

The recorded files have auxiliary sample encryption data in each track, with sample auxiliary sizes and absolute offsets in its sample table. Both vector widths are covered. Video uses subsample encryption; audio encrypts each complete sample. The ordinary AAC roll recovery description is retained. Regeneration may change nonessential metadata or random values in other tool versions; verify the hashes and every media invariant before replacing fixtures.

Independent local FFmpeg 9.0.2 checks used the complete two-key dictionary. Every per-track packet payload, absolute presentation/decode timestamp, duration, finite presentation window, decoded video frame, and decoded audio sample matched the original clip. Missing, wrong, and swapped key checks failed for the affected tracks. These observations establish fixture correspondence; they do not establish DRM playback or independent format conformance.
