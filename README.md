# drmr

Rust command-line tools for inspecting and repackaging streaming media with **digital rights management (DRM)**.

drmr inspects local MP4, DASH, and HLS media and converts qualified on-demand presentations between clear, `cenc` (counter-mode encryption), `cbcs` (pattern encryption using cipher block chaining), HLS AES-128 protection, and SAMPLE-AES transport-stream encryption. Input decryption keys and output encryption keys are supplied separately. It preserves the existing video and audio encoding; it does not transcode or change quality settings.

**DASH** (Dynamic Adaptive Streaming over HTTP) and **HLS** (HTTP Live Streaming) describe media segments using a manifest or playlists. A **rendition** is one available video quality or audio track.

The CLI is the primary interface. The local web console and optional [Desktop host](docs/desktop.md) are experimental.

[Project website (English)](https://kazuhide.github.io/drmr/) · [Japanese website](https://kazuhide.github.io/drmr/ja/)

[Web user guide (English)](https://kazuhide.github.io/drmr/guide/) · [Japanese web guide](https://kazuhide.github.io/drmr/ja/guide/) · [CLI usage](docs/usage.md) · [Conversion support](docs/conversion-support.md)

[Releases and compatibility](docs/releases.md) · [Changelog](CHANGELOG.md)

See [keys and local security](docs/security.md) for diagnostic masking, temporary files, external-tool process arguments, and the local interface security boundary.

## Install

Download a CLI archive for your operating system and processor from
[GitHub Releases](https://github.com/kazuhide/drmr/releases), then follow the
[archive and version checks](docs/releases.md#install-a-released-cli). Rust is
only needed when building from source; external media tools are installed
separately for either CLI installation method.

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
| `packager` | 3.8.0 | [Shaka Packager](https://github.com/shaka-project/shaka-packager#readme) | Encrypt and package media |

The Rust executable checks MP4 structure directly. These are supported lower bounds. The [requirements table](docs/requirements.md#media-tools) records the builds used for qualification and explains which CI tools are pinned. Some older FFmpeg builds omit packet durations needed for encrypted audio verification; drmr refuses incomplete timing evidence. See [media processing](docs/media-processing.md) for details.

For the full source test suite, install [Bento4](https://www.bento4.com/downloads/): `mp4dump` provides an independent reference for MP4 parser checks, and `mp4encrypt` generates encrypted test fixtures. These additional tools are used by development tests.

## Start here

```sh
drmr doctor
drmr analyze /path/to/media.mp4 --metadata-only
drmr analyze /path/to/media.mp4 --json
drmr plan /path/to/manifest.mpd --to cbcs --format dash,hls
drmr convert --help
drmr verify --help
```

`analyze --metadata-only` reads MP4 metadata or DASH/HLS declarations from a local input or HTTP(S) URL without keys or external tools. It does not retain a URL source or verify complete media. Without that flag, `analyze` checks complete source media and identifies candidate output formats. Without `--save`, `plan INPUT --to ...` describes conditional intent from metadata. The former `inspect` command has been removed; see the [pre-release migration](docs/releases.md#pre-release-cli-consolidation).

Use global `--progress text` or `--progress json` to write progress to standard error, including alongside `--json` results. Standard output remains the final result. The default `auto` mode uses text for readable results. With `--json`, it also shows text when standard error is a terminal and stays silent when standard error is redirected. Explicit `text`, `json` or `off` overrides that choice. See [progress output](docs/usage.md#progress-output) for examples and scope.

To retain a supported HLS, static DASH or MP4 URL, save fixed conversion conditions, execute them offline,
and verify against the retained original:

```sh
drmr analyze "https://example.com/vod/master.m3u8" --download-dir ./source --json
drmr plan ./source/input.m3u8 --to cbcs --format dash --output ./result --save ./conversion.plan.json --json
drmr convert --plan ./conversion.plan.json --keys-file ./keys.json --json
drmr verify --plan ./conversion.plan.json --keys-file ./keys.json --json
```

Use a private [key configuration](docs/usage.md#separate-input-and-output-keys)
with an output key for CBCS. A saved plan contains source fingerprints and the
requested output intent, with runtime keys supplied separately. See the
[four-step guide](docs/executable-plans.md) for supported input, result checks,
source retention, and the difference from metadata-only planning. Replace the
example URL with a DASH MPD or MP4 URL and use `source/input.mpd` or
`source/input.mp4` for the plan. URL format detection uses the response content.

For HLS FairPlay and DASH Widevine/PlayReady from the same supported clear
transport-stream source, save an [output list](docs/multiple-outputs.md) and use
`plan --outputs outputs.json --output result --save conversion.plan.json`.
Each output has its own format, protection, DRM systems, and external
manifest-check policy. `convert --plan` publishes the complete result directory
after all outputs pass; `verify --plan` checks them again against the original.
The built-in Rust linter checks final HLS/DASH manifests during conversion and
full verification. Use `verify OUTPUT --manifest-only` to check manifests
separately, and add `--external-validator` for Apple or DASH-IF checks.
See [manifest validation](docs/manifest-validation.md) for rule coverage,
report scopes, and optional tool setup.

`doctor` discovers executable files without running them. Use `drmr doctor --probe --route progressive-mp4` to check versions and advertised options for a selected processing route. The report shows the actual resolved paths and distinguishes required tools from optional tools. These checks do not prove that a particular asset can be processed.

Use `convert INPUT --to ... --format ... --output ...` with a protected key configuration, then `verify OUTPUT --source INPUT` for independent comparison. See [conversion support](docs/conversion-support.md) for the tested protection routes, TS output options, and precise input profiles.

For a supported HLS, static DASH or MP4 URL, use `convert URL --download-dir ./source` with the target and output options below. The saved source can be reused offline. DASH acquisition requires one static Period, SegmentTemplate addressing and an explicit finite SegmentTimeline; live input, byte ranges and other DASH addressing profiles are outside this acquisition profile. See the [usage guide](docs/usage.md) for configuration examples, key handling, and output.

The common converter can also write explicit Widevine/PlayReady DASH and
FairPlay HLS information for qualified clear transport-stream HLS and clear MP4.
See [DRM signaling](docs/drm-signaling.md) for the input profiles, separate
runtime settings, generated declarations, and independent library checks.

## Supported input

The generic `convert` command accepts qualified local MP4, fragmented MP4 DASH/HLS, and MPEG transport stream HLS profiles, including encrypted input. Transport stream input can be clear, AES-128, or SAMPLE-AES with supplied keys and initialization vectors. See [conversion support](docs/conversion-support.md) for restrictions and tested routes.

Subtitle tracks and arbitrary track selection are outside the common conversion profile. Live input, discontinuities, and other unsupported manifest or codec profiles are refused.

drmr packages supplied media and signaling. It does not provide a DRM license server.

## Commands

| Command | Purpose |
| --- | --- |
| `doctor` | Discover tools; add `--probe` for version and option checks, or `--json` for scripts |
| `analyze` | Read local or URL metadata with `--metadata-only`; otherwise check complete local media or retain and check a supported URL with `--download-dir`; encrypted full checks use `--keys-file` |
| `plan` | Describe conditional intent; add `--save` and `--output` for an executable plan; `--outputs` selects named outputs |
| `convert` | Convert media directly or execute a saved plan with `--plan`; `--outputs` selects several outputs without saving a plan |
| `verify` | Compare result media with its original source; `--plan` also checks source identity and output intent; `--manifest-only` runs the native HLS/DASH linter; `--external-validator` adds external checks |
| `serve` | Start the experimental local web console |
| `version` | Print the version |

Use `drmr help <COMMAND>` for command options. See [usage](docs/usage.md) for examples and exit codes.

See [media processing](docs/media-processing.md) for encryption terminology, segment boundaries, and the checks behind the reported results.

See [processing routes](docs/processing-routes.md) to understand inspection evidence, conditional plans, and the route reported by a successful conversion.

See [resource limits and measurements](docs/resources.md) for automatic worker selection, memory estimates, and reproducible comparisons.
