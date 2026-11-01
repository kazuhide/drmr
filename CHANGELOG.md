# Changelog

Versions group user-visible changes. See [releases and compatibility](docs/releases.md)
for installation, stable interfaces and versioning.

## [Unreleased]

### Added

- Convert and independently verify qualified VP9 MP4/fMP4 clear and CENC media,
  including persistent reference/superframe checks, native cipher spans,
  known-key roundtrips and separate generic/Apple HLS codec diagnostics.
- Convert and independently verify qualified HEVC Main/Main10 in MP4 and fMP4
  HLS/DASH, including clear, CENC and CBCS media. Native checks read every sample,
  parameter set, slice header and clock; optional decoding remains separate.
- Add attributed Main/Main10 regression seeds, independent packet/decode oracles
  and tamper/refusal coverage. Document the accepted codec profiles separately
  from container, encryption and DRM-device claims.

## [1.0.3] - 2026-10-31

Extend the qualified output profiles while retaining native media verification
and the existing CLI workflow. Web and Desktop remain experimental.

### Added

- Generate and independently check Widevine and PlayReady declarations for
  CENC (counter-mode) DASH, including PlayReady AESCTR headers and their key
  checksum. CBCS retains its separate AESCBC header profile.
- Generate CBCS fMP4 HLS with FairPlay, Widevine and PlayReady together, or
  any supported subset. CENC fMP4 HLS supports Widevine and PlayReady with
  `SAMPLE-AES-CTR`; FairPlay and Apple's authoring profile require CBCS.
  Check each provider declaration against the actual encryption and key
  identifier during conversion, retained-source acquisition and fresh verification.
- Package qualified fragmented MP4 files, DASH and fMP4 HLS as clear, AES-128
  or FairPlay SAMPLE-AES TS HLS. Encrypted input requires known keys.
  Container changes use `auto` or `stitch`; `segments` refuses them.
- Write one fragmented MP4 file with clear, CENC or CBCS protection from
  qualified clear, AES-128 or SAMPLE-AES TS HLS. Support audio only, video only,
  or one of each, retaining encoded samples, absolute clocks and exact final
  AAC playback ranges at 44.1 or 48 kHz. Ambiguous video or audio selections
  are refused rather than silently dropped.
- Link the website's input/output matrix to examples for the added routes,
  including separate clear-input and encrypted-input instructions.
- Qualify release archives with native A → B → A comparisons across the
  documented container/protection profiles. Reuse generated short fixtures,
  verify every conversion independently, and bind complete reports to each
  archive and source revision. Explicit track-layout conditions remain separate
  from successful conversions.

### Fixed

- Use validated MPEG-TS frame dimensions when sizing independent bundle
  verification, so small inputs can be checked within their memory budget.
- Describe HLS selections containing one video and one audio track as conditional
  MP4 planning candidates. Execution still validates the actual container,
  tracks, clocks and presentation windows.
- Normalize relative single-MP4 output paths before safe publication.
- Mask nested encoded DRM records as complete values and bound diagnostic
  search work. Keep JSON Lines records valid when a diagnostic is fully redacted.

### Requirements and verification

Native media checks run in Rust; packaging and output encryption use Shaka
Packager where required. Additional complete decoding needs FFmpeg and
ffprobe. Short attributed Big Buck Bunny regressions cover native conversion,
saved plans, direct conversion, independent verification and refusal cases;
selected cases also compare complete decoded media. Consult each release's
native archive reports for its exact source, tools and platform evidence.
These checks do not acquire DRM licenses or qualify device playback.

## [1.0.2] - 2026-10-30

CLI release for the supported media profiles, with native validation,
bounded parallel processing and readable output summaries. The Web console
and Desktop host remain experimental. Published v1.0.0 and v1.0.1 archives
remain unchanged pre-releases.

### Fixed

- Keep progress bars in fixed columns when task descriptions, counts,
  estimates or log lines change. Summarize task counts in source and output
  rows, without individual rendition rows. Refresh ordinary terminal frames
  at most once per second, including phase and output-state changes; show
  final success, failure and cancellation immediately.
- Keep terminal progress below the command in the existing screen. Preserve
  earlier output; after resizing, keep the old snapshot in history and start
  a fresh inline view. Clear only owned progress rows before final output.
- Use Shaka's serial job manager to avoid lost completion notifications.
  Keep Rust worker pools and independent output conversions concurrent.

### Changed

- Parallelize source snapshots, profile checks, original-to-intermediate
  comparisons and post-capture DASH metadata work within shared CPU/memory
  limits. Run media jobs serially inside each Shaka process.
  Preserve ordered results, full checks and failure/cancellation cleanup.
- Record staged conversion measurements without claiming a consistent
  whole-command speedup for every added concurrency stage.
- Reduce temporary sample copying and reuse the independently captured output
  for final DRM-declaration checks. Keep complete media comparisons, final
  original-file checks and bounded streaming fallbacks; record repeated
  conversion measurements at the same verification depth.
- Show preparation counts and media-comparison progress for named outputs.
  Qualified shared-source conversions include approximate work and remaining
  time from this operation's observed stages. Parallel output durations are
  not added together, and whole-result completion follows publication.
- Name the media, native manifest and DRM checks in named-output conversion
  results. Optional external validator results appear only when requested;
  independent `verify` continues to read and check the saved files again.
- Keep source-relative media directories in new URL downloads and segmented
  outputs, including shared directories with distinct filenames. Existing saved
  plans retain their recorded placement.
- Select numbered duration templates, exact timelines, explicit lists or indexed
  single-file DASH output with `--dash-addressing`. Accept the supported finite
  forms as local or URL inputs and verify the final references and media.
- Retain the common HLS target-duration bound in new segmented outputs. Derive
  a safe repackaging request from admitted video intervals, save that request,
  and keep existing plans' fixed settings. Retain the original HLS bound only
  after checking all final durations; recalculate master bandwidth from final
  bytes and the declared bound.
- Apply MP4 playback-time offsets to DASH references, presentation durations and
  indexes. Segment stitched AAC relative to its own start, then restore its
  original timestamps before the complete source-to-output checks.
- Copy and fingerprint independent source files in parallel within the shared
  worker and memory limits. Discover manifests in order, verify the complete
  retained source graph, and drain active workers before failure cleanup.
- Select processor acceleration for AES encryption and media SHA-256 checksums
  at runtime, with a software fallback on other supported processors. Keep
  complete source, sample, timing and output checks at the selected verification
  depth.
- Write qualified encrypted progressive MP4 to clear MP4 in Rust, preserving
  every encoded sample, timestamp and playback edit. Native analysis, planning,
  conversion and verification need no FFmpeg or ffprobe; those tools are used
  only for additional complete decoding. `doctor` checks native conversion
  requirements by default; add `--decode` to check decoder requirements too.
- Add `--processing auto|segments|stitch` to saved planning and direct conversion,
  with per-output choices in the shared CLI, Web and Desktop workflow. Native
  TS and fragmented-MP4 routes can re-encrypt original resources while retaining
  every sample's segment membership and clocks. `stitch` prepares complete
  tracks and packages the requested output; HLS and DASH are segmented again.
- Bind processing, boundary guarantees, source-relative directories, media
  filenames and DASH references in new single-output plan schema 9 and
  named-output schema 10. Existing plan schemas 1–8 retain their identities,
  recorded settings and verification minimums. Output recipe schema 3 adds
  per-output DASH references and processing choices; recipe schemas 1/2 and
  operator key-file schemas remain unchanged.
- Use native Rust media validation by default for complete analysis, saved
  planning, conversion and independent verification. Check every admitted
  sample, codec configuration, clock and supported playback window; refuse
  unsupported or incomplete evidence. Add `--decode` for complete decoding in
  addition to those native checks.
- Emit complete-media report schema 2 with explicit required depth, scope and
  actual decode evidence. Plans bind their minimum depth to the plan ID. Legacy
  plans 1/2 remain strict and retain a complete-decoding minimum; plans 3/4 keep
  their saved depth and complete-track packaging. The common Web/Desktop
  workflow shares this policy; operator key-file versions stay 1/2.
- Compute complete decoded-content SHA-256 in Rust with processor acceleration
  where available, preserving the same decoded bytes and FFmpeg timestamp
  refusal checks. Decoded media is streamed instead of retained in memory.
- Check prepared source tracks directly during complete input admission,
  avoiding repeated decoded-content matching against the same track set.
  MPEG-TS input checks retain complete original-packet and timing evidence;
  requested decoding adds complete decoded-content evidence. Independent
  source/output verification remains separate.
- Keep terminal progress in one live status with a history of finished stages
  and their durations. Redirected text logs record stage changes and occasional
  updates instead of every observation; detailed JSON Lines remain available.
- Show percentages for resource transfers and the current TS media playlist
  once their totals are known, and approximate remaining time after enough
  progress is observed. Keep discovery and unknown work unestimated, and
  distinguish collected segments from the final ordered boundary checks.
- Read independent TS segment metadata and encoded frames in parallel within
  worker limits, while retaining ordered duration and clock checks and keeping
  only a bounded set of compact results in memory.

### Removed

- Remove the unused subprocess media adapter and its engine interface. Current
  CLI, Web and Desktop processing uses the common native conversion workflow.

### Requirements and verification

CLI archives target Linux x86-64, Apple Silicon macOS and Windows x86-64.
Native analysis, planning and verification need no external media tools.
Packaging routes use Shaka Packager; optional complete decoding also needs
FFmpeg and ffprobe. Progressive MP4 copying and decryption run in Rust. See
[installation and compatibility](https://github.com/kazuhide/drmr/blob/v1.0.2/docs/releases.md)
and [requirements](https://github.com/kazuhide/drmr/blob/v1.0.2/docs/requirements.md).

Each native archive includes source/build identity and dependency notices,
with separate archive-content and extracted-CLI media qualification reports.
These checks cover the documented fixtures and profiles; they do not qualify
arbitrary media, DRM license services or device playback. Web and Desktop
remain experimental.

## [1.0.1] - 2026-10-23

Pre-release update for AAC in MPEG transport streams, key-free metadata
analysis and command progress. The published v1.0.0 archives remain unchanged.

### Added

- Read local and HTTP(S) MP4, HLS and DASH metadata with
  `analyze --metadata-only`, without keys or retained source downloads. This
  replaces the removed `inspect` command during the pre-release CLI
  consolidation; normal full-source analysis is unchanged.
- Select stderr progress with global `--progress auto|text|json|off`, including
  progress alongside JSON results, actual acquisition counters and periodic
  elapsed-time observations without estimated percentages or completion times.

### Fixed

- Preserve a uniquely declared partial final AAC playback range from HLS/TS in
  HLS and DASH, while retaining every coded frame, raw timestamp and decoded
  sample. Verify the result's own edit list and manifest boundaries independently.
- Accept AAC-LC at 44.1 kHz alongside 48 kHz in supported clear, AES-128 and
  SAMPLE-AES HLS transport streams, including audio-only input.
- Preserve every original AAC frame, decoded sample and absolute presentation
  and decoding timestamp when producing fragmented MP4 HLS, DASH or TS HLS.
  Native audio demuxing avoids accumulated timestamp rounding without
  re-encoding or changing the source sample rate.
- Validate audio segment durations using actual segment boundaries and explicit
  final-frame representations, rather than assuming that every audio frame
  occupies an integral number of transport-clock ticks.
- Retain exact 48 kHz checks and add 44.1 kHz configuration, clock, protection,
  complete-content and independent verification regressions.

### Requirements and verification

CLI archives target Linux x86-64, Apple Silicon macOS and Windows x86-64.
FFmpeg, ffprobe and Shaka Packager are installed separately; see
[installation and compatibility](https://github.com/kazuhide/drmr/blob/v1.0.1/docs/releases.md)
and [runtime requirements](https://github.com/kazuhide/drmr/blob/v1.0.1/docs/requirements.md).
Rust is required only for source builds.

Archive qualification checks the extracted CLI with ten-second video and
44.1 kHz AAC transport-stream fixtures, including saved-plan conversion and
independent verification. Reports accompany each archive. These checks do not
qualify arbitrary media, DRM license services or device playback. The Web console
and Desktop host remain experimental.

## [1.0.0] - 2026-10-22

Initial CLI release, subsequently reclassified as a pre-release candidate.
Its TS AAC input is limited to 48 kHz. The local Web console and Desktop host
remain experimental.

See [installation and compatibility](https://github.com/kazuhide/drmr/blob/v1.0.0/docs/releases.md)
and [runtime requirements](https://github.com/kazuhide/drmr/blob/v1.0.0/docs/requirements.md).

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
  encoding. See the [support matrix](https://github.com/kazuhide/drmr/blob/v1.0.0/docs/conversion-support.md) for individual
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
  unqualified platforms are listed in the [Desktop guide](https://github.com/kazuhide/drmr/blob/v1.0.0/docs/desktop.md#tested-results).
