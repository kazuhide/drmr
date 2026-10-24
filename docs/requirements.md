# Requirements and tested versions

## Building from source

Use **Rust 1.95 or newer**. The minimum supported Rust version is declared by `rust-version` in the workspace `Cargo.toml`. The repository respects the toolchain you selected and does not automatically switch it to a fixed release. A downloaded CLI archive does not need Rust installed.

CI derives the minimum version from that manifest and tests it alongside the latest stable Rust release on Linux, macOS, and Windows. Both run the native tests and real-media checks. Logs record the actual compiler and Cargo versions; downloadable CLI archives use the stable build. Formatting and warning checks apply to both versions.

## Media tools

Native complete-media checks in `analyze`, `plan --save` and `verify` run in
Rust for supported profiles and need none of the tools below. They read all
selected samples, codec configuration, clocks and playback windows.
`analyze --metadata-only` reads only metadata and remains a separate, bounded
operation. Conversion routes and additional `--decode` checks use the tools
below; add their executable directories to `PATH`.

| Tool | Supported from | Builds used for the media qualification |
| --- | --- | --- |
| FFmpeg and ffprobe | 9.0.1 | macOS: 9.0.1; Linux and Windows: 9.0.2 |
| Shaka Packager | 3.8.0 | 3.8.0 on all three platforms |

Shaka Packager performs packaging and encryption on its supported conversion
routes. Clear finite progressive MP4 can use a native copy; the edit-preserving
encrypted finite progressive route retains an FFmpeg copy remux without
transcoding. `--decode` requires FFmpeg and ffprobe for the additional complete
decoder checks. See [verification depth](verification.md).

The lower bounds describe the supported baseline, rather than the earliest version that might process a particular file. CI pins and checksum-verifies direct media-tool downloads. On macOS, FFmpeg comes from Homebrew and can change when its formula updates. The table records qualification builds; each platform report records the version actually used by that run. Newer builds must still provide the required options and complete packet timing; an increased version number does not replace the media checks.

`doctor` profiles cover conversion and additional complete decoding, including tools not needed for native-only analysis or verification. They do not report the native minimum from a metadata-only route candidate. Discovery finds executables without launching tools. Add `--probe` to check their version banners and advertised options, and `--route progressive-mp4` (or another processing route) to select required tools. A newer version can still lack an option. MP4 structure checks run in the Rust executable. During conversion, incomplete timing or unsupported metadata causes an error. See [media processing](media-processing.md) and [conversion support](conversion-support.md) for the verified profiles and known tool behavior.

Decoded-content verification also requires FFmpeg's `data` and `hash` output
formats, its `rawvideo` and `pcm_s16le` encoders, and the `thread_queue_size`
option. These preserve every decoded
picture and 16-bit audio sample used by the complete SHA-256 comparison;
`doctor --probe` checks that the build advertises each component. The probe
does not decode a sample or establish compatibility with a particular input.

Windows also needs the [Visual C++ runtime](windows.md#install-and-check-tools) used by its compiled executable. External media tools are installed separately from the CLI archive.

## Final-manifest checks

Native HLS/DASH manifest linting is included in the Rust executable and needs no
additional tools. Native sample/content/timing checks and additional complete decoding
retain separate evidence. External checks
of final HLS playlists and DASH manifests are an additional, explicit choice.
Use the [manifest validation guide](manifest-validation.md) for installation,
supported tool versions, local DASH-IF assets, and runtime executable options.
Apple's HLS validator runs on macOS. The DASH adapter uses PHP, Java and separately
installed DASH-IF schema and rule assets. Missing tools cause failure when a
saved output policy requires these checks. They are not silently replaced by
native manifest linting or the internal media comparison.

## Rust library dependencies

`Cargo.toml` gives each direct dependency a supported baseline and Cargo's compatible update range. For example, `1.2.3` admits compatible releases before `2.0.0`; before version `1.0`, those compatibility ranges are narrower. The baseline versions are the ones recorded in the qualified dependency set.

`Cargo.lock` records the exact dependency set used to build and test the CLI. Build with `--locked` to reproduce it. Updating that file selects newer versions within the declared ranges; minimum-Rust and stable CI must pass before such an update is merged.

The bundled process-supervision dependency retains an exact requirement while it carries the termination fixes used by the Windows lifecycle checks. A replacement must pass those checks before that exception is removed. See Cargo's [version requirement rules](https://doc.rust-lang.org/cargo/reference/specifying-dependencies.html) and [minimum Rust declaration](https://doc.rust-lang.org/cargo/reference/rust-version.html).
