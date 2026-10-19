# Progressive Common Encryption test fixtures

`cenc-iv8.mp4` and `cenc-iv16.mp4` encrypt the complete `clip.mp4` presentation with the counter-mode Common Encryption scheme (`cenc`). They retain 240 H.264 video frames, 470 AAC packets, and the finite audio presentation of 480,000 samples per channel. These media adaptations have the same [Big Buck Bunny attribution and Creative Commons Attribution 3.0 license](README.md) as the clear clip.

| File | SHA-256 |
| --- | --- |
| `cenc-iv8.mp4` | `566f39a597b72df0cf7aeaec1fc3f71b1fe64c2ec20b2235996b784be9d8f945` |
| `cenc-iv16.mp4` | `41165be29c041a61f7d95c7384e225d4f0ac491dfb729358332ef79478f5ba26` |

The XML files contain intentionally public test keys, key identifiers, and fixed initialization vectors. Never use these values or fixed vectors to protect other media. The two tracks have different keys and identifiers; neither key equals either identifier. There is no Protection System Specific Header (PSSH), which carries DRM initialization data. These fixtures do not qualify a license service or device playback.

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

## Audio-only CBCS fixture

`cbcs-audio.mp4` is an 83,721-byte, audio-only adaptation of `clip.mp4`, with SHA-256 `0868265d3c884bbf87a60cea8bb3de12df387393ffb33afa1652a089a95cea1d`. It retains all 470 AAC packets, their absolute clocks and payloads after decryption, and the finite presentation of 480,000 decoded samples per channel. The same attribution and Creative Commons Attribution 3.0 license apply.

This file exercises independent known-key verification of an existing progressive CBCS presentation. The `cbcs-audio.xml` key, identifier, and constant initialization vector are public test values only. The identifier differs from every public test key.

The same official GPAC source build recorded above generated the encrypted bytes without manual media or cryptographic editing. FFmpeg 9.0.2 first copied the original audio sample tables; MP4Box then encrypted that complete presentation. From this directory:

```sh
ffmpeg -v error -nostdin -threads 1 -copyts -i clip.mp4 \
  -map 0:a:0 -c copy -avoid_negative_ts disabled -use_editlist 1 audio-clear.mp4
MP4Box -p=0 -no-save -crypt cbcs-audio.xml audio-clear.mp4 -out rebuilt-cbcs-audio.mp4
```

Independent local FFmpeg checks decrypted the result with its known key and matched every packet payload and absolute clock, and all 480,000 decoded samples per channel, against the clear audio. No GPAC binary is required to run the committed regression. This is media-identity evidence, not a DRM license or device-playback qualification. The XML syntax and cipher selection follow the [official GPAC encryption documentation](https://wiki.gpac.io/xmlformats/Common-Encryption/).

## Joint video and audio CBCS fixture

`cbcs-joint.mp4` encrypts `clip.mp4` directly, retaining both tracks and the original audio presentation window. It is 195,719 bytes with SHA-256 `89102552e8aae65cc290594b8d72357ffca17f1ea5cc2e1e4e450b2405fa4870`. The same Big Buck Bunny attribution and Creative Commons Attribution 3.0 license apply.

`cbcs-joint.xml` assigns different public test keys and identifiers to video and audio. Video encrypts one block and skips nine blocks, with subsample encryption records and a 16-byte constant initialization vector. Audio encrypts complete samples with a different 16-byte constant vector. These fixed values are for reproducible tests only.

The same official GPAC release generated this fixture without an intermediate remux or manual box editing. The local static build also enabled ISO media fragments; optional libraries and network support remained disabled. From this fixture directory:

```sh
MP4Box -p=0 -no-save -crypt cbcs-joint.xml clip.mp4 -out rebuilt-cbcs-joint.mp4
```

Independent FFmpeg 9.0.2 checks with both role keys matched every video and audio packet's decrypted payload, absolute presentation and decode timestamps, and duration. All 240 decoded video frames, 480,000 presented audio samples per channel, and 481,280 audio samples per channel before applying the presentation window matched the clear source. Interchanging the two role keys changed the payloads of both tracks and failed the comparison. File hashes were unchanged by verification. These checks establish media correspondence; they do not qualify a license service, device playback, or independent format conformance.
