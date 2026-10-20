# Analyze, save a plan, convert, and verify

Use this workflow when you want to check the complete input first, review one
fixed conversion, and verify its result against the retained original. It
supports the [qualified local media profiles](conversion-support.md), including
supported completed HLS URLs within the documented acquisition profile.

For several outputs with separate DRM systems and final-manifest checks, use
the [multiple-output workflow](multiple-outputs.md). The single-output examples
below and their schema-version-1 plans remain supported.

## 1. Analyze and retain the input

```sh
drmr analyze "https://example.com/vod/master.m3u8" --download-dir ./source --json
```

`source` must be a new directory under an existing parent. The command downloads
the complete presentation, retains its original playlists and media, and creates
`source/input.m3u8` with local references. It checks all admitted tracks, source
decryption when needed, encoded content, complete decoding, and timing evidence.
This can take as much work as reading and decoding the whole asset; it is not a
quick check of the master playlist. A completed download survives a later media
admission failure so that you can examine it locally.

The JSON reports `input_admitted`, `input_checks`, the actual container in
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
markers. These notes do not waive packet, decoding, or timing checks. See
[transport-stream compatibility](media-processing.md#transport-stream-compatibility)
for the accepted conditions and handling of the unchanged original.

For a local input, use `drmr analyze original/manifest.mpd --json` or
`drmr analyze original.mp4 --json`. Supply `--keys-file` for encrypted local or URL input, including source checks when saving a plan.
Local input does not accept download options. `inspect` remains the faster,
metadata-only command and does not read or decode complete media.

The URL acquisition profile accepts the completed HLS container/encryption
combinations in the [input table](conversion-support.md#url-input). Acquisition
retains ciphertext; existing local admission uses the supplied source keys.
Relative key identifiers are resolved against the effective playlist URL and
matched to the protected key configuration without retrieving key URLs. It does
not acquire remote DASH, standalone MP4, live playlists, or byte ranges. See
[fetch settings](usage.md#optional-fetch-settings) for explicit additional
origins, authentication, and transfer limits; the same options apply to `analyze`.

## 2. Save one fixed conversion

```sh
drmr plan ./source/input.m3u8 --to cbcs --format dash --output ./result --save ./conversion.plan.json --json
```

The saved plan has `kind: "execution-plan"` and `schema_version: 1`. It records
one output format and protection scheme, the new output location, and a SHA-256
fingerprint and size for every referenced source file. The source is fully
admitted again before saving. The plan itself is created privately and never
overwrites an existing file. Keep the plan and result outside the saved source
directory. For a single MP4, sibling plan and output files are allowed.

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
execution plan. `plan --mpd` retains its prepared-DASH rendition listing.

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
`--max-workers`, `--json`, and `--measure` remain runtime options. Input, target,
format, output, and download overrides are refused with `--plan`.

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

The plan contains absolute local paths and is intended for reuse on the same
machine. Moving the original or result requires creating a new plan. Treat plan
JSON as private local data because paths and file fingerprints can reveal asset
information. Its digest detects edits; it is not a digital signature or proof
of a trusted author. Do not execute a plan from an untrusted source. Unknown
fields, unsupported versions, invalid fingerprints, unsafe source references,
and plan edits are refused. See [local security](security.md) for protected
configuration, process arguments, and private temporary-file boundaries.
