# DRM signaling

Encryption protects media samples. DRM signaling tells a player which license system
and key identifier to use. The common `convert` command can generate and check both
the encrypted media and the corresponding DASH or HLS manifests.

The default `auto` and explicit `stitch` routes accept completed video-on-demand **clear HLS with MPEG-TS segments**
and the existing qualified **clear MP4** profile: H.264 video with optional AAC
audio. It does not accept existing DRM records or unknown
MP4 initialization metadata for replacement. It does not contact a license server.

With `--processing segments`, supported original fMP4 resources can instead
produce fMP4 DASH or HLS with the requested DRM records. Supported TS resources
can produce TS HLS with FairPlay declarations. These paths accept their
qualified clear or encrypted source profiles with separately supplied input
keys. They preserve segment membership and replace the protection records in
new output files; they never modify the originals. Container changes still use
`stitch`. The supported codecs, playback windows and record checks remain
mandatory; recognizing an input DRM system alone does not qualify its media.

## Supported outputs

| Output | Encryption | Explicit DRM selection |
| --- | --- | --- |
| DASH with fragmented MP4 | `cbcs` | Widevine, PlayReady, or both |
| HLS with fragmented MP4 | `cbcs` | FairPlay |
| HLS with MPEG-TS | `hls-sample-aes` | FairPlay |

`cbcs` uses AES cipher block chaining with a defined encryption pattern inside
media samples. HLS `SAMPLE-AES` protects codec samples in MPEG-TS; its signaling is
different from the fragmented-MP4 representation.

MPEG-TS output requires an admitted MPEG-TS source with original transport timing
evidence. The qualified clear MP4 profile supports the fragmented-MP4 outputs.
Other conversions remain available without these explicit DRM settings; see the
[conversion support table](conversion-support.md).

The single-output options below create one output directory. For HLS and DASH
from one source in one operation, use [named outputs](multiple-outputs.md) with
the common `plan`, `convert`, and `verify` commands. Those outputs are independently
packaged; physical sharing of their fragmented-MP4 files is not guaranteed.

## Supply private output settings

Use an owner-protected schema-version-1 key file, as described in
[security](security.md). The examples below show the `output` object inside:

```json
{
  "schema_version": 1,
  "output": {}
}
```

Replace the empty object with the settings for the chosen output. Replace all
placeholders before running a command. Encryption keys and initialization vectors
are 16 bytes, written as 32 hexadecimal characters. A key identifier names the key;
it is not the encryption key itself.

### DASH: Widevine and PlayReady

```json
{
  "kid": "<32 hexadecimal characters>",
  "key": "<32 hexadecimal characters>",
  "signaling": {
    "system": "multi-drm",
    "widevine_pssh": "<complete Widevine PSSH box, hexadecimal or base64>",
    "playready_license_url": "https://license.example.invalid/playready"
  }
}
```

Protection System Specific Header (PSSH) boxes carry initialization data for a DRM
system. Widevine data must be a complete version-0 or version-1 box with the
Widevine system identifier, valid lengths, and at least one 16-byte key identifier.
Every identifier must match `kid`. If the payload declares an encryption scheme,
it must declare `cbcs`. Raw payloads without the PSSH box are not accepted here.

The supplied Widevine box is preserved byte for byte. PlayReady version-4.3 records
are generated from `kid` and the license URL. The same generated PSSH and PlayReady
Object (PRO) data are used in initialization files and the MPD. These operations do
not generate provider-specific Widevine policy data or validate license responses.

On `stitch` routes, explicit DASH PSSH data is passed to Shaka Packager through its
[`--flagfile` facility](https://github.com/shaka-project/shaka-packager/blob/v3.8.0/packager/app/packager_main.cc),
using a separate owner-protected temporary file. This avoids command-line size
limits for large metadata. The file is checked before and after packaging, is
kept until child-process cleanup is confirmed, and is removed with its private
workspace. Uncertain filesystem entries are retained if the workspace is replaced.
The file is never included in the output directory. The temporary directory
path must not contain a comma, because the flag-file option treats commas as
file-name separators; such a path is refused before packaging.
Native `segments` routes write the validated records directly to new MP4
initialization files and the MPD, then reopen and check that output graph.

Omit `widevine_pssh` or `playready_license_url` to select only one system. At least
one must remain. Do not select FairPlay for the DASH output.

```sh
drmr convert ./source/media.m3u8 --to cbcs --format dash \
  --output ./dash --keys-file ./dash.keys.json
```

### HLS with fragmented MP4: FairPlay

```json
{
  "kid": "<32 hexadecimal characters>",
  "key": "<32 hexadecimal characters>",
  "hls_key_uri": "skd://content-key",
  "signaling": {
    "system": "multi-drm",
    "fairplay": true
  }
}
```

The `skd://` address identifies the FairPlay content key. It is written into the
playlists and is never fetched by drmr. Supply the matching key to the license
service separately. Do not add `iv` to this configuration: fragmented MP4 stores
the sample-encryption initialization vector in its media initialization data.

```sh
drmr convert ./source/media.m3u8 --to cbcs --format hls \
  --output ./fairplay-fmp4 --keys-file ./fairplay-fmp4.keys.json
```

### HLS with MPEG-TS: FairPlay

```json
{
  "key": "<32 hexadecimal characters>",
  "iv": "<32 hexadecimal characters>",
  "hls_key_uri": "skd://content-key",
  "signaling": {
    "system": "multi-drm",
    "fairplay": true
  }
}
```

The explicit initialization vector (`iv`) is required for this output-key domain.
It must agree with the value supplied by the FairPlay license service. MPEG-TS
does not use an MP4 key identifier, so omit `kid`.

```sh
drmr convert ./source/media.m3u8 --to hls-sample-aes --format hls-ts \
  --output ./fairplay-ts --keys-file ./fairplay-ts.keys.json
```

FairPlay playlists may omit the `IV` attribute. drmr retains the operator's
associated vector for independent MPEG-TS decryption. An explicit playlist vector
must match it. Omission does not mean that the encryption uses no vector or that
the license service has been checked.

## What is checked

Conversion checks the final output before publication. DASH checks include the
actual referenced initialization and media files, the exact selected PSSH set, key
identifiers, encryption scheme, XML namespaces, and generated PlayReady PRO/PSSH
agreement. HLS checks every referenced media playlist and its FairPlay method,
key address, key-format declaration, initialization metadata, and declared vector
when present. A DASH media resource may repeat a selected provider's exact
initialization PSSH once per provider. Different records, additional systems,
and duplicate records are rejected. FairPlay fragmented-MP4 HLS has no PSSH
records in either its initialization or media files. These checks read bounded
metadata and skip media-data payloads; they do not load an entire segment into memory.

FairPlay media playlists require one valid `EXT-X-VERSION` declaration: at least
6 for ordinary fragmented-MP4 HLS using `EXT-X-MAP`, or at least 5 for MPEG-TS.
Higher compatible values are accepted. Missing, duplicate, malformed, and lower
versions are rejected. An unspecified audio language is omitted from the HLS
`LANGUAGE` attribute; a supplied language such as `en` is retained.

These signaling checks accompany complete encoded-content, timing, decoded-content,
and supported selection-metadata comparisons. They do not replace those checks.

CENC or CBCS output without selected provider signaling can still contain the
[W3C Common SystemID initialization record](https://www.w3.org/TR/eme-initdata-cenc/).
This record identifies encryption keys by identifier; it does not contain the
content keys or provider-specific policy data. drmr accepts only its generated
single-key form here: version 1, zero flags, the selected output KID and an empty
data field. The MPD, initialization files and actual track protection must agree.
Common records are accepted in initialization files declaring that selected track;
they are not accepted in a separate media fragment without that track declaration.
This output check does not relax source admission or authorize arbitrary provider
records. It does not mark service-specific signaling or license playback as verified.

Library callers can independently reopen an existing result with
`Converter::verify_output_signaling(input, format, target, &keys)`. This method
reads a fresh, byte-bound output graph and verifies its current records against
the operator settings. Call `Converter::verify` separately for full source/result
media comparison. A prior conversion report does not authorize either check.
The single-output CLI `verify` repeats these signaling checks for current saved
plans, including when no provider is selected. Older saved single-output plans
retain their original behavior when no signaling profile is supplied. It checks the same byte-bound
output as its complete media comparison. A [multiple-output saved plan](multiple-outputs.md)
also fixes the selected DRM systems and reports each output's media, DRM,
native manifest linting, and optional external manifest checks separately.

Whole-file AES-128 encryption can make resource bytes opaque. The native metadata
check examines the final manifest and readable initialization and provider records;
the standalone signaling method cannot prove that provider data is absent inside
ciphertext. The `segments` conversion and media-verification routes also check
the actual decrypted initialization and media resources and retain guards for
both their plaintext and the original ciphertext. Complete source/result media
comparison remains a separate check.

The existing `{"system":"playready","license_url":"..."}` setting retains its
qualified clear-MP4-to-CBCS-DASH profile. It is not silently expanded to other inputs.

Successful verification does **not** establish license acquisition, a content
decryption module (CDM) session, playback on a particular device, or full standards
conformance. Use the relevant provider and device tests for those claims.

Keys, vectors, license addresses, PSSH data, and FairPlay addresses are excluded
from diagnostic output. Addresses and initialization data intentionally appear in
the generated media and manifests where the delivery format requires them.

The playlist rules above follow
[RFC 8216 section 4.3.2.4](https://www.rfc-editor.org/rfc/rfc8216.html#section-4.3.2.4)
and [protocol version compatibility](https://www.rfc-editor.org/rfc/rfc8216.html#section-7).
See also the official
[Widevine PSSH payload definition](https://github.com/shaka-project/shaka-packager/blob/main/packager/media/base/widevine_pssh_data.proto).
