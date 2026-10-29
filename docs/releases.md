# Releases and compatibility

The documented CLI has the 1.x compatibility contract described below. Release
maturity is separate: check the **Pre-release** label on each
[GitHub release](https://github.com/kazuhide/drmr/releases) before choosing a
version. A 1.x version number alone does not establish complete media or platform
qualification. The local Web console and Desktop host remain experimental.

Version 1.0.2 groups the [changelog](../CHANGELOG.md) changes since the pre-release
updates. It uses native media checks by default, keeps source-relative output
paths and supports processing and DASH-addressing choices. Fixed progress
columns and observed rendition task states describe bounded parallel work;
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

CLI archive names include the version and target. For 1.0.2, they are
`drmr-1.0.2-x86_64-unknown-linux-gnu.tar.gz`,
`drmr-1.0.2-aarch64-apple-darwin.tar.gz` and
`drmr-1.0.2-x86_64-pc-windows-msvc.zip`, respectively. Choose an archive actually
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

The current source's archive qualifier defines 18 stages: the original 11-stage
video workflow, six stages that create and probe a 44.1 kHz AAC TS fixture,
then analyze it, save a plan, convert it to CBCS DASH, and independently verify it,
and a metadata-only analysis of the retained clear DASH source. The metadata
stage uses `analyze --metadata-only --json` without keys and does not replace
any of the previous 17 stages.
Windows release checks retain minimum and stable Rust builds, native platform
guards, and the extracted archive qualification as an ordinary user. The
complete Windows test suite runs separately after updates to main and on manual
request, without delaying archive builds or publication. Its results are
reported separately from the release archive checks; a qualified archive does
not imply that every Windows regression test has completed.

The existing v1.0.0 assets and their original 11-stage reports are unchanged.
These additional checks apply to newly built CI archives; they do not qualify the
old release archives for 44.1 kHz input.
