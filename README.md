# drmr

Rust command-line tools for inspecting and repackaging streaming media with **digital rights management (DRM)**.

drmr converts supported encrypted DASH content from `cenc` (counter-mode encryption) to `cbcs` (pattern encryption using cipher block chaining). It also analyzes clear, on-demand HLS content and packages it for Widevine, PlayReady, and FairPlay. It preserves the existing video and audio encoding; it does not transcode or change quality settings.

**DASH** (Dynamic Adaptive Streaming over HTTP) and **HLS** (HTTP Live Streaming) describe media segments using a manifest or playlists. A **rendition** is one available video quality or audio track.

The CLI is the primary interface. A local web console is experimental. A desktop application is not included in this release.

## Install

Use macOS or Linux with Rust 1.99.0. The repository pins this version in `rust-toolchain.toml`.

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

| Executable | Install | Purpose |
| --- | --- | --- |
| `ffmpeg`, `ffprobe` | [FFmpeg](https://ffmpeg.org/download.html) | Process and inspect media |
| `mp4dump` | [Bento4](https://www.bento4.com/downloads/) | Inspect MP4 structure |
| `packager` | [Shaka Packager](https://github.com/shaka-project/shaka-packager#readme) | Encrypt and package media |

## Start here

```sh
drmr doctor
drmr inspect /path/to/media.mp4
drmr plan /path/to/manifest.mpd --to cbcs --format dash,hls
drmr plan --mpd /path/to/asset/download/manifest.example.test/sample/dash/manifest.mpd
drmr run --help
```

`inspect` reads local MP4 metadata or DASH/HLS declarations without keys or external tools. `plan INPUT --to ...` describes conversion intent and separates input decryption keys from output encryption keys. Neither command verifies media samples or executes a conversion. `plan --mpd` keeps the original prepared-DASH rendition listing.

`doctor` checks whether the four executable files can be found. It does not run them, validate their versions, or prove that a particular asset can be processed.

For clear HLS, start with `analyze-hls`, then package the saved snapshot with `package-hls`. See the [usage guide](docs/usage.md) for configuration examples, the expected local layout, key handling, and output.

## Supported input

- **Local DASH:** MP4 video with `avc1` (H.264) or `hvc1` (H.265), and MP4 audio with `mp4a` codec identifiers. The asset must use the supported downloaded-directory layout and per-representation segment templates. Other codecs and subtitle adaptations are excluded from this path.
- **Clear, on-demand HLS:** supported MPEG transport stream or fragmented MP4 media with H.264/H.265 video and AAC-LC audio, plus supported WebVTT or MP4-carried TTML subtitle profiles. Analysis rejects unsupported playlists and media rather than treating every HLS stream as compatible.
- Live HLS, already encrypted HLS input, discontinuities, and unsupported subtitle profiles are outside the clear-HLS input scope.

drmr packages supplied media and signaling. It does not provide a DRM license server.

## Commands

| Command | Purpose |
| --- | --- |
| `doctor` | Check external tool availability; use `--json` for scripts |
| `inspect` | Read local MP4, DASH, or HLS metadata; use `--json` for scripts |
| `plan` | Describe a target conversion; `--mpd` lists prepared DASH renditions |
| `run` | Repackage one local DASH asset and check its output |
| `analyze-hls` | Download and analyze a clear HLS presentation into a saved snapshot |
| `package-hls` | Package a verified HLS snapshot with DRM signaling |
| `verify-snapshot` | Detect changes to a saved snapshot; `--files` also hashes its files |
| `batch` | Process an asset work list with restart support on macOS/Linux |
| `serve` | Start the experimental local web console |
| `canonical` | Print JSON with stable formatting and key order |
| `version` | Print the version |

Use `drmr help <COMMAND>` for command options. See [usage](docs/usage.md) for examples and exit codes.

See [media processing](docs/media-processing.md) for encryption terminology, segment boundaries, and the checks behind the reported results.
