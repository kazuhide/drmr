# DRM signaling

Encryption protects media samples. DRM signaling tells a player which license system
and key identifier to use. The common `convert` command can generate and check both
the encrypted media and the corresponding DASH or HLS manifests.

The `multi-drm` configuration accepts supported VOD inputs qualified by the same
native media checks as ordinary conversion. Clear or known-key CENC/CBCS MP4,
fMP4 HLS and static DASH inputs can be processed with `auto` or `stitch`, including
qualified progressive MP4 audio playback ranges. Supported TS HLS inputs retain
their documented key and codec requirements. A separate bounded metadata census
still refuses unknown opaque boxes and unsupported controls. Outer AES-128 fMP4
HLS resources undergo this census on the actual Rust-decrypted initialization
and media, before optional probing, decoding or packaging. Decryption is reused
from the existing localization pass. The active encryption declaration applies
to each following resource; an unused AES-128 declaration does not exempt
plaintext resources from their metadata checks. With `--decode`, every rendition's
Rust localization finishes before any rendition starts probing or decoding.
Recognized static
ID3 metadata and supported descriptive text tags do not establish timed-track
support. No license service is contacted.

`stitch` prepares complete tracks and segments the new output again. With
`--processing segments`, supported original fMP4 resources instead produce fMP4
DASH or HLS while preserving segment membership. Supported TS resources can
produce TS HLS with FairPlay declarations. Both paths use separately supplied
input keys and replace protection records in new output files. They never modify
the originals. Container changes use `stitch`. The supported codecs, playback
windows and record checks remain mandatory; a recognized DRM name alone does
not qualify the media.

VP9 sample protection uses CENC under the [VP9 MP4 binding](https://www.webmproject.org/vp9/mp4/#common-encryption); VP9/CBCS is refused. HLS whole-resource AES-128 is separate. A DRM declaration does not establish codec support in a license service or device, and this path does not qualify FairPlay playback.

## Supported outputs

| Output | Encryption | Explicit DRM selection |
| --- | --- | --- |
| DASH with fragmented MP4 | `cenc`, `cbcs` | Widevine, PlayReady, or both |
| HLS with fragmented MP4 | `cbcs` | Any nonempty selection of Widevine, PlayReady and FairPlay |
| HLS with fragmented MP4 | `cenc` | Widevine, PlayReady, or both |
| HLS with MPEG-TS | `hls-sample-aes` | FairPlay |

`cenc` uses AES counter mode. `cbcs` uses AES cipher block chaining with a defined
encryption pattern inside media samples. HLS `SAMPLE-AES` protects codec samples in MPEG-TS; its signaling is
different from the fragmented-MP4 representation.

FairPlay MPEG-TS output also accepts qualified fragmented MP4 HLS, static DASH,
or a fragmented MP4 file, including encrypted input with supplied keys. It uses
`stitch` to prepare complete tracks and segment the TS output again. Every final
encoded frame, supported playback range and timestamp is independently compared
with the original. MP4 initial-audio edits that TS cannot represent are refused.
The qualified clear progressive MP4 profile supports the fragmented-MP4 outputs.
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
it must match the selected `cenc` or `cbcs` output. A box declaring the other scheme
is refused before packaging. Raw payloads without the PSSH box are not accepted here.

The supplied Widevine box is preserved byte for byte. PlayReady records use a
version-4.1 header with `ALGID="AESCTR"` and a content-key checksum for `cenc`, or
a version-4.3 header with `ALGID="AESCBC"` and no checksum for `cbcs`. The checksum
uses the key identifier in PlayReady GUID byte order, as defined by the
[PlayReady Header Specification](https://learn.microsoft.com/en-us/playready/specifications/playready-header-specification#5-key-checksum-algorithm).
The same generated PSSH and PlayReady Object (PRO) data are used in initialization
files and the MPD. These operations do
not generate provider-specific Widevine policy data or validate license responses.

On `stitch` routes, explicit provider PSSH data is passed to Shaka Packager through its
[`--flagfile` facility](https://github.com/shaka-project/shaka-packager/blob/v3.8.0/packager/app/packager_main.cc),
using a separate owner-protected temporary file. This avoids command-line size
limits for large metadata. The file is checked before and after packaging, is
kept until child-process cleanup is confirmed, and is removed with its private
workspace. Uncertain filesystem entries are retained if the workspace is replaced.
The file is never included in the output directory. The temporary directory
path must not contain a comma, because the flag-file option treats commas as
file-name separators; such a path is refused before packaging.
Native `segments` routes write the validated records directly to new MP4
initialization files and the final MPD or HLS declarations, then reopen and check that output graph.

Omit `widevine_pssh` or `playready_license_url` to select only one system. At least
one must remain. Do not select FairPlay for the DASH output.

```sh
drmr convert ./source/media.m3u8 --to cbcs --format dash \
  --output ./dash --keys-file ./dash.keys.json
```

Select `--to cenc` for counter-mode DASH with the same DRM selection. Use a
matching Widevine box if its payload specifies the encryption scheme:

```sh
drmr convert ./source/media.m3u8 --to cenc --format dash \
  --output ./dash-ctr --keys-file ./dash-ctr.keys.json
```

For a qualified encrypted MP4, fMP4 DASH or HLS source, supply the input keys
separately. Use `--processing segments` when preserving original fMP4 segment
membership; `stitch` also accepts qualified encrypted sources. Both replace the DRM initialization
records with the selected output records while independently comparing input samples
and presentation clocks. `segments` also preserves segment membership. For example, CENC DASH can be converted to
CBCS DASH with new Widevine and PlayReady records using
`--to cbcs --format dash --processing segments`. Output verification checks the selected scheme and key
identifier in the MPD and every referenced initialization file, as well as the
exact provider records. A valid PSSH alone does not prove a working DRM license.

### HLS fragmented MP4

CBCS HLS can declare FairPlay, Widevine and PlayReady together for the same key:

```json
{
  "kid": "<32 hexadecimal characters>",
  "key": "<32 hexadecimal characters>",
  "hls_key_uri": "skd://content-key",
  "signaling": {
    "system": "multi-drm",
    "widevine_pssh": "<complete Widevine PSSH box, hexadecimal or base64>",
    "playready_license_url": "https://license.example.invalid/playready",
    "fairplay": true
  }
}
```

```sh
drmr convert ./source/media.m3u8 --to cbcs --format hls \
  --output ./hls-cbcs --keys-file ./hls-cbcs.keys.json
```

For CENC HLS, select `--to cenc`, omit `hls_key_uri`, and set `fairplay` to `false`
or omit it. Keep Widevine, PlayReady, or both. FairPlay requires CBCS. CBCS can
also select any nonempty subset of the three systems: omit unwanted provider
fields and disable `fairplay` when it is not selected. A FairPlay selection needs
a `skd://` key URI; Widevine/PlayReady-only selections do not need that field.
Do not supply `iv`: fMP4 stores the sample-encryption vectors in its media.

Every media playlist declares the selected systems with `KEYFORMATVERSIONS="1"`:

| System | `KEYFORMAT` | `URI` data |
| --- | --- | --- |
| Widevine | `urn:uuid:edef8ba9-79d6-4ace-a3c8-27dcd51d21ed` | Complete PSSH box in a base64 `data:text/plain` URI |
| PlayReady | `com.microsoft.playready` | PlayReady Object containing the UTF-16 header in a base64 `data:text/plain` URI |
| FairPlay | `com.apple.streamingkeydelivery` | The operator-supplied `skd://` identifier |

CBCS uses `METHOD=SAMPLE-AES`; CENC uses `METHOD=SAMPLE-AES-CTR`. The corresponding
MP4 initialization files contain the exact selected Widevine/PlayReady PSSH
records and the same key identifier and encryption scheme. FairPlay adds no PSSH.
Generated fMP4 declarations omit `IV`. An existing CBCS declaration can supply an
IV only when it matches the actual initialization file's constant vector; CENC
declarations cannot supply one.
Generated outputs can be supplied to `analyze`, `plan`, `convert` and `verify`
with the matching input key IDs and content keys. URI identifiers are never key
or license fetch instructions. An input `EXT-X-SESSION-KEY`, when present, must
agree with the corresponding media declaration.

CENC HLS follows the optional fMP4 counter-mode method in the
[HLS second-edition draft, section 4.4.4.4](https://datatracker.ietf.org/doc/html/draft-pantos-hls-rfc8216bis-22#section-4.4.4.4).
It is not an Apple HLS authoring profile: the Apple-profile linter rejects CTR.
Supported playback depends on the player, content decryption module and device;
generation and native verification do not certify those systems. The provider
URI forms follow the official
[Shaka Player HLS DRM parsers](https://shaka-project.github.io/shaka-player/docs/api/lib_hls_hls_parser.js.html).

Without explicit provider signaling, generic CENC/CBCS HLS uses
`KEYFORMAT="identity"` and still requires `output.hls_key_uri` alongside `kid` and
`key`. drmr uses supplied keys for verification and never retrieves that URI.
This generic encryption configuration does not claim provider-specific DRM
signaling or license playback.

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

The input may be a qualified clear or known-key encrypted TS or fragmented MP4
presentation. Supply the appropriate `input` key entries for encrypted media.
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
agreement. HLS checks every referenced media playlist's method, selected key formats,
provider data URIs, key identifiers and actual initialization metadata. Referenced
Widevine PSSH and PlayReady PRO bytes must match the MP4 records, including the
CENC PlayReady checksum. Explicit output checks also bind the FairPlay key URI
to the operator configuration; no license service is consulted. TS FairPlay
checks retain the declared vector requirements. A DASH media resource may repeat a selected provider's exact
initialization PSSH once per provider. Different records, additional systems,
and duplicate records are rejected. FairPlay-only fragmented-MP4 HLS has no PSSH
records. HLS selections containing Widevine or PlayReady carry only the exact
selected provider records; unexpected Common or other provider records fail. These checks read bounded
metadata and skip media-data payloads; they do not load an entire segment into memory.

FairPlay media playlists require one valid `EXT-X-VERSION` declaration: at least
6 for ordinary fragmented-MP4 HLS using `EXT-X-MAP`, or at least 5 for MPEG-TS.
Higher compatible values are accepted. Missing, duplicate, malformed, and lower
versions are rejected. An unspecified audio language is omitted from the HLS
`LANGUAGE` attribute; a supplied language such as `en` is retained.

These signaling checks accompany complete encoded-content, timing, and supported
selection-metadata comparisons. Add `--decode` for decoded-content comparison.
Signaling checks do not replace media comparison.

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

The ten-second Big Buck Bunny regression starts with two-second clear TS HLS,
then saves and executes three plans: CENC DASH with Widevine/PlayReady, CBCS DASH
with Widevine/PlayReady, and clear fMP4 HLS. Each output is independently verified
against the preceding input, and the final clear result is compared with the
original TS. The default native checks require no decode. The separate multi-DRM
regression also compares complete decoded content and rejects altered provider
records, key identifiers, manifest schemes, and initialization-file references.

```sh
cargo test --locked -p drmr --test conversion_cenc_drm -- --nocapture
cargo test --locked -p drmr-core --test conversion_multi_drm -- --nocapture
cargo test --locked -p drmr-core --test conversion_hls_multi_drm -- --nocapture
cargo test --locked -p drmr --test conversion_hls_drm -- --nocapture
```

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

Keys, vectors, license addresses, PSSH data, PlayReady PRO data and FairPlay
addresses are excluded from diagnostic output, including their base64, hexadecimal
and complete generated data-URI forms. Addresses and initialization data intentionally appear in
the generated media and manifests where the delivery format requires them.

The playlist rules above follow
[RFC 8216 section 4.3.2.4](https://www.rfc-editor.org/rfc/rfc8216.html#section-4.3.2.4)
and [protocol version compatibility](https://www.rfc-editor.org/rfc/rfc8216.html#section-7).
See also the official
[Widevine PSSH payload definition](https://github.com/shaka-project/shaka-packager/blob/main/packager/media/base/widevine_pssh_data.proto).
