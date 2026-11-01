# Big Buck Bunny regression fixture

`clip.mp4` is a 195,472-byte excerpt from **Big Buck Bunny**:

> (c) copyright 2008, Blender Foundation / www.bigbuckbunny.org

The movie and this adapted excerpt are licensed under [Creative Commons Attribution 3.0](https://creativecommons.org/licenses/by/3.0/), as stated on the [official project page](https://peach.blender.org/about/). This media license is separate from the repository's code license. Keep this attribution and the license link when redistributing the excerpt or outputs made from it.

The excerpt covers 00:30–00:40 of the official 320×180 movie. It was re-encoded as H.264 video at 24 frames per second and stereo AAC-LC audio at 48 kHz. Video has an instantaneous decoding refresh (IDR) frame every two seconds: these frames let each of the five video segments start independently. Audio segment boundaries may differ slightly because an audio frame has a fixed number of samples.

`provenance.json` records the official source URL, archive and movie hashes, clip hash, attribution, changes, and encoding arguments. The committed clip's SHA-256 is:

```text
a0ef6d20f2319c57af35d9c3165e46b669c67b951268cf882dbfff14b5b26e71
```

## Run the regression

Install FFmpeg (including `ffprobe`), Shaka Packager, and Python 3. With the repository's Rust toolchain installed, run:

```sh
cargo test --locked -p drmr --test bbb_media -- --nocapture
```

The fixture test checks the committed bytes, codecs, duration, packet count, and actual H.264 IDR slices. The saved-plan test copies the video and audio into five two-second clear MPEG-TS segments for HTTP Live Streaming (HLS). It runs the real `analyze --metadata-only`, `analyze --decode`, and `plan --save --decode` commands against that local presentation, then saves a plan with two independent outputs:

- Dynamic Adaptive Streaming over HTTP (DASH), using fragmented MP4 with CBCS encryption and Widevine/PlayReady signaling.
- HLS, using fragmented MP4 with CBCS encryption and FairPlay signaling.

`convert --plan --decode` must verify every coded packet, packet timestamp, decoded frame and audio sample for both outputs, together with native manifest checks and DRM signaling. A separate `verify --plan --decode` repeats the comparison and checks that both outputs match the saved plan. The test also checks that the source bytes and saved plan remain unchanged.

This test uses the MPEG-TS adapter as its comparison baseline. MPEG-TS does not carry the progressive MP4 edit list that trims AAC encoder priming and final padding. It therefore checks preservation of the complete TS presentation, rather than claiming to preserve the progressive clip's finite audio trim. Separate [progressive MP4 regressions](../../progressive_mp4.rs) check that behavior; see [media processing](../../../../../docs/media-processing.md) for the supported edit profiles and their limits.

The printed JSON records verification results, tool versions, elapsed time, and resident memory sampled every 100 milliseconds separately for the CLI and its living child processes. Sampled peaks can miss short-lived processes or spikes; they are not exact lifetime maxima. If no sample is available, memory is explicitly reported as `unmeasured`. Save a report by setting `DRMR_MEDIA_REPORT` to a file outside the repository.

The encryption keys and identifiers in `regression.py` are intentionally public test values. Generated encrypted outputs can be decrypted with those values; do not use them to restrict access or for production. This test does not verify a real license service, playback device, or external conformance tool. CI runs the regression natively on Linux, macOS, and Windows; each report identifies the platform that actually ran it.

The complete-media regressions request `--decode` explicitly, preserving their existing native and complete-decoding requirements. New reports use schema 2 with typed decode evidence, and the named-output plan uses schema 4 with a decoded minimum. Native source admission and packaging-only comparisons have separate Rust integration tests; recognizing a codec configuration does not qualify every profile or bit depth. See [verification depth](../../../../../docs/verification.md).

## Generic conversion matrix

`conversion_regression.py` creates a separate local static DASH adapter without optional rendition labels, then creates `cenc`, `cbcs`, and HLS AES-128 sources with public fixture keys. The real `convert` and `verify` commands exercise all sixteen input/output protection pairs with separate source and result keys. MP4 sample encryption targets are written to DASH; HLS AES-128 is written to HLS. Each source and result retains both video and audio.

The source baseline is the fragmented adapter. Its decoder presents final audio padding; this test does not assert that converting the original progressive clip preserves finite audio end trimming. Finite audio end trimming remains unsupported for the fragmented and encrypted outputs in this matrix. A separate clear progressive MP4 route preserves qualified finite edits and verifies the whole original presentation, including exact video display dimensions; see [media processing](../../../../../docs/media-processing.md). Its real CLI regressions are in `progressive_mp4.rs`, outside this sixteen-route matrix.

Set `DRMR_CONVERSION_REPORT` to save this matrix's measured JSON outside the repository. CI uploads the report alongside the saved-plan report. See [conversion support](../../../../../docs/conversion-support.md) for observations and the exact commands.

## Rebuild the clear clip

Download the original archive from the official Blender server:

```sh
curl --fail --location --output /tmp/BigBuckBunny_320x180.mp4.zip \
  https://download.blender.org/peach/bigbuckbunny_movies/BigBuckBunny_320x180.mp4.zip
python3 crates/drmr/tests/fixtures/bbb/regression.py \
  --rebuild-from-archive /tmp/BigBuckBunny_320x180.mp4.zip \
  --clip-output /tmp/bbb-clip.mp4
```

The command verifies the archive and movie SHA-256 values before reading the exact movie member and running the recorded FFmpeg arguments. It prints the resulting hash and whether the bytes match the committed clip. FFmpeg 9.0.2 produced the committed bytes; different encoder builds can produce a different hash with the same settings. Replacing the fixture requires updating its recorded hash and rerunning the regression.

To check the committed clip without packaging:

```sh
python3 crates/drmr/tests/fixtures/bbb/regression.py --verify-fixture
```

The finite progressive two-key CENC fixtures and their offline generation instructions are documented in [CENC.md](CENC.md). Their public keys are test data only.

The separate video-only HEVC Main and Main 10 seeds, including open-GOP, in-band
parameter-set and SEI coverage, are documented in [HEVC.md](HEVC.md).
