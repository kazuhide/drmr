# Analyze, save a plan, convert, and verify

Use this workflow when you want to check the complete input first, review one
fixed conversion, and verify its result against the retained original. It
supports the [qualified local media profiles](conversion-support.md), including
supported HLS, static DASH and MP4 URLs within the documented acquisition profile.

For several outputs with separate DRM systems and final-manifest checks, use
the [multiple-output workflow](multiple-outputs.md). The single-output examples
below create schema-version-5 plans. Legacy version-one and version-three plans
remain readable with their recorded validation and packaging behavior.

## 1. Analyze and retain the input

```sh
drmr analyze "https://example.com/vod/master.m3u8" --download-dir ./source --json
```

`source` must be a new directory under an existing parent. The command downloads
the complete presentation, retains its original manifests and media, and creates
`source/input.m3u8` with local references. For a DASH URL, the local entrypoint is
`source/input.mpd`; for an MP4 URL it is `source/input.mp4`. Pass that local path
to the subsequent `plan` command. Format detection uses the response content,
including for URLs without a filename extension. It checks all admitted tracks, source
decryption when needed, every admitted compressed sample, codec configuration,
playback windows and timing in Rust. Add `--decode` for complete source decoding.
This reads the whole asset; it is not a
quick check of the master playlist. A completed download survives a later media
admission failure so that you can examine it locally.

The version-two JSON reports `input_admitted`, typed `verification` evidence, the actual container in
`actual_format`, protection in `track_protection`, and candidate output formats
in `eligible_formats`. Input admission does not establish a converted result,
license acquisition, or device playback. An unsupported profile returns failure
with a fixed error code; it is not a claim that the media formats cannot support
the transformation.

A track can report `mixed`, for example when supported local HLS switches
between AES-128 and clear resources. This is a source fact, not another output
target. Verification does not accept mixed protection as matching a plan for
one uniform output protection scheme.

`compatibility_notes` lists fixed codes when source admission applies a documented
compatibility rule, such as empty TS padding or adjacent zero-duration HLS cue
markers. These notes do not waive sample or timing checks, or requested decoding. See
[transport-stream compatibility](media-processing.md#transport-stream-compatibility)
for the accepted conditions and handling of the unchanged original.

For a local input, use `drmr analyze original/manifest.mpd --json` or
`drmr analyze original.mp4 --json`. Supply `--keys-file` for encrypted local or URL input, including source checks when saving a plan.
Local input does not accept download options. `analyze --metadata-only INPUT`
reads local or URL metadata without keys or complete media checks. Its temporary
URL reads retain no original for a saved plan; use full analysis above to save
and admit the source.

The URL acquisition profile accepts the HLS, static DASH and MP4 profiles in the
[input table](conversion-support.md#url-input). DASH needs one Period,
`SegmentTemplate` and an explicit finite `SegmentTimeline`. Acquisition retains
ciphertext; existing local admission uses the supplied source keys.
Relative key identifiers are resolved against the effective playlist URL and
matched to the protected key configuration without retrieving key URLs. Live
input, byte ranges and other DASH addressing profiles are not acquired. See
[fetch settings](usage.md#optional-fetch-settings) for explicit additional
origins, authentication, and transfer limits; the same options apply to `analyze`.

## 2. Save one fixed conversion

```sh
drmr plan ./source/input.m3u8 --to cbcs --format dash --output ./result --save ./conversion.plan.json --json
```

The saved plan has `kind: "execution-plan"` and `schema_version: 5`. It records
one output format and protection scheme, the new output location, and a SHA-256
fingerprint and size for every referenced source file. `required_depth` is
`native` by default; `--decode` saves `decoded`. This minimum is included in
the plan ID and cannot be lowered at execution. The source is fully
admitted again before saving. The plan itself is created privately and never
overwrites an existing file. Keep the plan and result outside the saved source
directory. For a single MP4, sibling plan and output files are allowed.

Add `--processing segments` to preserve original boundaries in a supported
matching-container conversion, or `--processing stitch` to prepare complete tracks
and package the requested output. HLS/DASH output from `stitch` is segmented again.
The default is `auto`. The plan's `processing` selection records `requested`,
`resolved` and `boundaries`; the choice is included in its ID. Explicit unsupported
modes are refused. See [processing modes](usage.md#select-a-processing-mode).

On Unix, use a trusted output and plan parent owned by your user with no group
or other-user write permission. Protected shared temporary directories have a
separate supported rule. See [workspace permissions](security.md#temporary-files-and-cleanup).

Use `--to clear` for unencrypted output. Other supported combinations include
`cenc` DASH, `cbcs` fragmented MP4 HLS, and `hls-aes128` or `hls-sample-aes` with
`--format hls-ts` for qualified TS sources. `--format hls` always means fragmented
MP4 HLS, including when the input uses TS. A saved plan accepts one output format;
create a separate plan and destination for another output.

Without `--save`, `plan INPUT --to ... --json` still describes conditional intent
from metadata and can describe multiple formats. That older JSON is not an
execution plan.

## 3. Execute the saved plan

Prepare a private [operator key file](usage.md#separate-input-and-output-keys)
with the appropriate input and output key roles, then run:

```sh
drmr convert --plan ./conversion.plan.json --keys-file ./keys.json --json
```

For clear input and output, omit `--keys-file`. The command reads the retained
local source; the original server is no longer required. It checks the bytes
actually copied into its private working area against the saved fingerprints
before media work starts. It admits the source again and checks the retained
originals immediately before publishing verified output. Changed or missing
source files, unsupported conditions, existing output, and mismatched keys fail
without replacing the destination. Only new working files are cleaned up; the
retained originals remain available.

The plan fixes media intent and paths. Key values, initialization vectors, key
addresses, DRM signaling and license addresses, request headers, signed source
URLs, and external-tool commands are not serialized. Keys and optional signaling
are supplied at execution time. A plan therefore does not pin a particular key
identifier, key address, or license endpoint. Optional signaling must still meet
its separately documented input and output profile. `--packager-bin`,
`--max-workers`, `--json`, and `--measure` remain runtime options. `--decode`
can strengthen a saved minimum. Legacy single version-one and named-output
version-two plans keep a decoded minimum. Version-three and version-four plans
retain their saved depth and original complete-track packaging behavior. Input,
target, format, processing, output, and download overrides are refused with `--plan`.

## 4. Verify independently

```sh
drmr verify --plan ./conversion.plan.json --keys-file ./keys.json --json
```

Verification opens the current source and result bytes independently. It checks
the saved original fingerprints, actual result container and protection on every
track, and complete media correspondence. Success requires
`source_unchanged: true`, `output_matches_plan: true`, and
`verification.passed: true`. Replacing CBCS output with matching clear media
fails the output-intent check even if the decoded content still matches. A
conversion's earlier success report does not authorize this verification.
Native checks compare all admitted samples and validated presentation metadata;
`--decode` additionally compares complete decoded content. New reports explicitly
distinguish an unrequested decode from actual decoder evidence. See
[verification depth](verification.md) for the report and plan schema changes.

The plan contains absolute local paths and is intended for reuse on the same
machine. Moving the original or result requires creating a new plan. Treat plan
JSON as private local data because paths and file fingerprints can reveal asset
information. Its digest detects edits; it is not a digital signature or proof
of a trusted author. Do not execute a plan from an untrusted source. Unknown
fields, unsupported versions, invalid fingerprints, unsafe source references,
and plan edits are refused. See [local security](security.md) for protected
configuration, process arguments, and private temporary-file boundaries.
