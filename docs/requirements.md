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
routes. Qualified `--processing segments` conversion uses Rust for TS-to-TS
and fMP4-to-fMP4 processing, including HLS/DASH manifest generation, and needs
none of those tools at native depth. Qualified finite progressive MP4 uses a
native copy, including decryption of `cenc` or `cbcs` input. Its sample locations,
clocks and playback edits remain unchanged. FFmpeg and ffprobe are used only
when complete decoding is required. `--decode` requires both for the additional
decoder checks. TS segment conversion with `--decode` also needs Shaka Packager
for preparing the decoder inputs. See [verification depth](verification.md).

The lower bounds describe the supported baseline, rather than the earliest version that might process a particular file. CI pins and checksum-verifies direct media-tool downloads. On macOS, FFmpeg comes from Homebrew and can change when its formula updates. The table records qualification builds; each platform report records the version actually used by that run. Newer builds must still provide the required options and complete packet timing; an increased version number does not replace the media checks.

`doctor` checks native conversion requirements by default. Select a processing
route with `--route`; `progressive-mp4` and native segment routes require no
external media tools, while packaging routes require Shaka Packager. The default
`all` profile requires the tools used by any native conversion route. Add
`--decode` to include complete-decoding requirements. Discovery finds executables
without launching tools. Add `--probe` to check required tools' version banners
and advertised options; optional tools are discovered but not launched. A newer
version can still lack an option. These checks do not process media. MP4
structure checks run in the Rust executable. See
[media processing](media-processing.md) and [conversion support](conversion-support.md)
for the verified profiles and known tool behavior.

Decoded-content verification also requires FFmpeg's `data` and `hash` output
formats, its `rawvideo` and `pcm_s16le` encoders, and the `thread_queue_size`
option. These preserve every decoded
picture and 16-bit audio sample used by the complete SHA-256 comparison;
`doctor --probe --decode` checks that the build advertises each component. The probe
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
