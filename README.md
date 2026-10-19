# drmr

Rust command-line tools for inspecting and repackaging streaming media with **digital rights management (DRM)**.

drmr inspects local MP4, DASH, and HLS media and converts qualified on-demand presentations between clear, `cenc` (counter-mode encryption), `cbcs` (pattern encryption using cipher block chaining), HLS AES-128 protection, and SAMPLE-AES transport-stream encryption. Input decryption keys and output encryption keys are supplied separately. It preserves the existing video and audio encoding; it does not transcode or change quality settings.

**DASH** (Dynamic Adaptive Streaming over HTTP) and **HLS** (HTTP Live Streaming) describe media segments using a manifest or playlists. A **rendition** is one available video quality or audio track.

The CLI is the primary interface. The local web console and optional [Desktop host](docs/desktop.md) are experimental.

[Project website (English)](https://kazuhide.github.io/drmr/) · [Japanese website](https://kazuhide.github.io/drmr/ja/)

[Web user guide (English)](https://kazuhide.github.io/drmr/guide/) · [Japanese web guide](https://kazuhide.github.io/drmr/ja/guide/) · [CLI usage](docs/usage.md) · [Conversion support](docs/conversion-support.md)

See [keys and local security](docs/security.md) for diagnostic masking, temporary files, external-tool process arguments, and browser playback boundaries.

## Install

To run the CLI with its media tools included, use the [Docker guide](docs/docker.md). It also covers the experimental local web console.

Building from source requires **Rust 1.95 or newer**. Use your selected Rust toolchain; CI checks the minimum version and the latest stable release. See [requirements and tested versions](docs/requirements.md). The commands below use a macOS/Linux shell; see the [Windows guide](docs/windows.md) for PowerShell, native paths, and protected configuration.

From the repository root:

```sh
cargo install --path crates/drmr --locked
drmr --version
```

To build without installing:

```sh
cargo build --release --locked -p drmr
./target/release/drmr --help
```

Media processing uses these external tools. Follow their installation instructions and add their executable directories to `PATH`, the list of directories your shell searches for programs.

| Executable | Supported from | Install | Purpose |
| --- | --- | --- | --- |
| `ffmpeg`, `ffprobe` | 9.0.1 | [FFmpeg](https://ffmpeg.org/download.html) | Process and inspect media |
| `mp4dump` | 1.6.0, build 641 | [Bento4](https://www.bento4.com/downloads/) | Inspect MP4 structure |
| `packager` | 3.8.0 | [Shaka Packager](https://github.com/shaka-project/shaka-packager#readme) | Encrypt and package media |

These are supported lower bounds. The [requirements table](docs/requirements.md#media-tools) records the builds used for qualification and explains which CI tools are pinned. Some older FFmpeg builds omit packet durations needed for encrypted audio verification; drmr refuses incomplete timing evidence. See [media processing](docs/media-processing.md) for details.

## Start here

```sh
drmr doctor
drmr inspect /path/to/media.mp4
drmr plan /path/to/manifest.mpd --to cbcs --format dash,hls
drmr plan --mpd /path/to/asset/download/manifest.example.test/sample/dash/manifest.mpd
drmr convert --help
drmr verify --help
```

`inspect` reads local MP4 metadata or DASH/HLS declarations without keys or external tools. `plan INPUT --to ...` describes conversion intent and separates input decryption keys from output encryption keys. Neither command verifies media samples or executes a conversion. `plan --mpd` keeps the original prepared-DASH rendition listing.

`doctor` discovers executable files without running them. Use `drmr doctor --probe --route progressive-mp4` to check versions and advertised options for a selected processing route. The report shows the actual resolved paths and distinguishes required tools from optional tools. These checks do not prove that a particular asset can be processed.

Use `convert INPUT --to ... --format ... --output ...` with a protected key configuration, then `verify OUTPUT --source INPUT` for independent comparison. See [conversion support](docs/conversion-support.md) for the tested protection routes, TS output options, and precise input profiles.

For a completed clear HLS URL, use `convert URL --download-dir ./source` with the target and output options below. The saved source can be reused offline. The existing `run` path processes prepared DASH assets; `analyze-hls` and `package-hls` retain the separate workflow for its supported DRM-signaling and subtitle profiles. See the [usage guide](docs/usage.md) for configuration examples, the expected local layout, key handling, and output.

## Supported input

The generic `convert` command accepts qualified local MP4, fragmented MP4 DASH/HLS, and MPEG transport stream HLS profiles, including encrypted input. Transport stream input can be clear, AES-128, or SAMPLE-AES with supplied keys and initialization vectors. See [conversion support](docs/conversion-support.md) for restrictions and tested routes. The older workflow commands have these separate input scopes:

- **Local DASH:** MP4 video with `avc1` (H.264) or `hvc1` (H.265), and MP4 audio with `mp4a` codec identifiers. The asset must use the supported downloaded-directory layout and per-representation segment templates. Other codecs and subtitle adaptations are excluded from this path.
- **Clear, on-demand HLS:** supported MPEG transport stream or fragmented MP4 media with H.264/H.265 video and AAC-LC audio, plus supported WebVTT or MP4-carried TTML subtitle profiles. Analysis rejects unsupported playlists and media rather than treating every HLS stream as compatible.
- Live HLS, already encrypted HLS input, discontinuities, and unsupported subtitle profiles are outside the clear-HLS input scope.

drmr packages supplied media and signaling. It does not provide a DRM license server.

## Commands

| Command | Purpose |
| --- | --- |
| `doctor` | Discover tools; add `--probe` for version and option checks, or `--json` for scripts |
| `inspect` | Read local MP4, DASH, or HLS metadata; use `--json` for scripts |
| `plan` | Describe a target conversion; `--mpd` lists prepared DASH renditions |
| `convert` | Convert qualified local media or acquire clear HLS URLs with `--download-dir`, then verify |
| `verify` | Compare result media directly with its original source |
| `run` | Repackage one local DASH asset and check its output |
| `analyze-hls` | Download and analyze a clear HLS presentation into a saved snapshot |
| `package-hls` | Package a verified HLS snapshot with DRM signaling |
| `verify-snapshot` | Detect changes to a saved snapshot; `--files` also hashes its files |
| `batch` | Process an asset work list with restart support; see the [Windows conditions](docs/windows.md) |
| `serve` | Start the experimental local web console |
| `canonical` | Print JSON with stable formatting and key order |
| `version` | Print the version |

Use `drmr help <COMMAND>` for command options. See [usage](docs/usage.md) for examples and exit codes.

See [media processing](docs/media-processing.md) for encryption terminology, segment boundaries, and the checks behind the reported results.

See [processing routes](docs/processing-routes.md) to understand inspection evidence, conditional plans, and the route reported by a successful conversion.

See [resource limits and measurements](docs/resources.md) for automatic worker selection, memory estimates, and reproducible comparisons.
