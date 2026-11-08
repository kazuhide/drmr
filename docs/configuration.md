# Conversion settings: outputs.json and keys.json

For ordinary conversion, prepare an output list and, when encryption is involved,
a private key file. Then run `convert` once. It checks the source, creates the
outputs, compares their media with the original, and checks the final manifests
before publishing the result. A separate `analyze`, saved plan or `verify` is
optional; use those operations when you need preflight inspection, fixed source
fingerprints or a fresh comparison later.

| File | Prepared by | Purpose | Contains secrets |
| --- | --- | --- | --- |
| `outputs.json` | You | Output IDs, containers, encryption and DRM selections | No content keys |
| `keys.json` | You | Input decryption keys and keys/DRM settings for named outputs | Yes; keep private |
| `conversion.plan.json` | `plan --save` | Fixed source fingerprints, destination and conversion conditions | No keys; source paths can still be private |

`plan` reads `outputs.json`; it does not generate it. File names are examples.
The three file types have independent schema versions. Use output-configuration
schema 4 and key-file schema 2. Quick single-output commands use the same key
format and select `outputs.default`; configured outputs select their explicit IDs.
See the [1.2 migration](releases.md#version-12-migration) for replaced names.

## Editor validation and templates

The public [output-configuration schema](../schemas/output-config-v4.schema.json)
and [named-key schema](../schemas/named-keys-v2.schema.json) use JSON Schema
Draft 7. An editor that supports JSON Schema can suggest fields and check required
values, types, enums, unknown fields, counts and supported output/DRM combinations
while you edit. These schemas add no dependency to the drmr CLI.

Version 1.3.0 accepts the optional `$schema` annotation. For older archives,
remove this member from the downloaded templates and associate the schemas
through your editor settings.

Choose a complete pair below and save the files as `outputs.json` and `keys.json`.
The files contain public dummy keys and assume clear input. For encrypted input,
add the appropriate [source-key mapping](#keysjson) or start from an existing
[input-key example](examples/README.md). Apply the private-file permissions
described below before using a real key file.

| Template | Output list | Key file | Selected outputs |
| --- | --- | --- | --- |
| Clear | [Download outputs.json](https://kazuhide.github.io/drmr/examples/templates/clear-outputs.json) | [Download keys.json](https://kazuhide.github.io/drmr/examples/templates/clear-keys.json) | Clear fMP4 HLS and clear DASH |
| Encrypted | [Download outputs.json](https://kazuhide.github.io/drmr/examples/templates/encrypted-outputs.json) | [Download keys.json](https://kazuhide.github.io/drmr/examples/templates/encrypted-keys.json) | CBCS fMP4 HLS with FairPlay; CBCS DASH with Widevine and PlayReady |
| Mixed HLS + DASH | [Download outputs.json](https://kazuhide.github.io/drmr/examples/templates/mixed4-outputs.json) | [Download keys.json](https://kazuhide.github.io/drmr/examples/templates/mixed4-keys.json) | Four HLS outputs: clear/FairPlay fMP4 and clear/FairPlay MPEG-TS; plus encrypted DASH |

For clear input and the Clear template, omit `--keys-file`; its empty key file is
an optional starting point for adding encrypted input later. The Encrypted and
Mixed templates need their matching `--keys-file keys.json`. Replace every dummy
output key, KID, URI and PSSH with values issued for your content. The templates
select native checks; optional external manifest tools are not required by these
settings. Processing remains `auto`, subject to the supported media routes.

Each template includes an optional top-level `$schema` reference to its public
schema. To associate a hand-written file, use the same reference, or configure a
local schema mapping in your editor. drmr accepts a nonempty `$schema` string of
up to 4096 printable ASCII characters without spaces. It discards this annotation;
it never retrieves the referenced resource, retains it in a saved plan or repeats
it in diagnostics. Other unknown fields are still refused. Your editor controls
its own schema downloads and extensions; follow your local policy when editing
private keys.

Schema validation does not establish that the two files match. drmr still checks
unique output IDs and source-key identifiers, key roles, complete PSSH contents,
DRM/KID/scheme correspondence, full URL syntax, the 64 KiB file limit and UTF-8
byte limits. File ownership, permissions, links and platform destination paths
also require runtime checks. Neither schema validation nor a valid configuration
qualifies the source media, proves license-service/device playback or installs
the requested external validators. Run the normal `plan` or `convert` workflow
for the corresponding runtime checks.

## Correcting a configuration error

`analyze`, `plan`, `convert` and `verify` validate a supplied key file before
processing media. `plan` and direct `convert` also validate the output list.
Invalid files keep the existing `key-file-invalid` or `output-spec-invalid`
error code and exit status `2`.

For example, a boolean instead of a key string produces guidance like this:

```text
keys configuration at input.keys[0].key (line 5, column 18): Use a string of exactly 32 hexadecimal characters (16 bytes).
```

Array positions start at zero: `keys[0]` is the first key. Fix the named field in
your local file; drmr does not print its supplied value. `--json` includes the
same guidance in `error.hint` and adds structured detail, for example:

```json
{
  "code": "key-file-invalid",
  "configuration": {
    "schema_version": 1,
    "file": "keys",
    "path": ["input", "keys", 0, "key"],
    "expected": "hex-16-bytes",
    "line": 5,
    "column": 18
  }
}
```

This is an excerpt from the `error` object, not a complete command result.
Line and column are included for JSON decoding errors. A value that decodes
successfully but fails validation may have a field position without a line or
column. Unsupported combinations can point to the containing object rather than
a single field. File-access failures and the 64 KiB file limit point to `root`.

Diagnostics contain only known schema fields, array positions and fixed
guidance. For named key domains, `outputs.*.key` points to a key in an output
entry; `*` deliberately replaces the operator's output ID. An unknown field name
is omitted, and the diagnostic points to the containing object. The original
file path, supplied IDs, key material, URIs and raw parser messages are never
included. Web and Desktop use the same safe fields. See
[local security](security.md) for memory and process-argument handling.

## Before running an example

Install the [required tools](requirements.md) for the selected conversion and
check the [supported input conditions](conversion-support.md). Examples use
nonfunctional `example.com` URLs and public dummy keys. Replace them with your
source and issued key material; these values do not provide a DRM license
service or establish playback-device compatibility.

Create key files in an owner-only working directory outside version control and
outside the input/output trees. On macOS/Linux, restrict `keys.json` to its owner
with `chmod 600 keys.json`; use the [Windows ACL instructions](windows.md) on
Windows. drmr also checks the file type, ownership and links. Read
[keys and local security](security.md) for process arguments, memory, temporary
files and published DRM metadata.

URL conversion needs a new `--download-dir` and a separate new result directory under
existing parent directories. Neither destination is overwritten or silently
reused. A completed download remains available if conversion fails. For a local
source, replace the URL with its entrypoint and omit `--download-dir`. Additional
origins or authentication use an optional private
[fetch-policy file](usage.md#optional-fetch-settings).

## outputs.json

This complete example requests one CBCS fMP4 HLS output for FairPlay:

```json
{
  "schema_version": 4,
  "output_dir": "result",
  "outputs": [
    {
      "id": "hls",
      "drms": [
        "fairplay"
      ],
      "output_format": "hls-fmp4",
      "encryption": "cbcs"
    }
  ]
}
```

The list accepts one to eight outputs, including a list with just one output.
Each ID becomes a result subdirectory. Required fields have no implicit default.

| Field | Required | Values and behavior |
| --- | --- | --- |
| `schema_version` | Yes | Use `4` for all fields below |
| `output_dir` | No | Result root, relative to this JSON file; otherwise supply `--output-dir` |
| `outputs` | Yes | Array of one to eight output objects |
| `outputs[].id` | Yes | Unique, 1–32 ASCII characters: lowercase letter first, then lowercase letters, digits or hyphens; reserved device names are refused |
| `outputs[].output_format` | Yes | `mp4`, `dash`, `hls-fmp4`, or `hls-ts` |
| `outputs[].encryption` | Yes | `clear`, `cenc`, `cbcs`, `hls-aes128` or `hls-sample-aes`, subject to the table below |
| `outputs[].drms` | Yes | Explicit DRM names: `widevine`, `playready`, `fairplay`; use `[]` for no service-specific declarations |
| `outputs[].processing` | No | `auto`, `segments` or `stitch`; otherwise uses the command's `--processing`, whose default is `auto` |
| `outputs[].dash_addressing` | No | DASH only: `template`, `template-timeline`, `list` or `base`; otherwise uses `--dash-addressing`, whose default is `template` |
| `outputs[].external_manifest_check` | No | `false` by default; `true` requires external final-manifest checks and is refused for MP4 |
| `outputs[].manifest_profile` | No | Native manifest rules: `base` by default, or HLS-only `apple-vod`; HLS/DASH outputs only |

Explicit CLI `--external-manifest-check` and `--manifest-profile` settings
override every configured output and refuse incompatible formats. Otherwise each
output uses its own settings. Per-output `processing` continues to override the
CLI's processing default.

An explicit CLI `--output-dir` overrides this file's `output_dir`. CLI relative
paths use the current working directory; JSON relative paths use the directory
containing `outputs.json`. A saved plan records the resolved destination and
refuses an execution-time override. `--output-file` instead names the MP4 file
for a quick single-output command and cannot be combined with `--output-config`.

| Output format | Encryption targets | Nonempty `drms` selection |
| --- | --- | --- |
| `mp4` | `clear`, `cenc`, `cbcs` | Not available through this output-list profile; use `[]` |
| `dash` | `clear`, `cenc`, `cbcs` | With `cenc`/`cbcs`: Widevine, PlayReady, or both |
| `hls-fmp4` | `clear`, `cenc`, `cbcs`, `hls-aes128` | With `cenc`: Widevine/PlayReady; with `cbcs`: any nonempty selection of Widevine/PlayReady/FairPlay |
| `hls-ts` | `clear`, `hls-aes128`, `hls-sample-aes` | With `hls-sample-aes`: FairPlay |

`drms: []` does not disable encryption or native checks. It requests encryption
without service-specific DRM information. In that case, encrypted HLS still
needs the key URI described below. FairPlay uses CBCS fMP4 HLS or SAMPLE-AES TS;
it cannot be added to CENC HLS or DASH. See [DRM signaling](drm-signaling.md).

Native media comparison and HLS/DASH linting run with either external-check
setting. Use `true` only when you have installed the optional
[external manifest validators](manifest-validation.md). Missing or incomplete
required external checks fail the operation rather than silently being skipped.

`segments` retains the original segment membership and clocks for supported,
matching containers. `stitch` joins when needed and segments HLS/DASH output
again. `auto` selects a qualified route. See [processing and segment timing](media-processing.md#generic-conversion-and-timing-limits)
and [DASH reference forms](dash-addressing.md). No output setting widens the
supported source codecs, track layouts or playback windows.

## keys.json

For the FairPlay example above and an unencrypted source, save this complete file:

```json
{
  "schema_version": 2,
  "outputs": {
    "hls": {
      "kid": "00000000000000000000000000000001",
      "key": "00112233445566778899aabbccddeeff",
      "hls_key_uri": "skd://example",
      "signaling": {
        "system": "multi-drm",
        "fairplay": true
      }
    }
  }
}
```

`outputs[].id: "hls"` selects `keys.outputs.hls`. Provide exactly one key entry
for every encrypted output and no entries for clear outputs. Clear input omits
`input`; clear output omits its named key entry. When both sides are clear,
omit the entire key file and `--keys-file` option.

| Field | Required | Meaning |
| --- | --- | --- |
| `schema_version` | Yes | `2` for every CLI key file |
| `input` | For encrypted input | Shared source-key mappings; omit for clear input |
| `input.keys` | For CENC/CBCS input | Array of `{ "kid": "…", "key": "…" }` for every required source KID |
| `input.hls_keys` | For AES-128 or SAMPLE-AES TS input | Array of `{ "uri": "…", "key": "…" }`, with associated `iv` for SAMPLE-AES when needed |
| `outputs` | Yes when using this file | Object keyed by encrypted output IDs; use `{}` for encrypted input with only clear outputs |
| `outputs.ID.key` | For every encrypted output | 16-byte content key written as 32 hexadecimal characters |
| `outputs.ID.kid` | For `cenc`/`cbcs` output | 16-byte key identifier, also written as 32 hexadecimal characters; omit for AES-128 or SAMPLE-AES TS |
| `outputs.ID.hls_key_uri` | For identity HLS, AES-128 HLS and FairPlay | Value to write into playlists, not a location drmr uploads to or fetches from; FairPlay requires `skd://` |
| `outputs.ID.iv` | For SAMPLE-AES TS output | Explicit associated 16-byte IV; refused for other output targets |
| `outputs.ID.signaling` | For nonempty `drms` | Matching `multi-drm` settings; omit when `drms` is empty |

KID means key identifier: it selects a key but is not the secret key itself.
IV means initialization vector: it supplies an encryption starting state.
Source and output keys are separate roles, even when the same test values are
used. Input keys must match the original content. Output KIDs and keys must
match the DRM initialization information issued for that output.

| `signaling` field | When used |
| --- | --- |
| `system` | Set to `"multi-drm"` for explicit named-output DRM settings |
| `widevine_pssh` | Required for Widevine: one complete PSSH box in hexadecimal or Base64, with matching KIDs and, if declared, encryption scheme |
| `playready_license_url` | Required for PlayReady; drmr generates the corresponding PlayReady records |
| `fairplay` | Set `true` for FairPlay; omitted means `false` |

PSSH means Protection System Specific Header, initialization data for a DRM
client. Raw Widevine payload bytes without their PSSH box are refused. HLS with
only Widevine/PlayReady uses embedded DRM declarations and omits
`hls_key_uri`; FairPlay needs its `skd://` identifier. drmr never fetches the
advertised key URI or license URL. Those values may be written to output
manifests and must not contain private authentication tokens.

Direct conversion checks key roles, IV rules, and the key identifiers and
encryption scheme in DRM initialization data before acquiring the source.
It completes format-specific checks after determining the output format.
Saving a plan does not require output keys.

### Clear source to encrypted HLS

Use the two complete files above and run:

```sh
drmr convert "https://example.com/vod/master.m3u8" \
  --download-dir source \
  --output-config outputs.json --keys-file keys.json
```

The generated entrypoint is `result/hls/master.m3u8`. URL acquisition, source
checks, conversion, media comparison and native manifest checks are part of this
one operation. Add `--decode` only when complete decoding is also required.

### Encrypted source to clear DASH

For CENC/CBCS input with one KID, `outputs.json` is:

```json
{
  "schema_version": 4,
  "outputs": [
    {
      "id": "dash",
      "drms": [],
      "output_format": "dash",
      "encryption": "clear"
    }
  ]
}
```

`keys.json` contains only source keys and the required empty output object:

```json
{
  "schema_version": 2,
  "input": {
    "keys": [
      {"kid": "00000000000000000000000000000001", "key": "00112233445566778899aabbccddeeff"}
    ]
  },
  "outputs": {}
}
```

```sh
drmr convert "https://example.com/vod/manifest.mpd" \
  --download-dir source \
  --output-config outputs.json --keys-file keys.json \
  --output-dir result
```

The entrypoint is `result/dash/manifest.mpd`. Supply every source KID if audio
and video use different keys. For AES-128 HLS input, the complete key file is
instead:

```json
{
  "schema_version": 2,
  "input": {
    "hls_keys": [
      {"uri": "https://example.com/vod/content.key", "key": "00112233445566778899aabbccddeeff"}
    ]
  },
  "outputs": {}
}
```

For SAMPLE-AES TS input, use its exact key identifier and associated IV:

```json
{
  "schema_version": 2,
  "input": {
    "hls_keys": [
      {"uri": "skd://source-example", "key": "00112233445566778899aabbccddeeff", "iv": "00000000000000000000000000000002"}
    ]
  },
  "outputs": {}
}
```

For these HLS sources, change the command's input URL to the actual `.m3u8`.
URI mappings must match the absolute key identifier resolved against the final
playlist URL after redirects. If both the playlist and key file declare an IV,
they must agree. AES-128 can use a playlist IV or the standard sequence-derived
IV; SAMPLE-AES does not substitute the media sequence number for its associated
IV. Key rotation needs a mapping for every key used by the source.

### Clear source to clear MP4

No `keys.json` is needed. Save this `outputs.json`:

```json
{
  "schema_version": 4,
  "outputs": [
    {
      "id": "mp4",
      "drms": [],
      "output_format": "mp4",
      "encryption": "clear"
    }
  ]
}
```

```sh
drmr convert "https://example.com/vod/media.mp4" \
  --download-dir source --output-config outputs.json --output-dir result
```

The entrypoint is `result/mp4/media.mp4`. The output-list interface always creates
a result root with named subdirectories, even for one MP4 output. Supported TS
HLS can also produce one MP4 after selecting at most one video and one associated
audio rendition. See [TS track conditions](conversion-support.md#mpeg-transport-streams-to-one-mp4).
Version 1.3.0 adds a qualified video/AAC pair from the same DASH/fMP4 HLS
presentation to this MP4 output configuration. Its
[metadata and track conditions](conversion-support.md#fragmented-presentations-to-one-mp4)
are required.

## Several outputs and saved plans

The same file structures support up to eight outputs with independent keys and
DRM selections. See the [complete DASH and HLS example](multiple-outputs.md),
including a matching dummy Widevine PSSH.

If you need to review conditions before execution or repeat a fresh independent
check later, use the [saved-plan workflow](executable-plans.md). `plan --save`
reads the output list and binds the source fingerprints and destination.
`convert --plan-file` and `verify --plan-file` read that generated plan and take keys
separately. Do not hand-edit it as an alternative to `outputs.json`.

For a fresh comparison without a saved plan, run:

```sh
drmr verify result --source source/master.m3u8 \
  --output-config outputs.json --keys-file keys.json
```

The positional `result` selects the existing result root; the JSON describes its
expected outputs. Quick single-result verification uses
`verify OUTPUT --source INPUT` and the `default` key domain. All modes use
schema-2 keys. `verify OUTPUT --manifest-only` needs no key file and checks
manifests without media correspondence.

## Compatibility and rejected settings

New output configurations require schema 4; replace the former `format`,
`target` and `conformance` fields as described in the
[migration guide](releases.md#version-12-migration). Key files use schema 2,
including quick single-output commands. Existing saved plans remain readable
and retain their recorded processing, addressing and minimum verification depth.
See [saved plans](executable-plans.md) and [verification depth](verification.md).

Unknown fields, duplicate fields, incompatible formats/DRM selections, repeated
IDs and extra or missing encrypted-output keys are refused. Each configuration
file is limited to 64 KiB. Content matching and supported-source conditions are
checked again when the settings are used; valid JSON alone does not establish a
successful conversion.
