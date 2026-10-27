# Final manifest validation

The common `convert` and `verify` commands check final HLS playlists or DASH
manifests with the built-in Rust linter. The check uses the actual encrypted manifest bytes. It does not create
a decrypted copy of the presentation.

This is a **manifest-only** check. It does not fetch media segments, initialization segments, content keys, or licenses. It does not establish that encrypted media conforms to a media format or plays on a device. Complete internal media verification and native DRM declaration checks remain separate results.

## Native checks

The independent `drmr-manifest` library accepts manifest bytes and an explicitly
supplied playlist graph. It does not open manifest, media or key files, fetch
resources or launch tools. Automatic worker selection queries available CPU
capacity. drmr captures the local graph, binds the report to those bytes, and
rechecks the files before accepting a result.

HLS rules cover syntax, attribute types and duplicates, playlist and segment
state, encryption and initialization declarations, feature-derived protocol
versions, and linked rendition declarations. The base RFC 8216 profile and the
Apple VOD profile are separate: an Apple recommendation does not become a general
HLS requirement. DASH rules check the declared static-presentation profile,
including namespaces, structure, inherited addressing, template expressions,
timeline arithmetic, and manifest-visible protection declarations. These native
rules are independently implemented; they are not a complete XML schema or
Schematron engine.

Each report names its profile and revision. Rule coverage records `passed`,
`failed`, or `not-checked`, with specification references. Diagnostics use fixed
rule identifiers and source positions rather than echoing input text, URLs,
keys, or protection payloads. Unsupported in-scope features, missing linked
manifests, exhausted limits, and truncated diagnostics cannot produce a clean
result. Checks requiring media or delivery evidence are marked outside the
manifest-only scope. Complete source-to-result verification supplies separate
packet, decoding, metadata, and timing evidence.

Small inputs run inline. Larger independent documents can run in parallel;
available CPU capacity, `--max-workers`, and memory admission bound concurrency.
The report order is deterministic. Memory admission estimates the parser's
working allocation; it is separate from a measurement of total process memory.

## Choose when to check

To check an existing final output without a source or keys:

```sh
drmr verify ./result/hls/master.m3u8 --manifest-only --json
drmr verify ./result/dash/manifest.mpd --manifest-only --json
```

These results explicitly contain `scope: "manifest-only"`, `media_verified: false`,
and `license_playback_verified: false`. This mode cannot be combined with
`--source`, `--keys-file`, or `--plan`. `--max-workers` sets an upper bound for
native work; omitting it selects concurrency automatically. No external
validator installation is needed for the native check.

`--manifest-profile base` is the default: RFC 8216 manifest rules for HLS and
the static-presentation profile for DASH. Select
`--manifest-profile apple-vod` with `--manifest-only` to add the supported
Apple VOD authoring rules to HLS. This selection is invalid for DASH. The chosen
profile appears in `native_manifest.profile`; it does not change the external
validator's mode or settings. Conversion and complete source/result verification
use the base profile.

For example, `drmr verify ./result/hls/master.m3u8 --manifest-only
--manifest-profile apple-vod --json` checks the Apple profile. A single video
bitrate can pass base HLS rules and fail Apple's multiple-bitrate requirement
in [section 9.9](https://developer.apple.com/streaming/hls-authoring-specification-for-apple-devices.html).
drmr preserves existing video qualities during conversion. It does not create
additional bitrates to satisfy that requirement. Even a clean Apple manifest
report does not establish the media, delivery or device requirements outside
the report's declared scope.

Add `--external-validator` to either command to run the native check and the
corresponding Apple or DASH-IF adapter. The JSON result separates
`native_manifest` and `external_manifest`.
Apple's adapter uses parse-only mode. It does not have the same authoring-rule
coverage as the native Apple profile; a successful parse is separate evidence.

Conversion already performs complete native source/result media comparison,
native manifest checks and the requested DRM declaration checks. No additional
flag or `verify` command is needed to perform those checks during conversion.

To additionally require external manifest validation during conversion, choose
`conformance: "required"` for each applicable output in the
[output list](multiple-outputs.md). This applies both to direct
`convert --outputs` and to saved plans. Every required check must pass before the
result directory is published. A separate `verify --plan` reads the current
originals and results again and repeats the required checks, complete source/result
media comparison and the plan's DRM selections.
`conformance: "none"` leaves the external check unrequested; it does not mark it
as passed or disable native manifest or complete media verification. Saved
version-2 plans retain this external-engine requirement: a native result does
not substitute for `conformance: "required"`.

`convert` and `verify` accept `--hls-validator-bin` (default
`mediastreamvalidator`) and `--dash-validator-bin` (default `php`). These options
select executables for requested checks; they do not enable a check on their
own. Runtime executable locations are not saved in a plan.

Human-readable multi-output results name the completed media comparison, native
manifest checks and DRM declaration checks. An external result is shown only when
requested. In JSON, an unrequested external check remains `conformance: null`;
it is separate from `conversion.verification.native_manifest` in a conversion or
`verification.native_manifest` in independent verification.

## HLS

Install Apple's [HTTP Live Streaming tools](https://developer.apple.com/documentation/http-live-streaming/using-apple-s-http-live-streaming-hls-tools) on macOS. Apple distributes `mediastreamvalidator` separately; it is not bundled with drmr or the Linux Docker image. Access to Apple's download requires an Apple Developer account.

Set `ManifestValidationTools.mediastreamvalidator` to the executable name or path. drmr captures the entry playlist and all referenced local variant and audio playlists, then validates byte-identical private copies with the tool's `-p` playlist-only option. Every captured playlist must have a complete, successful parse-only JSON result. Media files and key files are absent from this private workspace.

The initial adapter supports the JSON 1.2 format produced by `mediastreamvalidator` 1.25.36. It rejects every reported issue, including issues nested in a child playlist. A successful process exit alone does not pass: this tool can exit successfully while its JSON contains errors. Unknown JSON versions and incomplete results fail rather than implying compatibility.

The supported profile covers local, finite video-on-demand playlists generated by drmr. Child playlist references must stay inside the output directory. Remote child playlists, symbolic links, variable expansion, and unsupported playlist extensions are rejected before the tool runs.

### HLS version requirements

`EXT-X-VERSION` declares a compatibility level. Validation derives a minimum from the features used in each playlist; it does not require a single literal version. For example, a normal fragmented MP4 playlist with `EXT-X-MAP` requires version **6 or higher**. Both 6 and 7 satisfy that requirement. FairPlay's `KEYFORMAT` attribute independently requires version 5 or higher.

| Feature used in a playlist | Minimum version |
| --- | --- |
| Explicit `IV` on `EXT-X-KEY` | 2 |
| Decimal `EXTINF` durations | 3 |
| `EXT-X-BYTERANGE` or `EXT-X-I-FRAMES-ONLY` | 4 |
| `KEYFORMAT` or `KEYFORMATVERSIONS` on `EXT-X-KEY` | 5 |
| `EXT-X-MAP` in an I-frame-only playlist | 5 |
| `EXT-X-MAP` in a normal media playlist | 6 |
| `SERVICE` closed-caption identifiers in the master playlist | 7 |

These requirements follow [RFC 8216, section 7](https://www.rfc-editor.org/rfc/rfc8216.html#section-7) and [the `EXT-X-MAP` definition](https://www.rfc-editor.org/rfc/rfc8216.html#section-4.3.2.5). Missing, malformed, duplicate, or insufficient version declarations cannot satisfy a required higher level.

### Shared target duration

The native graph check also compares `EXT-X-TARGETDURATION` across linked media
playlists, applying the VOD subtitle and I-frame exceptions in
[RFC 8216 section 6.2.4](https://www.rfc-editor.org/rfc/rfc8216.html#section-6.2.4).
Audio frame boundaries can produce a different per-track duration bound from
video. RFC 8216 compares segment durations after nearest-integer rounding.
Common conversion derives conservative bounds by rounding final segment durations
upward, then applies their maximum to all its audio/video playlists. A two-second
segmentation target can therefore produce a declaration of 3. Segment data and `EXTINF` durations stay
unchanged. Fresh verification checks the captured final files before any
normalization of the media.

## DASH

Native checks support inherited `SegmentTemplate`, `SegmentList` and indexed
`SegmentBase`. Lists check URL counts against exact timelines or nominal segment
durations, and closed ranges for the same literal resource must not overlap.
Range-only Lists require a single effective object `BaseURL`. Base checks
initialization and index declarations. Actual file extents, index contents,
sample clocks and any URI aliases need the separate native media verifier.
Nominal timing arithmetic never relaxes the checks for an explicit timeline.

Existing general typed rules use the pinned fifth-edition schema. Additional
addressing facts use the [MPEG sixth-edition schema](https://github.com/MPEGGroup/DASHSchema/blob/6th-Ed/DASH-MPD.xsd):
`startNumber` and `endNumber` are unsigned 64-bit integers, while `duration` and
`timescale` are unsigned 32-bit integers. This native profile applies signed
`eptDelta` to nominal addressing. `presentationDuration` declares a representation's
own presented endpoint relative to `presentationTimeOffset`, in the MPD timescale.
It must lie within the Period. An explicit `endNumber` fixes the finite reference
count; without it, the count is derived from that own extent and nominal duration.
Each nominal start must precede the own endpoint. These manifest checks do not
require tracks of unequal duration to reach the longest Period endpoint; the
media verifier must still prove their actual samples, clocks and nominal edges.
Bounded arithmetic overflow leaves the dependent rule unresolved. Existing XML
Schema 1.0 datatype rules remain unchanged. In particular, `xs:unsignedInt` and `xs:unsignedLong` collapse
XML whitespace before lexical validation, so ` 1 ` has the same value as `1`.
The qualification environment's PHP 8.5.7 / libxml 2.9.14 schema engine rejected
eight such boundary cases. Those differences were resolved against
[the fixed whitespace rule](https://www.w3.org/TR/2004/REC-xmlschema-2-20041028/#rf-whiteSpace),
without changing the native rule or the recorded external verdicts. An external
rejection still fails an explicitly required external check, even when the native
check passes; drmr does not silently substitute one result for the other.

The DASH adapter uses the official DASH-IF XML schema and Schematron rules. An XML schema checks element structure, attributes, and their order. Schematron checks additional rules expressed as assertions. Both checks must complete and pass. This is more than checking that the file is well-formed XML.

The adapter invokes these installed assets directly, without running the DASH-IF web service, segment downloader, or media conformance checks. Nothing is uploaded to the public DASH-IF service.

Install the following dependencies separately:

- PHP 8.4 or later with the DOM and libxml extensions.
- Java 21 or later on the PHP process's executable search path.
- A local checkout of the official [DASH-IF Conformance](https://github.com/Dash-Industry-Forum/DASH-IF-Conformance) source, including `resources/xsd/DASH5th.xsd`, `resources/xsd/xlink.xsd`, `schematron/output/val_schema.xsl`, `schematron/saxon12he.jar`, and its bundled `schematron/lib` dependencies.

For a reproducible source installation, select a fixed release outside the output directory:

```sh
git clone --branch v3.1.0 --depth 1 \
  https://github.com/Dash-Industry-Forum/DASH-IF-Conformance.git \
  ./dash-if-conformance
export DRMR_DASH_IF_ROOT="$(pwd)/dash-if-conformance"
```

Set `ManifestValidationTools.dash_manifest_validator` to the **PHP interpreter**, such as `php` or its absolute path. drmr supplies its own embedded adapter and fixed arguments. This field does not accept a shell command or an arbitrary list of arguments. On Windows, select the PHP executable directly. No Composer installation, Laravel server, or DASH-IF web application setup is required for this adapter. The external engine is subject to its own [license](https://github.com/Dash-Industry-Forum/DASH-IF-Conformance/blob/v3.1.0/LICENSE).

Only the fixed installed schema and its bundled XLink schema can be resolved. Instance `schemaLocation`, media `BaseURL`, and license URLs do not authorize a request. Document type declarations, external entities, XInclude, XLink references, and XML stylesheet instructions are refused. Saxon runs with network protocols and external Java functions disabled; unsupported external dependencies in the installed stylesheet also fail validation.

## Results and operational limits

A native result includes rule coverage, error and warning counts, incomplete/truncated status, and the profile revision. `verification.native_manifest` accompanies complete HLS/DASH media verification. Standalone manifest-only results use the separate `native_manifest` field. MP4 files have no HLS/DASH manifest to lint.

An external report has `scope: "manifest-only"`, a manifest count, and a result for the selected engine. HLS external results include the declared and required minimum version for every playlist. The observed Apple version or DASH-IF checkout revision is included only when it can be extracted in a restricted format; otherwise it is unknown. A checkout revision identifies the observed source checkout, rather than proving an unmodified installation.

The public report and events omit executable paths, manifest URLs, license signaling, and raw external-tool diagnostics. Tools run with the existing process timeout and bounded output capture. Missing tools, cancellation, timeout, output overflow, malformed reports, and partial check coverage return an error. They never produce a passing report. The file identities and contents of private manifest copies and the embedded adapter are rechecked before success. Private copies and engine files are normally removed after process-tree cleanup is confirmed. If a workspace directory has been replaced, validation fails and cleanup preserves uncertain entries rather than deleting the replacement.

Qualification used Apple `mediastreamvalidator` 1.25.36 and the official DASH-IF v3.1.0 assets at revision `bdf0c27a8f198da7a4bf81634bb01faf7c7c3c06`, with PHP 8.5.7, Java 21.0.11, and SaxonJ-HE 12.9. Actual encrypted FairPlay HLS playlists and a CBCS DASH manifest carrying Widevine and PlayReady signaling passed manifest-only validation. Reduced HLS version declarations and malformed DASH structures failed. A live local HTTP observer recorded no engine requests while schema, media, and license URLs referenced it. These checks did not fetch or validate media, contact a license service, or test playback.
