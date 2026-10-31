# drmr

Rust command-line tools for inspecting and repackaging streaming media with **digital rights management (DRM)**.

drmr inspects local MP4, DASH, and HLS media and converts qualified on-demand presentations between clear, `cenc` (counter-mode encryption), `cbcs` (pattern encryption using cipher block chaining), HLS AES-128 protection, and SAMPLE-AES transport-stream encryption. Input decryption keys and output encryption keys are supplied separately. It preserves the existing video and audio encoding; it does not transcode or change quality settings.

**DASH** (Dynamic Adaptive Streaming over HTTP) and **HLS** (HTTP Live Streaming) describe media segments using a manifest or playlists. A **rendition** is one available video quality or audio track.

The CLI is the primary interface. The local web console and optional [Desktop host](docs/desktop.md) are experimental.

[Project website (English)](https://kazuhide.github.io/drmr/) · [Japanese website](https://kazuhide.github.io/drmr/ja/)

[Web user guide (English)](https://kazuhide.github.io/drmr/guide/) · [Japanese web guide](https://kazuhide.github.io/drmr/ja/guide/) · [Configuration reference](docs/configuration.md) · [CLI usage](docs/usage.md) · [Conversion support](docs/conversion-support.md)

[Releases and compatibility](docs/releases.md) · [Changelog](CHANGELOG.md)

See [keys and local security](docs/security.md) for diagnostic masking, temporary files, external-tool process arguments, and the local interface security boundary.

## Install

Download a CLI archive for your operating system and processor from
[GitHub Releases](https://github.com/kazuhide/drmr/releases), then follow the
[archive and version checks](docs/releases.md#install-a-released-cli). Rust is
only needed when building from source. Install external tools for the
conversion routes or additional checks you use.

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

Ordinary media checks in `analyze`, saved planning and `verify` run in Rust and
need no external media tools for supported inputs. `--decode` adds complete
decoding. Conversion and additional checks use the tools below. Add their
executable directories to `PATH`, the directories your shell searches.

| Executable | Supported from | Install | Purpose |
| --- | --- | --- | --- |
| `ffmpeg`, `ffprobe` | 9.0.1 | [FFmpeg](https://ffmpeg.org/download.html) | Additional complete decoding with `--decode` |
| `packager` | 3.8.0 | [Shaka Packager](https://github.com/shaka-project/shaka-packager#readme) | Encrypt and package media |

The Rust executable checks MP4 structure directly. These are supported lower bounds. The [requirements table](docs/requirements.md#media-tools) records the builds used for qualification and explains which CI tools are pinned. In the additional `--decode` path, some older FFmpeg builds omit packet durations needed for encrypted audio verification; drmr refuses incomplete timing evidence. See [media processing](docs/media-processing.md) for details.

Native checks retain all selected samples, codec configuration, clocks and
playback windows. They do not claim decoder execution. See
[verification depth and report schemas](docs/verification.md) for `--decode`,
the actual evidence reported, and legacy saved-plan requirements.

For the full source test suite, install [Bento4](https://www.bento4.com/downloads/): `mp4dump` provides an independent reference for MP4 parser checks, and `mp4encrypt` generates encrypted test fixtures. These additional tools are used by development tests.

## Start here

Choose an output in `outputs.json` and, when encryption is involved, supply a
private `keys.json`. One `convert` command handles source acquisition, analysis,
conversion, media comparison and native manifest checks. It publishes the result
only after those checks pass; separate `analyze`, `plan` and `verify` commands
are optional.

For example, this `outputs.json` requests CBCS fMP4 HLS for FairPlay:

```json
{
  "schema_version": 3,
  "outputs": [
    {"id": "hls", "format": "hls", "target": "cbcs", "drms": ["fairplay"]}
  ]
}
```

For an unencrypted source, `keys.json` needs only the output settings:

```json
{
  "schema_version": 2,
  "outputs": {
    "hls": {
      "kid": "00000000000000000000000000000001",
      "key": "00112233445566778899aabbccddeeff",
      "hls_key_uri": "skd://example",
      "signaling": {"system": "multi-drm", "fairplay": true}
    }
  }
}
```

These are public dummy values. Replace them with your source and issued key
material; drmr does not fetch a key or license from the advertised URI. Keep the
key file outside version control and the media directories, with owner-only
access (`chmod 600 keys.json` on macOS/Linux; use [ACLs on Windows](docs/windows.md)).
Select new source and result paths under existing parent directories. Then run:

```sh
drmr convert "https://example.com/vod/master.m3u8" \
  --download-dir source \
  --outputs outputs.json --keys-file keys.json \
  --output result
```

The entrypoint is `result/hls/master.m3u8`; the retained source is
`source/input.m3u8`. Replace the illustrative URL before running. For local
input, pass its path and omit `--download-dir`. Encrypted input adds its own
source-key mappings; clear-to-clear conversion omits `keys.json` and
`--keys-file` entirely.

The [configuration reference](docs/configuration.md) explains every field,
default and key role, with complete examples. The same output-list format works
for one to eight outputs. See [several named outputs](docs/multiple-outputs.md)
for DASH Widevine/PlayReady together with HLS carrying three DRM declarations.
[Conversion examples](https://kazuhide.github.io/drmr/#support) link each input
and output to the matching recipe. Container, codec, track and playback-window
conditions remain mandatory; a valid settings file does not qualify arbitrary
media or license services.

Use `analyze --metadata-only INPUT` for a quick key-free read of local or HTTP(S)
MP4, DASH or HLS declarations. It does not retain a URL source or verify complete
media. Use the [saved-plan workflow](docs/executable-plans.md) when you want to
inspect the source first, fix its fingerprints and destination, then run
`convert --plan` and a fresh independent `verify --plan`. `plan` reads your
output list; `plan --save` produces a different generated execution-plan file.

Use global `--progress text` for a live terminal status and a history of finished
stages, or `--progress json` for detailed observations on standard error. Standard
output remains the final result. The default `auto` mode shows text with `--json`
when standard error is a terminal and stays silent when it is redirected.
Explicit `text`, `json` or `off` overrides that choice. See
[progress output](docs/usage.md#progress-output).

The built-in Rust linter checks final HLS/DASH manifests during conversion and
full verification. Use `verify OUTPUT --manifest-only` for a separate manifest
check, and add `--external-validator` for optional Apple or DASH-IF checks.
See [manifest validation](docs/manifest-validation.md) for coverage and setup.
Matching encryption and DRM declarations does not prove license acquisition or
playback on a particular device.

`doctor` discovers executable files without running them. Use
`drmr doctor --probe --route dash-presentation` to check native conversion tools
for a selected route; add `--decode` to check decoder tools. These checks do not
prove that a particular asset can be processed.

## Supported input

The generic `convert` command accepts qualified local MP4, fragmented MP4 DASH/HLS, and MPEG transport stream HLS profiles, including encrypted input. Transport stream input can be clear, AES-128, or SAMPLE-AES with supplied keys and initialization vectors. See [conversion support](docs/conversion-support.md) for restrictions and tested routes.

Qualified TS HLS can also become one fragmented MP4 file with clear, `cenc`
or `cbcs` protection. Select at most one video and one audio rendition before
conversion; a master with several video qualities is refused for this output.
Qualified fragmented MP4, DASH and fMP4 HLS can produce clear, AES-128 or
FairPlay SAMPLE-AES TS HLS. Container changes use `auto` or `stitch` processing.
See [input profiles and output limits](docs/conversion-support.md) before
choosing the tracks and protection.

Subtitle tracks and arbitrary track selection are outside the common conversion profile. Live input, discontinuities, and other unsupported manifest or codec profiles are refused.

drmr packages supplied media and signaling. It does not provide a DRM license server.

## Commands

| Command | Purpose |
| --- | --- |
| `doctor` | Discover native conversion tools; add `--decode` for decoder requirements, `--probe` for version/option checks, or `--json` for scripts |
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

