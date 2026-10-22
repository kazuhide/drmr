# Changelog

Versions group user-visible changes. See [releases and compatibility](docs/releases.md)
for installation, stable interfaces and versioning.

## [Unreleased]

## [1.0.0] - 2026-10-22

First stable CLI release. The local Web console and Desktop host remain
experimental.

The eight public commands are `inspect`, `analyze`, `plan`, `convert`, `verify`,
`doctor`, `serve` and `version`.

### Media workflows

- Inspect local MP4, DASH and HLS metadata; analyze complete media before
  choosing an output.
- Retain supported MP4, static DASH and completed HLS URLs for offline work,
  including supported encrypted input with operator-supplied keys.
- Save source fingerprints and output conditions in an execution plan, convert
  from that plan, then independently verify the current result against the
  retained original. Single-output version-1 plans and version-2 plans with up
  to eight named outputs are supported.
- Convert the documented profiles between clear media, `cenc`, `cbcs`, HLS
  AES-128 and transport-stream SAMPLE-AES without changing the audio or video
  encoding. See the [support matrix](docs/conversion-support.md) for individual
  routes and input restrictions.
- Generate DASH and HLS manifests, including separate Widevine/PlayReady DASH
  and FairPlay HLS outputs from qualified clear sources. Keys and DRM settings
  are supplied separately for each output.

### Verification and operation

- Check encoded content, complete decoding, timing and actual output protection
  against the source. A saved plan also checks source identity and output intent.
- Lint final HLS and DASH manifests with the built-in Rust implementation.
  Optional Apple and DASH-IF checks validate final manifests separately from
  media correspondence.
- Choose worker limits from processor and memory capacity, with an explicit
  `--max-workers` upper bound and optional measurement reports.
- Keep runtime keys in protected configuration, mask sensitive diagnostics,
  and supervise external media processes. Saved-plan conversion publishes new
  output only after the required checks complete.
- Provide CLI archives for Linux x86-64, Apple Silicon macOS and Windows x86-64,
  with build manifests and checksums. External media tools remain separate;
  Docker can supply them together with the CLI.

### Scope

- Remove the pre-1.0 `run`, `batch`, `analyze-hls`, `package-hls`,
  `verify-snapshot` and `canonical` commands, the catalog-only `plan --mpd` and
  `plan --asset-id` options, and the catalog/queue APIs and screens. The console
  uses the common saved-plan workflow. `DRMR_INPUT_ROOT` replaces `DRMR_CATALOG_ROOT`;
  `DRMR_HLS_PROFILES` is removed.
- The CLI compatibility policy begins at 1.0.0. Saved-plan and report schema
  versions are independent of the product version.
- Supported formats are limited to the documented on-demand profiles. License
  acquisition and device DRM playback are outside the conversion checks.
- The Web/Desktop saved-plan workflow uses the CLI. Native Desktop results and
  unqualified platforms are listed in the [Desktop guide](docs/desktop.md#tested-results).
