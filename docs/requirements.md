# Requirements and tested versions

## Building from source

Use **Rust 1.95 or newer**. The minimum supported Rust version is declared by `rust-version` in the workspace `Cargo.toml`. The repository respects the toolchain you selected and does not automatically switch it to a fixed release. A downloaded CLI archive does not need Rust installed.

CI derives the minimum version from that manifest and tests it alongside the latest stable Rust release on Linux, macOS, and Windows. Both run the native tests and real-media checks. Logs record the actual compiler and Cargo versions; downloadable CLI archives use the stable build. Formatting and warning checks apply to both versions.

## Media tools

Add the executable directories to `PATH`. Use each tool at the supported lower bound or a newer compatible version:

| Tool | Supported from | Builds used for the media qualification |
| --- | --- | --- |
| FFmpeg and ffprobe | 9.0.1 | macOS: 9.0.1; Linux and Windows: 9.0.2 |
| Shaka Packager | 3.8.0 | 3.8.0 on all three platforms |
| Bento4 SDK | 1.6.0, build 641 | 1.6.0, build 641 on all three platforms |

The lower bounds describe the supported baseline, rather than the earliest version that might process a particular file. CI pins and checksum-verifies direct media-tool downloads. On macOS, FFmpeg and Bento4 come from Homebrew and can change when its formulas update. The table records qualification builds; each platform report records the version actually used by that run. Newer builds must still provide the required options and complete packet timing; an increased version number does not replace the media checks.

`doctor` discovers executables without launching tools. Add `--probe` to check their version banners and advertised options, and `--route progressive-mp4` (or another processing route) to select required tools. A newer version can still lack an option. Bento4 does not expose its build number in its banner; the report marks that check as `build-unverified`, so confirm build 641 or newer with its distributor. During conversion, incomplete timing or unsupported metadata causes an error. See [media processing](media-processing.md) and [conversion support](conversion-support.md) for the verified profiles and known tool behavior.

Windows also needs the [Visual C++ runtime](windows.md#install-and-check-tools) used by its compiled executable. External media tools are installed separately from the CLI archive.

## Rust library dependencies

`Cargo.toml` gives each direct dependency a supported baseline and Cargo's compatible update range. For example, `1.2.3` admits compatible releases before `2.0.0`; before version `1.0`, those compatibility ranges are narrower. The baseline versions are the ones recorded in the qualified dependency set.

`Cargo.lock` records the exact dependency set used to build and test the CLI. Build with `--locked` to reproduce it. Updating that file selects newer versions within the declared ranges; minimum-Rust and stable CI must pass before such an update is merged.

The bundled process-supervision dependency retains an exact requirement while it carries the termination fixes used by the Windows lifecycle checks. A replacement must pass those checks before that exception is removed. See Cargo's [version requirement rules](https://doc.rust-lang.org/cargo/reference/specifying-dependencies.html) and [minimum Rust declaration](https://doc.rust-lang.org/cargo/reference/rust-version.html).
