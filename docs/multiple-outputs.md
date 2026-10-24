# Plan, convert, and verify several outputs

Use an output list when one source needs several delivery formats or different
digital rights management (DRM) systems. For example, a supported clear HLS
transport-stream presentation can produce:

- fragmented MP4 DASH encrypted with `cbcs`, with Widevine and PlayReady information;
- fragmented MP4 HLS encrypted with `cbcs`, with FairPlay information;
- transport-stream HLS encrypted with `SAMPLE-AES`, with FairPlay information.

Choose the outputs you need. The common `plan`, `convert`, and `verify` commands
name every output explicitly. Native checks cover every selected sample, configuration, clock and supported playback window. Add `--decode` at saved planning to require complete decoding for every later execution and verification, or at runtime to strengthen the saved minimum. New named-output plans use schema 6; legacy schema 2 plans retain their decoded minimum and schema 4 plans retain their saved depth. See [verification depth](verification.md).

## Describe the outputs

Save this as `outputs.json`:

```json
{
  "schema_version": 1,
  "outputs": [
    {
      "id": "dash",
      "format": "dash",
      "target": "cbcs",
      "drms": ["widevine", "playready"],
      "conformance": "required"
    },
    {
      "id": "hls",
      "format": "hls",
      "target": "cbcs",
      "drms": ["fairplay"],
      "conformance": "required"
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
  "drms": ["fairplay"],
  "conformance": "required"
}
```

`conformance: "required"` requests an external check of the final output
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
modes, use output-recipe schema 2 and set `processing` on individual outputs:

```json
{
  "schema_version": 2,
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
default. Recipe schema 1 remains supported and refuses the new `processing`
field. These recipe versions are separate from the private key-file versions.

## Keep runtime keys separate

The output list and saved plan contain no keys, license addresses, or DRM
initialization data. Use a protected, schema-version-2 key file for the shared
input keys and separate output settings. Its `outputs` names must match the
encrypted output IDs. Clear outputs have no entry. This template uses
placeholders and must be completed with the values issued for your content:

```json
{
  "schema_version": 2,
  "outputs": {
    "dash": {
      "kid": "<32 hexadecimal characters>",
      "key": "<32 hexadecimal characters>",
      "signaling": {
        "system": "multi-drm",
        "widevine_pssh": "<complete Widevine PSSH box, hexadecimal or base64>",
        "playready_license_url": "https://license.example.invalid/playready"
      }
    },
    "hls": {
      "kid": "<32 hexadecimal characters>",
      "key": "<32 hexadecimal characters>",
      "hls_key_uri": "skd://<content-identifier>",
      "signaling": {
        "system": "multi-drm",
        "fairplay": true
      }
    }
  }
}
```

PSSH means Protection System Specific Header: initialization information used by
a DRM client. Supply a complete Widevine PSSH box, rather than only its payload.
It must declare at least one 16-byte key identifier, and every identifier must
match that output's `kid`. An explicitly declared protection scheme must be
`cbcs`. drmr checks it and generates the PlayReady information
from the configured key identifier and license address. For HLS, it writes the
FairPlay key declaration using `hls_key_uri`. No license service or advertised key
address is contacted.

For the transport-stream output, use an `hls-ts` key entry instead:

```json
{
  "key": "<32 hexadecimal characters>",
  "iv": "<32 hexadecimal characters>",
  "hls_key_uri": "skd://<content-identifier>",
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

The `auto` and `stitch` multi-DRM profile admits supported clear HLS transport streams
and the qualified clear MP4 profile. Other source layouts or existing DRM
metadata are refused by this profile. Existing conversions without explicit DRM
information retain their [documented source profiles](conversion-support.md).
For `segments` outputs, supported clear or known-key encrypted TS can produce
TS FairPlay, while supported fMP4 can produce fMP4 HLS FairPlay or CBCS DASH
with Widevine/PlayReady. Source and output containers must match. See the
[DRM source conditions](drm-signaling.md).

## Analyze, save, execute, and verify

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

For the first example, the result contains:

```text
result/
  dash/manifest.mpd
  hls/master.m3u8
```

Each directory also contains its initialization files, media segments, and any
child playlists. An `hls-ts` output has its own transport-stream resources.
This implementation packages the requested outputs separately, including DASH
and fragmented MP4 HLS; it does not promise shared physical media files.

For one operation without saving a plan, use:

```sh
drmr convert ./source/input.m3u8 --outputs ./outputs.json --output ./result --keys-file ./keys.json --json
```

URL input uses the same output list and still requires `--download-dir`. Use a
saved plan when you want a later `verify --plan` to check the exact original
fingerprints and output selections.

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
