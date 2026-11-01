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
[HEVC Main/Main10 MP4/fMP4 profiles](#hevc-in-mp4-and-fmp4). Transport stream video
still requires the implemented H.264 profile. Recognizing AV1, VP9, AC-3,
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

Current source supports single-layer, progressive HEVC (H.265) Main and Main10
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
