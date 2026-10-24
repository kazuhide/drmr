# Native media checks and optional complete decoding

Ordinary `analyze`, `plan --save`, `convert`, and `verify` use Rust media checks.
They read all admitted samples; they do not sample only the first segment or
trust a previous success report. Add `--decode` to perform complete decoding as
an additional check.

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

Complete media admission currently accepts the implemented AVC (H.264) and
typed AAC profiles. Recognizing HEVC, AV1, VP9, AC-3, E-AC-3, Opus or FLAC
configuration metadata does not implement their complete native validation.
Full media operations refuse those profiles with
`native-codec-profile-not-supported`; metadata-only identification remains
available. `--decode` still requires native admission and cannot bypass this
refusal. Container and encryption support tables do not qualify every codec
that those formats can carry.

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
original resources and generate the manifests in Rust. Clear finite progressive MP4 can use a native
copy. The edit-preserving encrypted finite progressive route retains an
FFmpeg copy remux; this applies when preparing that encrypted source for MP4,
DASH or fMP4 HLS. It rewrites the container without transcoding and is separate
from verification. `--decode` requires compatible FFmpeg and ffprobe. For TS
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
