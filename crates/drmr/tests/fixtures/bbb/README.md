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

Install FFmpeg (including `ffprobe`), Shaka Packager, Bento4's `mp4dump`, and Python 3. With the repository's Rust toolchain installed, run:

```sh
cargo test --locked -p drmr --test bbb_media -- --nocapture
```

The fixture test checks the committed bytes, codecs, duration, packet count, and actual H.264 IDR slices. The existing HLS workflow test generates clear HTTP Live Streaming (HLS) and Dynamic Adaptive Streaming over HTTP (DASH) adapters in a temporary directory. It runs the real `drmr analyze-hls` and `drmr package-hls` commands against the HLS adapter. Packaging must pass the CLI's source correspondence, timing, decoded media identity, and CBCS encryption signaling checks for both tracks. The generated DASH adapter is checked for clear signaling; it is not a DASH packaging test.

An independent check then concatenates each complete source presentation and its final encrypted output. FFmpeg decrypts the output directly: all decoded video frames and audio samples must match, and every packet timestamp must retain the intended two-second transport shift. This catches an audio trim being lost or incorrectly applied at every segment boundary.

The fixture contains AAC encoder priming: leading samples needed by the decoder but excluded from presentation using an MP4 edit list. The HLS packaging path preserves a single open-ended trim at normal playback rate while compensating decode timestamps. This HLS packaging path rejects finite edits, multiple presentation intervals, nonintegral timescale conversion, absolute media offsets, and timestamp overflow as unsupported. Matching encrypted payloads alone does not prove that presentation trimming was preserved. Optional internal random-access indexes (`mfra`, `sidx`, and dependent `ssix`) are omitted when restoring a trim so they cannot retain stale byte offsets or timestamps; the manifests still locate the media fragments.

The printed JSON records source/output file hashes, matching decoded-content hashes and frame/sample counts, tool versions, elapsed time, and resident memory sampled every 100 milliseconds separately for the CLI and its living child processes. Sampled peaks can miss short-lived processes or spikes; they are not exact lifetime maxima. If no sample is available, memory is explicitly reported as `unmeasured`. Save a report by setting `DRMR_MEDIA_REPORT` to a file outside the repository. Reports contain platform and tool versions, but no source or output paths.

One local run with FFmpeg 9.0.2 and Shaka Packager 3.8.0 produced the following identical source/output decoded hashes. These are observations, not a requirement that different decoder builds produce identical bytes. The audio count includes the source decoder's final frame padding; the test preserves the source rather than silently trimming it.

| Track | Decoded amount | Source and output SHA-256 |
| --- | --- | --- |
| Video | 240 frames | `2d853e654596374cd44cb5338c84ea4ef14e10a3739f08509bcfc8474bcf5271` |
| Audio | 480,256 samples per channel | `c988781b14c3d73a9d22b3ec4767f0a14c50e8e84600850b99a5aee55a0f6e26` |

The video hash covers the frame-size/hash pairs emitted by FFmpeg; the audio hash covers signed 32-bit little-endian PCM samples. The JSON also records the actual assembled MP4 hashes, which may vary with generated metadata.

The encryption key and identifiers in `regression.py` are intentionally public test values. Generated encrypted outputs can be decrypted with those values; do not use them to restrict access or for production. This test does not verify a real license service, playback device, Windows behavior, or independent format conformance.

## Generic conversion matrix

`conversion_regression.py` creates a separate local static DASH adapter without optional rendition labels, then creates `cenc`, `cbcs`, and HLS AES-128 sources with public fixture keys. The real `convert` and `verify` commands exercise all sixteen input/output protection pairs with separate source and result keys. MP4 sample encryption targets are written to DASH; HLS AES-128 is written to HLS. Each source and result retains both video and audio.

The source baseline is the fragmented adapter. Its decoder presents the final audio padding already recorded above; this test does not assert that converting the original progressive clip preserves finite audio end trimming. Finite audio end trimming remains unsupported for the fragmented and encrypted outputs in this matrix. A separate clear progressive MP4 route preserves qualified finite edits and verifies the whole original presentation, including exact video display dimensions; see [media processing](../../../../../docs/media-processing.md). Its real CLI regressions are in `progressive_mp4.rs`, outside this sixteen-route matrix.

Set `DRMR_CONVERSION_REPORT` to save this matrix's measured JSON outside the repository. CI uploads the report alongside the existing HLS workflow report. See [conversion support](../../../../../docs/conversion-support.md) for observations and the exact commands.

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
