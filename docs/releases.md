# Releases and compatibility

The documented CLI has the 1.x compatibility contract described below. Release
maturity is separate: check the **Pre-release** label on each
[GitHub release](https://github.com/kazuhide/drmr/releases) before choosing a
version. A 1.x version number alone does not establish complete media or platform
qualification. The local Web console and Desktop host remain experimental.

Version 1.0.3 adds CENC DASH DRM declarations, TS output from qualified
fragmented MP4, and single MP4 output from TS. See the [changelog](../CHANGELOG.md) and
[conversion profiles](conversion-support.md) for source and track restrictions.
It retains native media checks by default, source-relative segmented output
paths, and processing and DASH-addressing choices. Fixed progress columns and
aggregate task counts describe bounded parallel work;
the [measurements](performance.md) do not establish a consistent whole-command
speedup for every added concurrency stage.

The 44.1 kHz AAC support and supported final AAC playback ranges from 1.0.1
are retained. See [AAC processing and playback boundaries](media-processing.md#aac-sample-rates-and-transport-clocks).
Check each release's status, requirements and verification reports; these
conditions do not qualify arbitrary codecs, broadcast streams or devices.

Published v1.0.0 and v1.0.1 remain immutable pre-releases. The v1.0.0 archives
retain the original 48 kHz-only TS audio profile. Both releases' tags, archives
and original verification reports remain unchanged.

## Install a released CLI

Choose a version and its CLI archive from
[GitHub Releases](https://github.com/kazuhide/drmr/releases). Match the operating
system and processor in the release notes:

| Operating system | Processor | Rust target |
| --- | --- | --- |
| Linux | x86-64 | `x86_64-unknown-linux-gnu` |
| macOS | Apple Silicon | `aarch64-apple-darwin` |
| Windows | x86-64 | `x86_64-pc-windows-msvc` |

CLI archive names include the version and target. For 1.0.3, they are
`drmr-1.0.3-x86_64-unknown-linux-gnu.tar.gz`,
`drmr-1.0.3-aarch64-apple-darwin.tar.gz` and
`drmr-1.0.3-x86_64-pc-windows-msvc.zip`, respectively. Choose an archive actually
listed on GitHub Releases, and download its documentation and verification
reports from that same release. Current repository documentation can describe
changes absent from an older archive.

These are the native CLI distribution targets. They do not establish support
for other processor architectures or native Desktop operation. See
[Windows conditions](windows.md) and [Desktop qualification](desktop.md#tested-results)
for their separate requirements.

Download the CLI archive and the release's `SHA256SUMS` file. Compare the archive's
SHA-256 digest with its entry before extracting it. For example:

```sh
shasum -a 256 /path/to/downloaded-archive.tar.gz
```

On Windows, use:

```powershell
Get-FileHash -Algorithm SHA256 C:\Downloads\downloaded-archive.zip
```

The checksum detects different bytes; it is not a substitute for trusting the
release source. Each archive also contains `build-manifest.json`, identifying
the source commit, tree, target, compiler and binary hashes. Release tags and
assets are fixed after publication. Corrections use a new version. GitHub marks
protected releases as [immutable](https://docs.github.com/en/code-security/concepts/supply-chain-security/immutable-releases).

Extract the archive, add its executable directory to `PATH`, then run:

```sh
drmr --version
drmr doctor --probe
```

The reported version must match the release. A CLI archive does not need Rust
installed. In the current source, native analysis and verification need no external media tools. Shaka Packager is required by packaging routes; optional `--decode` needs FFmpeg and ffprobe. Progressive MP4 copying and decryption run in Rust. Install the tools required by your release and workflow separately;
see [requirements](requirements.md). `doctor` checks the selected tool setup,
while a successful conversion and independent `verify` check the actual media.
See the [four-step workflow](executable-plans.md) and
[input and output support](conversion-support.md) before selecting a route.

GitHub's automatically generated **Source code** downloads contain source,
rather than a compiled CLI. Docker is a separate installation option described
in the [Docker guide](docker.md).

## Pre-release CLI consolidation

During the 1.0.1 pre-release, `inspect` is removed and its metadata report moves
to `analyze --metadata-only`. Replace `drmr inspect INPUT --json` with
`drmr analyze --metadata-only INPUT --json`; no `inspect` alias remains.
This is an explicit exception to the command-preservation rule below while the
pre-release CLI is consolidated. Published v1.0.0 archives and their original
documentation remain unchanged.

The replacement reads local MP4, HLS or DASH metadata and also accepts HTTP(S)
URLs without keys or a download directory. URL metadata reads are temporary;
they do not retain an original for a saved plan or establish complete media
validity. Normal `analyze URL --download-dir NEW_DIR` retains and checks the
complete source as before. See [metadata reading and progress](usage.md).

## What stays compatible in 1.x

drmr follows [Semantic Versioning](https://semver.org/). Compatibility covers
the documented CLI, rather than every implementation detail:

- Except for the pre-release migration above, existing commands, options and
  documented exit-code meanings remain usable.
  Deprecated interfaces remain available throughout 1.x, with migration guidance
  before removal in a major release.
- Supported configuration formats retain their existing fields and meanings.
  This includes version-1 and version-2 operator key files, fetch settings and
  output lists. New optional settings may be added.
- Machine-readable reports retain documented fields, their types and meanings
  within a report schema. Programs must ignore additional fields and handle
  unknown diagnostic codes or status values as unrecognized results, rather
  than treating them as success. Report schema versions are separate from the
  drmr version.
- Valid saved execution plans using schema version 1 or 2 remain readable
  throughout 1.x. Plan versions describe JSON formats, not drmr releases.

Legacy saved-plan version 1 describes one output; version 2 describes one to eight named outputs. Both remain strict and keep a minimum of complete decoded verification. Versions 3 and 4 additionally bind `required_depth` in their identity; versions 5 and 6 bind processing and boundary behavior. Versions 7 and 8 additionally bind source-relative placement and DASH references. New single-output plans use version 9 and named-output plans use version 10, including a one-output console list. They also bind source-derived media filenames. Versions 1–8 retain their identities, fixed packaging requests and recorded placement and filenames. Execution and fresh verification may strengthen the saved minimum with `--decode`, but cannot lower it or override the processing or output settings.

New complete-media report version 2 states native checks, required depth and actual decode evidence. A `not-checked` decode result does not claim decoded equality. Legacy report version 1 keeps its original full-decode meaning. These report and plan changes do not change operator key-file versions.

All saved-plan versions fix source fingerprints and output conditions while keeping key
values and DRM initialization data separate. See
[single-output plans](executable-plans.md) and [multiple outputs](multiple-outputs.md).

Being readable does not make a plan portable or authorize unsafe input. Source
files must remain at their recorded locations and match their fingerprints.
Runtime keys, tools, supported media conditions and destination permissions must
still satisfy the checks. Newer 1.x releases can reject previously accepted
invalid or unsafe media when fixing a validation or security defect. Such
changes are described in the release notes.

Human-readable wording, JSON object order, progress-event order, timing
measurements and exact generated media bytes are not stable interfaces.
The experimental Web/Desktop interfaces and their internal protocols may
change. Workspace Rust crates are not published as a supported SDK, so this
policy does not promise Rust library API compatibility.

## What key validation establishes

Key-file checks validate the JSON format, supported schema, value lengths,
identifiers and the input/output roles required by the selected operation.
Multiple-output checks also match encrypted output IDs and their DRM settings.
Actual media checks use the supplied keys to compare encoded content, decoding,
timing and protection with the original. A well-formed key file alone does not
prove that its keys decrypt an asset.

These checks do not obtain a DRM license or qualify a license service, secure
hardware or device playback. Generated manifests intentionally contain the DRM
declarations configured for publication. See [key handling and security](security.md)
and [DRM signaling](drm-signaling.md) for the boundaries.

## When versions change

Ordinary pull requests do not change the product version. User-visible changes
are collected under **Unreleased** in the [changelog](../CHANGELOG.md). A release
groups useful changes into a version; it does not correspond to every merge.
Security and serious correctness fixes can be released promptly.

| Change since the previous release | Version example |
| --- | --- |
| Compatible bug fix or performance improvement | `1.0.1` |
| Compatible new command, option or processing capability | `1.1.0` |
| Incompatible change to a stable interface | `2.0.0` |

A release fixes its tag, product version, source identity and distribution
archives together. Before publication, the actual extracted CLI archives run
tool checks and the analyze → saved plan → convert → independent verify workflow
on their matching native targets. The release notes identify supported targets,
requirements and remaining scope limits. These checks supplement the existing
unit, media, manifest and archive-content tests; they do not claim universal
media or DRM-device compatibility.

The current source's archive qualifier retains 18 decoded/metadata stages: the original 11-stage
video workflow, six stages that create and probe a 44.1 kHz AAC TS fixture,
then analyze it, save a plan, convert it to CBCS DASH, and independently verify it,
and a metadata-only analysis of the retained clear DASH source. The metadata
stage uses `analyze --metadata-only --json` without keys and does not replace
any of the previous 17 stages.
Windows checks retain the complete ordinary-user regression suite on both
the minimum and stable Rust versions. Independent test binaries run within
CPU and memory limits. CLI builds and extracted-archive qualification run
concurrently with those tests; merging and publication require every check
in the workflow to succeed for the same source.

The existing v1.0.0 assets and their original 11-stage reports are unchanged.
These additional checks apply to newly built CI archives; they do not qualify the
old release archives for 44.1 kHz input.

## Codec checks on extracted archives

Current CI also checks the extracted Linux, macOS and Windows CLI against the
committed ten-second HEVC Main/Main10 and six VP9 profile/depth fixtures.
These checks do not rebuild or regenerate the fixtures. Each fixture runs
`analyze`, saves a native execution plan, converts using that exact plan, and
independently verifies the result. A return conversion is then compared directly
with the original fixture. HEVC smoke cases use CBCS fMP4 HLS with FairPlay,
Widevine and PlayReady declarations; VP9 smoke cases use CENC DASH with Widevine
and PlayReady declarations.

The eight normal-CI round trips are distinct from the full codec matrix below.
Both scopes require 38 refusal controls: unsupported MPEG-TS output for every
fixture, VP9/CBCS output, missing and incorrect keys, and one changed media
sample. Decoder executables are unavailable during codec checks. A native pass
establishes complete sample, metadata, timing and manifest consistency; it does
not establish decoded-pixel equality, HDR, license acquisition or device playback.

To qualify a previously built CI archive on its native system without creating a
tag or release, use a clean checkout of its exact source revision:

```sh
python3 tools/codec_qualification.py \
  --archive /path/to/ci-archive.tar.gz \
  --target aarch64-apple-darwin --expected-version 1.0.3 \
  --source /path/to/exact-source --scope full --jobs 4 \
  --report /path/outside/source/codec-qualification.json
```

Choose the actual target and archive name; Windows accepts its `.zip` archive.
The command rechecks archive contents, notices, native executable header and
version, then binds the codec report to the source commit/tree and archive/binary
hashes. It neither compiles the CLI nor repeats the H.264/AAC release matrix.
It refuses elevated or non-native execution. `--scope smoke` runs only the eight
representative round trips; that report cannot substitute for a full report.
Reports contain fixed classifications and allowlisted evidence, not temporary
keys, paths or child-process output. All owned media and keys are removed after
checking. This source qualification does not change existing v1.0.3 archives.

The `Codec archives` manual workflow runs this full check on all three native
systems using archives from successful `CI` and `Windows` runs. Choose the two
run IDs for the same exact source commit, then dispatch it without publishing:

```sh
gh workflow run codec-archives.yml --ref main \
  -f source_sha=EXACT_SOURCE_COMMIT \
  -f ci_run_id=CI_RUN_ID -f windows_run_id=WINDOWS_RUN_ID
```

The workflow validates run and archive identities before native execution and
retains the three full codec reports as CI artifacts. It fetches locked package
metadata for archive verification but does not rebuild the CLI or repeat full
source regressions. All three native jobs must finish successfully; a successful
smoke run alone does not establish full-matrix qualification.

## Release-only round-trip qualification

The manual release workflow additionally runs the extracted Linux, macOS and
Windows CLI archives through the documented input/output matrix. It reuses the
archives and their 18-stage reports from successful CI for the exact tagged main
commit. It does not rebuild the CLI or rerun the ordinary platform regression
suites. Publication assembly requires all three native round-trip reports;
staging the archives alone produces an explicitly unqualified input packet.

The test generates short fixtures from the attributed ten-second Big Buck Bunny
clip once per native job, then reuses each source graph. HLS/DASH segments target
two seconds. The five layouts are H.264 video with B-frames, AAC audio at 44.1 kHz,
AAC audio at 48 kHz, and one video plus one audio track at each sample rate. The
audio fixtures declare only 240 presented samples in their final complete AAC
frame. Every coded sample, its clock, the visible playback range, and metadata
representable by both containers must survive conversion. HLS selection-group
attributes have no MP4 equivalent; this test does not claim to preserve them in
MP4 files. Language labels use the documented semantic mapping, such as `en` to
the MP4 code `eng`.
The joint HLS source uses the shared mapping's `default-audio-group`, audio name
`stream_1`, language `en`, `DEFAULT=NO` and `AUTOSELECT=YES`. This follows the
common materializer's video-first, audio-second track order. Audio-only seeds
use a media playlist without rendition names or selection attributes; the test
does not invent those attributes. These per-layout fixture values are bound in
the report and matrix identity. A round trip through DASH does not promise to restore
arbitrary original HLS selection groups or player selection attributes; see
[metadata preservation](media-processing.md#multiple-qualities-and-audio-declarations).

Each matrix cell expands to the input protection schemes and qualified track
layouts. The matrix uses automatic processing and default DASH `SegmentTemplate`
references; separate regressions cover other processing and addressing options.
The report retains 845 H.264/AAC records across 13 input and output profiles:

| Records per native archive | Required check |
| --- | --- |
| 761 reversible cases | Convert A → B, independently verify B against A, convert B → A, independently verify the return leg, and compare final A directly with original A |
| 42 joint MP4 → DASH/HLS cases | Convert and independently verify the forward direction; the reverse joint DASH/HLS → MP4 layout is not qualified |
| 42 joint DASH/HLS → MP4 conditions | Explicitly record the single-track MP4 output restriction rather than report a successful conversion |

An additional codec report requires 534 reversible cases: all 100 valid
container/protection pairs for each HEVC fixture, all 49 pairs for each VP9
fixture, and 40 explicit `segments`/`stitch` cases for same-container DASH and
fMP4 HLS. HEVC uses clear, CENC and CBCS in MP4, DASH and fMP4 HLS; VP9 uses
clear and CENC. Both also cover whole-resource AES-128 fMP4 HLS. Protected-to-
protected cases use distinct output keys, and every return result is checked
against the original clear fixture. VP9/CBCS and MPEG-TS output remain refusal
controls rather than successful matrix cells.

These are codec and layout conditions, not a claim that every possible input
bitstream is supported. Multiple video qualities are not silently selected for
one MP4 output. Every input profile is generated with the extracted CLI and
independently checked against its retained seed. CENC fMP4 HLS requests Widevine
and PlayReady declarations; CBCS fMP4 HLS requests FairPlay, Widevine and
PlayReady. Every encrypted DASH output requests Widevine and PlayReady.
The report verifies each required DRM key format and the target encryption
method, without claiming license acquisition. CENC HLS uses the base HLS manifest
profile; Apple authoring's separate prohibition on CTR remains an error under
the Apple-specific profile.

Independent workers run within a fixed resource limit and each CLI invocation
uses one media worker. Completed case outputs are removed after their evidence
has been collected. FFmpeg and FFprobe are used to generate and check fixtures;
the conversion matrix itself uses native complete-media verification without
`--decode`. Matrix workers receive an absolute Packager path and a tool search
path containing only failing FFmpeg/FFprobe guards. A native preflight checks
that both guards refuse execution, so an unintended external inspection or
decoding fallback cannot qualify. The existing 18-stage archive qualification retains its separate
decoded checks. Archive-media and release-roundtrip report schema 2 require the
scope-bound codec report; older reports without it cannot qualify a new release.
These report versions are independent of the CLI version. Neither report establishes DRM license acquisition or device
playback.

The `cli-release-roundtrip-TARGET.json` assets bind the source commit and tree,
archive and binary hashes, native executable header and reported version,
attributed fixture hash, generated source graphs, and the exact matrix case
identities. They distinguish complete, failed, not checked and unqualified
records. Missing reports, incomplete native proofs, unexpected case counts or
mismatched identities block publication assembly. The reports themselves are
included in the release manifest and `SHA256SUMS`. Test keys use temporary
owner-protected files; reports contain allowlisted evidence rather than keys,
source addresses or raw command output.
