# Native media checks and optional complete decoding

Ordinary `analyze`, `plan --save`, `convert`, and `verify` use Rust media checks.
They read all admitted samples; they do not sample only the first segment or
trust a previous success report. Add `--decode` to perform complete decoding as
an additional check.

`convert` includes source/result comparison and the applicable native manifest
and DRM declaration checks before publishing the result. This also applies to
direct URL conversion and multiple outputs; there is no separate verification
flag to enable. Use standalone `verify` when the saved files should be read and
checked again after conversion. `verify --plan` additionally checks the saved
source fingerprints and output selections. A direct conversion does not save a
plan automatically.

```sh
drmr analyze source/input.m3u8 --json
drmr analyze source/input.m3u8 --decode --json
drmr plan source/input.m3u8 --to cbcs --format dash --output result --save conversion.plan.json --decode --json
drmr convert --plan conversion.plan.json --keys-file keys.json --json
drmr verify --plan conversion.plan.json --keys-file keys.json --json
```

The last two commands retain the saved plan's decode requirement even without
an explicit `--decode`. For direct conversion or comparison, the flag applies
to that invocation. `--decode` is refused with metadata-only analysis, planning
without `--save`, and manifest-only verification.

## What the default checks establish

Source admission reads the original container, codec configuration, sample
locations, encryption information, payloads and clocks within the supported
profile. Conversion checks and independent verification reopen the actual
output and compare it with the original. Native checks cover:

- Required stream and codec configuration, including retained display and audio metadata.
- Every selected compressed sample or access unit, its content and order.
- Every presentation time, decode time and duration, including the last sample.
- Validated playback windows and edits, so unchanged packets cannot conceal a changed presentation.
- Source identities, output container/protection, and the applicable manifest and signaling checks.

An access unit is the compressed data for one picture or audio frame. Container
framing can change during repackaging; format-aware checks retain the complete
non-framing content and codec configuration. Native evidence is read from the
original and final representations independently. Matching file names,
declared durations, or a conversion's earlier report are insufficient.

MP4 media clocks and presentation clocks are kept separately. An edit list can
shift the presentation clock or select a finite playback range. A zero-duration
edit in fragmented MP4 has no declared end; it can still have a nonzero clock
offset. Rust applies that offset to every sample before comparing presentation
times, while retaining the raw clocks and the independently declared range.
This accepts valid B-frame alignment and detects an edit-only timing change.
The single rate-one edit and its zero-duration meaning are described in the
[W3C ISO BMFF byte-stream note](https://www.w3.org/TR/2024/NOTE-mse-byte-stream-format-isobmff-20240723/#initialization-segments).
Known movie, track and media durations are checked against their respective
sample or playback evidence; a zero header duration remains unknown.

Display layers, track-size interpretation, video composition controls and audio
balance are part of the metadata checks. MP4-to-MP4 comparison also retains each
sample's recovery-group requirements. For comparison with TS, nonzero MP4
`roll` or `prol` recovery distances are currently refused: those groups specify
additional samples needed to recover decoding, and their equivalence to TS
signaling has not been qualified. Unchanged compressed bytes alone do not
establish equivalent playback or seeking.

Unknown mandatory structures, incomplete sample evidence and unsupported
profiles cause refusal. Selecting the default does not permit missing samples,
invented clocks, ignored playback edits, or truncated validation output.
An identified codec name or configuration box alone is not codec qualification.
Full MP4 admission also refuses unmodeled track apertures (`tapt`), track
references (`tref`) and extended-language controls (`elng`). Metadata-only
identification does not qualify those playback or selection semantics.

Complete media admission accepts the implemented AVC (H.264), typed AAC and
[HEVC Main/Main10](#hevc-in-mp4-and-fmp4) and [VP9 MP4/fMP4 profiles](#vp9-in-mp4-and-fmp4). Transport stream video
still requires the implemented H.264 profile. Recognizing AV1, AC-3,
E-AC-3, Opus or FLAC configuration metadata does not establish complete native
validation. Full operations refuse unimplemented profiles; metadata-only
identification remains available. `--decode` cannot bypass native refusals.
Container/encryption tables do not qualify every codec those formats can carry.

When a saved plan selects `segments`, verification also compares each original
resource's sample membership, fragment grouping and absolute clocks with the
corresponding result. Moving unchanged samples between resources therefore
fails the boundary check. HLS durations, DASH timelines, audio playback ranges
and final manifests are checked separately. File names and ciphertext bytes
can change when encryption records or transport packets are rewritten; the
boundary guarantee concerns the samples and their times. A `stitch` plan permits
new boundaries while retaining the content and playback checks above.

For AVC in transport streams, Rust checks SPS/PPS parameter sets, VUI metadata,
every slice header, original access-unit content and clocks, and original
playlist durations. Parameter sets describe how slices are interpreted; VUI
can carry timing, aspect-ratio and colour information. The qualified path needs
a stable parameter-set relationship and supported progressive pictures. It
refuses field-coded or mixed pictures, MBAFF, unsupported slice groups or slice
ordering, and unsupported repeated-picture durations. A TS video timing clock
must be available. Header parsing is not a claim that every AVC profile,
bit depth, chroma arrangement or combination has been qualified.

## HEVC in MP4 and fMP4

Version 1.1.0 adds single-layer, progressive HEVC (H.265) Main and Main10
with 4:2:0 chroma and matching 8-bit or 10-bit luma/chroma depths. This applies
to progressive MP4 and fMP4 in HLS or DASH, with clear, `cenc` or `cbcs` samples.
It is not included in the v1.0.3 release archives. HEVC transport streams and
Dolby Vision are not qualified by this path.

Rust parses the HEVC decoder configuration (`hvcC`) and the video, sequence and
picture parameter sets (VPS/SPS/PPS). It checks their identifiers, profile,
level, bit depth, dimensions, aspect ratio and colour information against each
other and the MP4 declarations. Each compressed sample is parsed as one coded
picture, with every Network Abstraction Layer (NAL) unit and slice header
checked. I/P/B pictures, multiple slices, short/long-term references, weighted
prediction, tiles, dependent slices and wavefront parallel processing are
modeled. IDR, BLA and CRA random-access pictures and leading pictures retain
their distinct headers and must match the MP4 sync declarations.

The `hvc1` entry keeps parameter sets in its configuration and refuses in-band
parameter sets. `hev1` accepts only unchanged declared parameter payloads before
a picture. Each NAL header's temporal identifier must separately satisfy its
access-unit rules; a repeated PPS can have a valid different temporal header.
Canonical comparison removes only these validated
framing repetitions and length prefixes. It retains every other compressed
byte, including supplemental enhancement information (SEI). Supported SEI
includes timing, recovery, user data, active parameter sets, decoded-picture
hashes and static HDR metadata. Decoded-picture hash messages are structurally
validated and retained; native checks do not recompute pixel hashes. Header
checks do not decode entropy-coded
coding tree units; `--decode` separately compares every decoded picture.
Retaining HDR metadata does not qualify HDR rendering or device playback.

Native CENC/CBCS writing leaves the complete parsed slice header and non-slice
NAL units clear. CENC protects a whole number of 16-byte blocks; CBCS begins
immediately after the slice header and applies the declared block pattern,
leaving incomplete final blocks clear. Each slice's pattern and encryption
records are checked independently of the output's original success report.
Native `segments` processing preserves resource membership and needs no
Packager or decoder. `auto`/`stitch` packaging routes use Shaka Packager while
Rust independently checks their complete input and output media.

A neutral `fiel` value of `[1, 0]` declares progressive frames. Its omission by
a packager is equivalent only when both independently parsed HEVC configurations
establish progressive pictures. Other HEVC field declarations are refused;
other display controls remain part of the comparison.

NAL temporal identifiers are checked against the picture and NAL type. End of
sequence (EOS) and end of bitstream (EOB) contain only their two-byte NAL header,
without a trailing payload. A picture following either terminal must be an
intra random-access picture (IRAP). Filler follows the picture's complete slices.

Hypothetical Reference Decoder (HRD) parameters describe decoder-buffer timing.
All declared sublayer CPB counts are parsed; a coded picture buffer (CPB) holds
compressed data before decoding. Buffering-period messages are checked against
the actual highest video temporal layer across each complete bitstream,
including ordered HLS/DASH resources. A bounded seven-layer candidate mask lets
later pictures resolve a message seen earlier. The declared maximum alone does
not choose a layer. EOB closes that proof and starts a new one when another
bitstream follows; EOS retains the current proof. These checks interpret the
headers and their applicable layer; they do not simulate decoder-buffer fullness.

The current validation profile requires one stable VPS, SPS and PPS. It refuses
multilayer/other profiles, unmodeled extensions or SEI, field/repeated pictures,
and a nonzero VUI default-display crop. These are implementation acceptance
limits; HEVC defines additional configurations. Configuration is bounded to
1 MiB and access units to 8 MiB and 4,096 NAL units. Exceeding a limit causes a
refusal rather than a switch to unchecked raw samples.

The attributed [Main/Main10 fixtures](../crates/drmr/tests/fixtures/bbb/HEVC.md)
contain 240 pictures each, including reordered B pictures, multiple slices,
CRA/leading pictures and in-band parameter repetition. The native regressions
cover MP4 clear → CENC → CBCS → clear, DASH/HLS conversions in `auto`, `stitch`
and `segments`, fresh independent verification, wrong/missing keys, late
payload/clock/sync changes and moved resource membership. Independent FFprobe
packet hashes/clocks and explicit complete-decoding tests check the native
implementation. See [the regression source](../crates/drmr-core/tests/conversion_hevc.rs).
This evidence does not establish DRM license acquisition or device playback.

## VP9 in MP4 and fMP4

Version 1.1.0 adds VP9 stored as `vp09` in MP4 and fragmented MP4 in HLS or
DASH, with clear or `cenc` samples. It is not included in the v1.0.3 release
archives. VP9 in transport streams is outside this conversion profile.

The supported profile/depth pairs are profile 0 at 8 bits, profile 1 at 8 bits,
profile 2 at 10 or 12 bits, and profile 3 at 10 or 12 bits. Profiles 0 and 2 use
4:2:0 chroma; profiles 1 and 3 accept the representable 4:2:2 or 4:4:4 forms.
The MP4 decoder configuration (`vpcC`) must match the coded headers. Rust checks
its version, flags, profile, bit depth, chroma arrangement and color declarations,
and retains the declared level, including the defined value 0 (unspecified). Coding-Independent Code Points (CICP) identify
color primaries, the transfer function and matrix coefficients; these values,
the range flag and chroma-siting declaration remain part of exact configuration
comparison. An identity RGB matrix uses the corresponding planar RGB format.
Unsupported configuration values and initialization data are refused.

Rust parses every coded frame in each MP4 sample, including hidden alternative
reference frames and `show_existing_frame` displays. It checks complete
uncompressed headers, reference availability, persistent header state,
compressed-header and tile bounds, and any superframe index. A superframe index
records the boundaries of several VP9 frames stored in one MP4 sample. State
persists across samples and ordered resources with eight reference slots.
Starting a dependent resource with an empty state, changing its order or
continuing after a parser failure cannot establish complete evidence.

The current profile requires exactly one displayed picture per MP4 sample.
Coded and render dimensions must stay fixed, fit the sample entry's dimension
bounds and remain unchanged in the comparison. The sample entry may provide a
larger bound; it does not replace the dimensions parsed from the frames.
Container sync declarations must agree with the whole sample's validated
reference independence, including hidden frames. A neutral progressive `fiel`
declaration is equivalent to its absence only after both tracks pass complete
VP9 validation. Other VP9 field declarations are refused. Other retained
semantic metadata remains strict.

The encoded-content comparison retains every sample byte and its boundary,
including uncompressed headers and superframe index bytes. It separately checks
every presentation time, decode time, duration and synchronization declaration.
Header validation does not decode entropy-coded coefficients or reconstruct
pictures. Use `--decode` to add independent complete decoding and decoded-content
comparison; it does not relax a native refusal. The supported encryption modes
remain unauthenticated, so correspondence without an independent known clear
reference does not authenticate a key or plaintext.

For VP9, the [VP codec MP4 binding](https://www.webmproject.org/vp9/mp4/)
requires `cenc`. The Rust segment writer keeps each complete uncompressed frame header
and the complete superframe index clear. It adds the alignment residue before
the protected bytes of each frame so its protected length is a whole number of
16-byte blocks. A header-only display can therefore remain entirely clear within
a CENC sample. Fresh sample/encryption checks still apply.

Final CENC output must use that prefix layout, including MP4 and `stitch`
packaging routes. After Shaka packaging, Rust rewrites the VP9 protection
records and ciphertext with the same key before final checks. Provider PSSH
data remains intact. This changes sample protection, without re-encoding
pictures or changing their clocks. A successful packager exit or matching decrypted bytes alone
cannot establish a conforming encryption map. Fresh verification checks the
actual encrypted ranges against each parsed frame and its complete index.

Packagers can also replace an unspecified VP9 level with an inferred one.
The source level is restored only after all other typed configuration fields
match. Final HLS/DASH codec declarations use the complete parameters from the
actual resulting `vpcC`, including its restored level and color values.

Known Shaka Packager 3.8 input with the alignment residue at the end of each
frame is accepted only as a repair source after complete native checks.
Analysis records `vp9-cenc-prefix-normalization-required`; conversion writes
canonical prefix ranges. The same noncanonical media is refused as a verified
final output. Whole-sample, header-encrypted and index-encrypted VP9 layouts
remain outside the admitted source profile. Saved plans rederive these facts
from current source bytes. No stored compatibility flag grants permission.

VP9 `cbcs` sources and targets are refused with
`vp9-protection-scheme-not-supported`. A packager accepting a VP9/CBCS request
does not make that combination conform to this binding. Standalone conversion,
saved-plan creation and replay retain this restriction. HLS whole-resource
`AES-128` is a separate outer encryption layer and is supported for the
qualified fMP4 HLS paths; it does not turn VP9 sample protection into CBCS.

VP9 samples are bounded to 8 MiB. Oversized or incomplete samples cause refusal
without falling back to an unchecked digest. The stateful VP9 resource rewrite
runs in source order within a rendition. Independent tracks and outputs remain
eligible for the ordinary bounded parallel scheduler; other codecs retain their
existing resource scheduling.

The attributed [VP9 fixtures](../crates/drmr/tests/fixtures/bbb/VP9.md) cover six
real profile/depth pairs with ten seconds of Big Buck Bunny, two-second keyframe
intervals and ordinary hidden alternative reference frames and superframes:

| Profile | Bit depth | Real fixture chroma |
| --- | --- | --- |
| 0 | 8 | 4:2:0 |
| 1 | 8 | 4:4:4 |
| 2 | 10 and 12 | 4:2:0 |
| 3 | 10 and 12 | 4:4:4 |

These regressions cover clear/CENC MP4 conversion and rekeying, fMP4 HLS/DASH
conversion, native source admission and fresh verification, selected complete
decoding, and independent FFprobe packet hashes and clocks. A separate
one-second-resource case starts its second resource with a dependent frame,
checking state continuity across resource boundaries. Parser cases also cover
4:2:2, RGB, header-only displays, one-frame indexes and malformed headers/indexes.
Crafted cases do not establish a real encoded-media matrix for those variants.
The higher-depth and 4:4:4 fixtures are adaptations of an 8-bit 4:2:0 source;
they do not qualify source quality or HDR rendering. VP9 HDR boxes such as
`SmDm` and `CoLL` remain outside the current sample-entry profile and are
refused. Device/license-server playback is not qualified by this fixture set.

The [VP9 bitstream specification](https://storage.googleapis.com/downloads.webmproject.org/docs/vp9/vp9-bitstream-specification-v0.7-20170222-draft.pdf)
and [MP4 binding source](https://github.com/webmproject/vp9-dash/blob/master/VPCodecISOMediaFileFormatBinding.md)
describe codec and storage rules; implementation limits and the evidence above
define this path's supported scope.

## What `--decode` adds

Complete decoding checks that the selected decoder can process the whole
source. During source-to-result comparison, it also compares the complete
decoded pictures or audio. Supported finite audio checks retain both all coded
audio and each file's independently validated presented range.

Native correspondence can establish that compressed content, configuration and
presentation were preserved without establishing that every entropy-coded
picture or audio payload can be decoded. `--decode` obtains that additional
evidence. It does not replace native checks or relax a refusal. A required
decode that fails, is interrupted or produces incomplete evidence cannot pass.
Neither depth proves license acquisition or playback on a particular device.

The supported encryption modes are unauthenticated. Without an independent
known clear reference, matching media under a supplied key does not authenticate
the key or original plaintext. Complete decoding adds decoder evidence and
cannot serve as cryptographic authentication of either.

Native `analyze`, saved planning and ordinary `verify` need no FFmpeg, ffprobe
or Shaka Packager for supported inputs. Conversion uses Shaka Packager on its
packaging/encryption routes; qualified `segments` routes instead transform
original resources and generate the manifests in Rust. Qualified finite
progressive MP4 copying and decryption also run in Rust, retaining sample
locations, clocks and playback edits. FFmpeg and ffprobe are used only for
complete decoding. `--decode` requires compatible builds of both. For TS
segment conversion, it also uses Shaka Packager to prepare decoder inputs;
the additional decoded comparison retains the native boundary checks.
External manifest validators remain separately requested tools.

## Reports state the evidence actually obtained

New complete-media reports use schema version 2. A comparison at native depth
can report:

```json
{
  "schema_version": 2,
  "required_depth": "native",
  "scope": "comparison",
  "passed": true,
  "checks": {
    "stream_metadata": "pass",
    "packet_payloads_equal": true,
    "packet_timing_equal": true
  },
  "decode_evidence": {
    "status": "not-checked",
    "reason": "not-requested"
  },
  "license_playback_verified": false
}
```

`passed` means the required depth was satisfied. `not-checked` never means
decoded equality. Complete source decoding uses `scope: "source-admission"`
and `decode_evidence.status: "source-decoded"`; a completed decoded comparison
uses `status: "content-compared"` with its actual `equal` boolean. A recorded
failure remains a failure when reports are combined or refreshed.

Full input-analysis schema version 2 contains this report under `verification`.
The former version-one `input_checks`, `media_verified` and
`decoded_content_equal` shapes are not emitted as substitutes for unperformed
decoding. Legacy version-one report readers retain their original meaning.
Metadata-only inspection and manifest-only reports keep their own schemas and
do not establish complete media correspondence.

## Saved plans retain a minimum depth

| Plan | Schema version | Minimum verification |
| --- | --- | --- |
| New single output | 3 | Explicit `required_depth`, included in the plan ID |
| New named outputs | 4 | Explicit `required_depth`, included in the plan ID |
| Legacy single output | 1 | Complete decoding |
| Legacy named outputs | 2 | Complete decoding |

Execution and fresh verification may strengthen the minimum with `--decode`;
they cannot lower it. Legacy identities and strict fields are retained. Keys,
request headers and tool commands remain runtime configuration rather than
saved plan content. The experimental Web/Desktop common workflow uses the same
policy and distinguishes unrequested decoding from a pass.
