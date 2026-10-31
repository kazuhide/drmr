# Convert to several named outputs

Use an output list when one source needs several delivery formats or different
digital rights management (DRM) systems. For example, a supported clear HLS
transport-stream presentation can produce:

- fragmented MP4 DASH encrypted with `cbcs`, with Widevine and PlayReady information;
- fragmented MP4 HLS encrypted with `cbcs`, with FairPlay information;
- transport-stream HLS encrypted with `SAMPLE-AES`, with FairPlay information.

Prepare `outputs.json` and a private `keys.json`, then run `convert` once. The
same format works for one to eight outputs. See the [configuration reference](configuration.md)
for required fields, defaults, input-key variants and single-output examples.
The optional `plan --save` and `verify --plan` workflow is described below.
Native checks cover every selected sample, configuration, clock and supported
playback window. Add `--decode` for complete decoding as an additional condition.
See [verification depth](verification.md).

## Describe the outputs

Save this as `outputs.json`:

```json
{
  "schema_version": 3,
  "outputs": [
    {
      "id": "dash",
      "format": "dash",
      "target": "cbcs",
      "drms": ["widevine", "playready"]
    },
    {
      "id": "hls",
      "format": "hls",
      "target": "cbcs",
      "drms": ["widevine", "playready", "fairplay"]
    }
  ]
}
```

`format: "hls"` means fragmented MP4 HLS. For FairPlay with transport streams,
replace that output with:

```json
{
  "id": "hls-ts",
  "format": "hls-ts",
  "target": "hls-sample-aes",
  "drms": ["fairplay"]
}
```

These examples use native media and manifest checks by default. Add
`conformance: "required"` to request an external check of the final output
manifests. Install the [manifest validation tools](manifest-validation.md) first.
Use `"none"`, or omit this field, when you do not request those external checks.
Native manifest linting and complete internal media verification run in either case. A requested tool
that is missing, interrupted, or unable to produce a complete result causes
failure; the check is never silently skipped.

Each `id` names one subdirectory of the result. Use one to 32 lowercase ASCII
characters, starting with a letter and continuing with letters, digits, or
hyphens. Up to eight outputs are accepted. Duplicate IDs, unknown fields,
unsupported combinations, and arbitrary child paths are refused.

### Select processing for each output

`--processing` supplies one default for the output list. To choose different
modes, set `processing` on individual outputs in the schema-3 file:

```json
{
  "schema_version": 3,
  "outputs": [
    {"id":"hls-ts","format":"hls-ts","target":"hls-sample-aes","drms":["fairplay"],"processing":"segments"},
    {"id":"dash","format":"dash","target":"cbcs","drms":["widevine","playready"],"processing":"stitch"}
  ]
}
```

For a qualified clear TS source, this requests HLS TS at its original segment
boundaries and DASH fMP4 with new segmentation. `segments` preserves sample
membership and clocks; `stitch` joins when needed and packages the selected
format. `auto` selects a qualified route. Omitted per-output values use the CLI
default. Earlier recipe versions remain accepted: schema 1 has no per-output
processing field, while schemas 2 and 3 accept it. These recipe versions are
separate from the private key-file versions.

### Select DASH references for each output

Use recipe schema 3 to select `dash_addressing` on a DASH output:

```json
{
  "schema_version": 3,
  "outputs": [
    {"id":"dash","format":"dash","target":"cbcs","drms":["widevine","playready"],"processing":"stitch","dash_addressing":"template"},
    {"id":"hls","format":"hls","target":"cbcs","drms":["fairplay"],"processing":"stitch"}
  ]
}
```

The four choices are `template` (default), `template-timeline`, `list` and `base`.
The CLI's `--dash-addressing` supplies a default for DASH outputs; a value in the
recipe takes precedence. Other formats refuse this field, and recipes 1/2 refuse
it. Schema 3 also accepts per-output `processing`. See [DASH references](dash-addressing.md)
for the generated structure and timing conditions. Directory placement follows
the [source layout](source-layout.md); no layout selector is needed.

## Keep runtime keys separate

The output list and saved plan contain no keys, license addresses, or DRM
initialization data. Use a protected, schema-version-2 key file for the shared
input keys and separate output settings. Its `outputs` names must match the
encrypted output IDs. Clear outputs have no entry. The complete example below
uses public dummy values, including a complete Widevine PSSH with the matching
KID. Replace all of them with the values issued for your content. The two output
key domains are independent, even though these test values are identical:

```json
{
  "schema_version": 2,
  "outputs": {
    "dash": {
      "kid": "00000000000000000000000000000001",
      "key": "00112233445566778899aabbccddeeff",
      "signaling": {
        "system": "multi-drm",
        "widevine_pssh": "000000327073736800000000edef8ba979d64acea3c827dcd51d21ed00000012121000000000000000000000000000000001",
        "playready_license_url": "https://license.example.invalid/playready"
      }
    },
    "hls": {
      "kid": "00000000000000000000000000000001",
      "key": "00112233445566778899aabbccddeeff",
      "hls_key_uri": "skd://example",
      "signaling": {
        "system": "multi-drm",
        "widevine_pssh": "000000327073736800000000edef8ba979d64acea3c827dcd51d21ed00000012121000000000000000000000000000000001",
        "playready_license_url": "https://license.example.invalid/playready",
        "fairplay": true
      }
    }
  }
}
```

PSSH means Protection System Specific Header: initialization information used by
a DRM client. Supply a complete Widevine PSSH box, rather than only its payload.
It must declare at least one 16-byte key identifier, and every identifier must
match that output's `kid`. If the PSSH explicitly declares an encryption scheme,
it must match the selected target: `cbcs` here, or `cenc` for CTR output. drmr
checks it and generates the PlayReady information
from the configured key identifier and license address. For HLS, it writes the
FairPlay key declaration using `hls_key_uri`. No license service or advertised key
address is contacted.

For the transport-stream output, use an `hls-ts` key entry instead:

```json
{
  "key": "00112233445566778899aabbccddeeff",
  "iv": "00000000000000000000000000000002",
  "hls_key_uri": "skd://example",
  "signaling": {
    "system": "multi-drm",
    "fairplay": true
  }
}
```

`iv` is the initialization vector associated with that content key. The
transport-stream profile requires it explicitly and does not use an MP4 key
identifier. Output key domains are independent; choose each one explicitly.
For encrypted source media, the shared `input` object uses the same `keys` and
`hls_keys` fields as the [single-output key file](usage.md#separate-input-and-output-keys).

The `auto` and `stitch` multi-DRM profiles accept qualified clear or known-key
encrypted MP4, DASH and HLS sources, subject to their native codec, track,
playback-window and metadata conditions. For `segments`, fMP4 remains fMP4 for
DASH/HLS output, and TS remains TS for HLS output. Container changes use
`stitch`. See the [DRM source conditions](drm-signaling.md) and
[conversion limits](conversion-support.md); selecting a DRM name does not
qualify an otherwise unsupported source.

## Convert once

For clear input, the complete key file above has no `input` object. Protect the
file as described in the [configuration reference](configuration.md#before-running-an-example),
then run:

```sh
drmr convert "https://example.com/vod/master.m3u8" \
  --download-dir source \
  --outputs outputs.json --keys-file keys.json \
  --output result
```

The source is downloaded once and retained. Direct `convert` checks and plans the
source in memory, creates the outputs with bounded parallel workers, and
compares them with the original before publishing the result. No separate
`analyze`, `plan` or `verify` is required to obtain those checks. For clear HLS TS
with two or more `stitch` fMP4 HLS/DASH outputs, it also shares joining, track
separation and clear-media preparation. Each output receives its own working
copy for encryption. Input checking is not a persistent cache: a later command
reads and checks the current files again.

The generated entrypoints are:

```text
result/
  dash/manifest.mpd
  hls/master.m3u8
```

Each directory also contains its initialization files, media segments and child
playlists. An `hls-ts` output has its own TS resources; an `mp4` output named
`file` is `result/file/media.mp4`. DASH and HLS are packaged independently;
physical sharing of their media files is not guaranteed.

For an already downloaded or local source, replace the URL with its entrypoint
and omit `--download-dir`:

```sh
drmr convert source/input.m3u8 --outputs outputs.json \
  --keys-file keys.json --output result
```

Use a new result path. For encrypted input, add its shared source mappings to
`input` in the version-2 key file. Clear outputs have no named key entry. The
[complete key-file examples](configuration.md#keysjson) cover both cases.

## Analyze, save, execute, and verify

Use this alternative when the original fingerprints and conversion conditions
need to be saved before execution. New named-output plans use schema 10 and bind
source filenames, resource placement and DASH addressing. Older plans retain
their recorded minimum verification depth and processing behavior.

```sh
drmr analyze "https://example.com/vod/master.m3u8" --download-dir ./source --json
drmr plan ./source/input.m3u8 --outputs ./outputs.json --output ./result --save ./conversion.plan.json --json
```

The same acquisition command accepts a supported MP4 or static DASH URL; use
`source/input.mp4` or `source/input.mpd` for the plan. Acquisition does not
widen the chosen processing route's media profile. See
[URL input](conversion-support.md#url-input).

The output list describes the requested outputs. The saved execution plan fixes
the qualified original files, their sizes and SHA-256 fingerprints, the result
directory, every output format and protection scheme, the DRM system selections,
each external manifest-check policy, and the requested/resolved processing and
boundary behavior for every output. The root `processing` list associates each
output ID with its saved selection. It does not fix a particular key,
license address, or key address; those remain runtime settings.

After preparing the key file, run:

```sh
chmod 600 keys.json
drmr convert --plan ./conversion.plan.json --keys-file ./keys.json --json
drmr verify --plan ./conversion.plan.json --keys-file ./keys.json --json
```

For Windows, use the [protected-file instructions](windows.md) instead of
the Unix permission command. Both commands also accept `--hls-validator-bin` for
the Apple executable and `--dash-validator-bin` for the PHP executable used by
the local DASH-IF adapter. Runtime executable locations are not saved in a plan.

This optional workflow reads the source again at each command boundary. Use it
when saved conditions or a fresh independent comparison are useful, rather than
repeating it after every direct conversion. `verify` has no `--outputs` option;
named-output verification uses the generated plan and the matching version-2 key
file. `verify OUTPUT --source INPUT` is the separate direct interface and uses a
version-1 key file.

## Publication and verification

All outputs are built in a private working directory. The complete new result
directory is published only after every output passes media verification,
native manifest linting, the requested DRM checks, and any required external manifest checks. A failure or
cancellation does not publish a subset of the outputs. Existing destinations
are never replaced. Retained source files remain available.

Independent `verify --plan` reads the current original and result files again.
For each output, it reports media correspondence, actual format and protection,
DRM information, native lint coverage, and the external manifest result separately. Changed PSSH,
PlayReady data, key identifiers, or FairPlay declarations can fail even when the
media still decrypts. Required HLS protocol versions are minimum versions: a
normal fragmented MP4 playlist using `EXT-X-MAP` needs version 6 or later;
version 7 is also valid. The FairPlay transport-stream declaration requires
version 5 or later. An IV attribute may be omitted in FairPlay playlists.
For TS, full media verification uses the supplied content key and associated IV;
an explicit playlist IV must match that value. For fragmented MP4, the constant
IV is read from the initialization segment's `tenc` encryption information;
an explicit playlist IV must agree with it.

An empty `drms` list requests no service-specific DRM information. Its signaling
result is not marked verified; the ordinary media and protection checks still
apply. An unrequested external check is reported separately from a successful
one. Manifest checks do not establish license acquisition or playback on a
particular device.

Verification retains separate temporary copies of the outputs until their
checks finish. Allow disk space for the packaged outputs and these copies in
addition to the saved source. This bounds memory use and prevents later
file changes from being mistaken for the bytes that were checked.

To check just one existing output manifest, without reading keys or comparing
its media with an original, use:

```sh
drmr verify ./result/hls/master.m3u8 --manifest-only --json
drmr verify ./result/dash/manifest.mpd --manifest-only --json
```

These commands use the built-in Rust linter. Add `--external-validator` for
Apple or DASH-IF checks as well. These reports have `scope: "manifest-only"`
and `media_verified: false`.
Use ordinary `verify --plan` for the complete source-to-result proof.
