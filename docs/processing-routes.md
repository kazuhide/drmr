# Understand a processing route

`analyze --metadata-only` reports what local or URL metadata declares. Without `--save`, `plan` lists conditional ways to satisfy the requested output. Normal `analyze` checks complete source media; `plan --save` additionally fixes its file fingerprints and output intent for later execution. Use `--outputs` for [several named outputs](multiple-outputs.md). A successful single conversion reports the handler it actually used in `processing.route`, with a fixed reason and its input preparation; a multiple-output report includes this information for each child conversion. The identifiers below connect those reports to this guide. See [saved execution plans](executable-plans.md) for the four-stage workflow and fresh revalidation boundaries.

```text
Original local file or manifest resources
  -> private complete tracks, with decryption when needed
  -> requested packaging and output protection
  -> native metadata, every sample's timing/content, and playback-window checks
  -> additional complete decoding when required by --decode or a saved plan
  -> new output, published only after verification
```

The progressive MP4 route copies the original encoded tracks directly, retaining their finite presentation window. The complete-track routes below prepare media before packaging new fragments and preserve qualified playback windows in the resulting metadata.

## Select the processing unit

`plan --save` and direct `convert` accept `--processing auto|segments|stitch`.
`segments` uses supported original resources and retains their sample membership,
timestamps and boundaries. `stitch` prepares complete tracks and packages the
requested output; HLS/DASH output is segmented again, while MP4 output is a file.
`auto` resolves a qualified route for the input and requested output. An explicitly requested mode never silently changes to another mode.

New single-output plans use schema 9 and named-output plans use schema 10. Both
fix the requested and resolved modes, boundary guarantee, source-relative
placement and DASH reference form. Earlier schemas 1–6 keep their exact saved
packaging requests and previous placement. `convert --plan`
refuses `--processing`; create a new plan to change it. Native segment processing
needs no per-segment media subprocess. Additional `--decode` can still prepare
complete tracks for decoding; the existing TS decoding preparation also needs
Shaka Packager. It does not replace native checks. Actual reports state `mode`
and `boundaries` separately from the handler identifier. See
[processing and timing](media-processing.md#generic-conversion-and-timing-limits).

## Route conditions

| Identifier | Conditions and behavior | Required external tools |
| --- | --- | --- |
| `fmp4-segments` | Supported original fMP4 resources with their sample grouping, clocks and segment boundaries retained; encryption and initialization metadata are rewritten natively. | None at native depth |
| `mpeg-ts-segments` | Supported original TS resources with each segment's frames and clocks retained; native encryption updates TS signaling and packet layout as needed. | None at native depth |
| `progressive-mp4` | Qualified clear, `cenc`, or `cbcs` progressive AAC-LC, optionally with qualified video from the same file, to clear MP4. Native copying and decryption preserve the finite audio end. | None at native depth |
| `fragmented-mp4` | One supported track, or qualified finite audio and optional video from the same complete MP4 source, to clear, `cenc`, or `cbcs` MP4. Shaka Packager creates fragments; qualified finite playback windows are preserved. | Shaka Packager |
| `dash-presentation` | Supported complete local tracks to static DASH. Source manifest resources are localized and assembled into tracks before new segmentation. | Shaka Packager |
| `fmp4-hls-presentation` | Supported complete tracks to fragmented MP4 HLS. Sample encryption applies within media samples; HLS AES-128 separately encrypts each complete output resource. | Shaka Packager |
| `mpeg-ts-hls` | The narrow local MPEG transport stream profile to fragmented MP4 HLS. Tracks are demultiplexed, and the actual final playlists are checked against the original transport streams. | Shaka Packager |
| `mpeg-ts-sample-aes-hls` | Known-key SAMPLE-AES transport streams to fragmented MP4 HLS. Decrypted encoded frames and original clocks are checked before packaging; final playlists are verified against the original encrypted source. | Shaka Packager |
| `mpeg-ts-dash` | Supported clear, AES-128, or SAMPLE-AES TS input to clear, `cenc`, or `cbcs` static DASH. Video, audio, or both retain their original frame and clock evidence; the final MPD and referenced media are read again before publication. | Shaka Packager |
| `mpeg-ts-hls-ts` | Supported original TS input to TS HLS, with clear, whole-resource AES-128, or SAMPLE-AES protection. Original frame and clock evidence qualifies the private reference; final playlists are compared against it. | Shaka Packager |
| `fmp4-hls-ts` | Qualified fragmented MP4 file, HLS or DASH to TS HLS, with clear, AES-128 or FairPlay SAMPLE-AES output. Complete tracks are prepared and the final encoded frames, clocks and playlists are independently checked. | Shaka Packager |

The table describes native conversion. Add FFmpeg and ffprobe for `--decode`,
and Shaka Packager for existing TS complete-decoding preparation;
native analysis and verification need none of these tools. Decode evidence is
additional to every required native check. See [verification depth](verification.md).

These are dependency requirements, not a report that tools are installed or were version-checked. See [supported input profiles](conversion-support.md) for restrictions and [tool requirements](requirements.md#media-tools) for supported versions.

Metadata-only candidate `required_tools` lists the native minimum. Its
`condition_codes` and explanation state additional requirements:
`additional-complete-decoding-requires-ffmpeg-and-ffprobe` applies to complete
decoding. Actual processing reports list the tools required by the selected
depth. Native analysis and verification are distinct from these conversion
tool sets.

### progressive-mp4

Complete original MP4 → native copy and decryption when needed, retaining its presentation edits → every original track checked → new clear MP4. No external media tool is used at native depth. See [finite MP4 processing](media-processing.md#finite-audio-presentation-windows).

### fragmented-mp4

Complete original tracks → decryption when required → Shaka packaging and requested output encryption → preserve qualified playback windows → original-to-output checks. Finite audio and optional video from one qualified progressive source can share the resulting encrypted MP4. A joint fragmented source with one video and one AAC-LC track can also be packaged again when both retain their positive normal-rate finite edits and matching protection. Clear output on this reinput path stays fragmented. Its full coded content, native windows and playback settings must match the original. With `--decode`, each file's own presented decoded samples must also match; see [finite audio checks](media-processing.md#finite-audio-presentation-windows).

### dash-presentation

Local track files, or local manifest resources assembled into tracks → track normalization → Shaka segmentation and DASH manifest → original-to-output checks.

### fmp4-hls-presentation

Complete local tracks → Shaka segmentation and HLS playlists → optional whole-resource AES-128 encryption → original-to-output checks. Sample encryption is applied by the packaging step instead of the whole-resource step.

### mpeg-ts-hls

Original local HLS transport-stream resources → native structure admission and complete-track demultiplexing → fragmented MP4 HLS packaging with Shaka Packager → actual final playlist checks against the original streams. Rust supplies complete access-unit, configuration and clock evidence. Optional `--decode` additionally supplies complete decoding and decoded comparison evidence. The intermediate MP4 is not used as the final reference.

This includes audio-only 44.1 or 48 kHz AAC-LC: original PES clocks and ADTS frame evidence
anchor all content, sample counts, and timing checks. Complete frames determine
audio segment boundaries; a two-second target does not permit trimming samples.
See [audio-only fMP4 HLS](media-processing.md#audio-only-fmp4-hls-from-transport-streams).

Rust prepares 44.1 kHz AAC as private fragmented MP4 directly from validated
ADTS frames, with the original 90 kHz timestamps and the actual audio rate of
44,100 samples per second. Shaka Packager then segments that track. Video and 48 kHz audio retain their
Shaka demultiplexing path. The same 44.1 kHz preparation feeds DASH and TS output.
All routes retain every original PTS and DTS exactly; source timestamp rounding
is never reused as an output error allowance. See
[AAC clock handling](media-processing.md#aac-sample-rates-and-transport-clocks).
This path is included in the 1.0.1 source. It is not included in the unchanged v1.0.0 archives,
whose TS audio profile accepts only 48 kHz; see [release status](releases.md).

### mpeg-ts-sample-aes-hls

Original encrypted HLS transport-stream resources → program and packet checks →
SAMPLE-AES frame decryption with supplied keys and IVs → a private clear TS view
checked against every original encoded frame and clock → track demultiplexing and
fragmented MP4 HLS packaging → actual final playlist checks. A separate `verify`
reads and decrypts the original source again. It does not trust a saved conversion
report or use only the intermediate MP4 as its reference.

### mpeg-ts-dash

Original local HLS TS resources → native frame and clock checks, with known-key
decryption when needed → separate complete tracks → fMP4 segments with the
requested sample protection and a static DASH manifest → original-source checks
against the actual final manifest and media. Audio-only input uses the same native
AAC evidence as TS output. Video can be accompanied by one supported audio track.
The original TS clocks remain the reference; an intermediate MP4 is not a replacement
for them. See [DASH timing from transport streams](media-processing.md#dash-output-from-transport-streams)
for presentation offsets and the manifest's decimal duration representation.

### mpeg-ts-hls-ts

Original clear, AES-128, or SAMPLE-AES local TS resources → native program,
frame, and clock checks, with decryption when needed → complete tracks → new
TS segmentation and requested protection → independent final-playlist checks
against the original media. Joint audio/video produces separate playlists and
an HLS master; audio-only uses native AAC frame boundaries and original clocks.
The packaging step disables an initial clear interval and timestamp shifting.
This route is selected explicitly by `--format hls-ts`; the existing `hls`
format continues to produce fragmented MP4.

Metadata alone may not qualify a finite MP4 profile or distinguish supported fragmented MP4 HLS from transport-stream HLS. A metadata-only plan can therefore contain several conditional candidates. Each has `revalidation_required: true` and fixed condition codes. Actual source validation selects a handler or refuses the input; a rejected progressive profile does not imply permission to fall back to another route. A saved plan with `--outputs` executes its named outputs together, using the appropriate route for each one.

For metadata-only plans, `compatible` requires a valid target intent and a candidate for every requested output format. A known text track has no conversion candidate and makes the plan unsupported: the media handlers support audio and video. A plan with a supported DASH candidate but no candidate for its requested MP4 output is also unsupported. Its `execution_available` field additionally requires one output format; neither field establishes that the source, keys, or tools have passed execution checks. Unknown HLS roles retain conditional candidates until actual resource inspection establishes their roles.

## Several final outputs

The common saved-plan workflow can package a supported clear HLS transport-stream
source into FairPlay HLS and Widevine/PlayReady DASH in one operation. It prepares
and packages each output separately. DASH and fragmented MP4 HLS do not share
physical media files in this workflow. Separate copies bind verification to
the exact files about to be published, which increases temporary disk use;
the checks do not load all media into memory.

Each output must pass its complete media comparison, actual format and
protection checks, and any explicitly requested DRM declaration checks. The
DRM check compares the supplied runtime settings with the final MPD, MP4
initialization data, and effective HLS key declarations. It also checks the
minimum HLS protocol version required by those declarations.

Native Rust linting checks the final HLS/DASH declarations and reports coverage
separately from media correspondence. An output may additionally require
[external manifest validation](manifest-validation.md).
Apple's playlist-only validator checks the final HLS playlists, and the
DASH-IF schema and rule checks inspect the final MPD. These checks operate on
byte-identical copies of the encrypted output manifests. They do not substitute
decrypted media, request keys, or prove license acquisition or device playback.
The new result directory appears only after every output has passed all its
required checks. A fresh `verify --plan` repeats those checks against the
current original and final output files.

## Encryption facts and their limits

`details.track_protection` uses the same cipher and encryption-unit terms for MP4, DASH, and HLS. `container-default` evidence describes sample-description defaults; `manifest-declaration` describes signaling. Neither proves that all media samples follow those defaults. In MP4, the reported entries retain the existing sample-description granularity, which is not necessarily one entry per actual track.

For HLS, each track description also records effective `hls_key_methods`, using
fixed values such as `SAMPLE-AES` or `AES-128`. An unknown method is reported as
`UNKNOWN`, without copying input text. A SAMPLE-AES declaration leaves the
container protection `unknown`: metadata inspection has not established whether
the media is TS or MP4. A plan can offer conditional TS and MP4 routes only when
the method evidence belongs to each corresponding track. Conversion still
validates the actual container, keys, IVs, and complete media.

`sample_coverage: unknown` means that inspection did not establish whether whole samples or only parts of samples are encrypted. A scheme label, a `tenc` default, or a pattern such as one encrypted block followed by nine clear blocks cannot establish that distinction. Pattern counts describe 16-byte blocks. `not-applicable` means the question does not apply to the declared unit, such as complete-resource HLS AES-128 encryption; it is different from unknown.

The fixed `sample_coverage_reason` explains unknown evidence: `sample-auxiliary-data-not-inspected` for input inspection, or `target-intent-not-sample-evidence` for a requested output. Neither is evidence about actual produced samples.

Metadata-only analysis reads at most 16 MiB of MP4 movie metadata and skips media and fragment payloads. Local HLS follows bounded child playlists inside its directory tree. URL metadata reads HLS playlists or a DASH MPD under the fetch policy, never initialization files, media segments or keys; remote MP4 requires valid Range responses. These temporary reads do not retain an executable-plan source. The additional facts do not widen those reads. DASH period counts describe direct, supported-namespace `Period` declarations, not verified durations or a complete media timeline. MP4 and HLS have no DASH period count.

Metadata report version-one additions remain optional: `Inspection.details`, `ConversionPlan.processing`, and `ConversionReport.processing`. An absent field in an older report means the information was not supplied. It never means clear media, a selected route, or successful verification. Conditional plans and saved reports are not executable instructions. The separate `kind: "execution-plan"` format saves typed local intent and source identity; execution still revalidates the actual media and runtime configuration. License and device playback remain outside the media-identity checks.

### fmp4-hls-ts

Use `--format hls-ts` with `--to clear`, `--to hls-aes128`, or
`--to hls-sample-aes`. Supplied input keys decrypt CENC/CBCS samples or outer
HLS AES-128 protection; the source is retained unchanged. Normalization is
checked in Rust before TS packaging, and the final HLS presentation is reopened
for native media and manifest checks. Additional `--decode` checks complete
media decoding with FFmpeg and ffprobe.

This container change uses `stitch`, including segmentation of its TS output.
It does not preserve the original fMP4 segment boundaries. MP4 initial audio
edits that TS cannot represent are refused; supported final AAC playback ranges
remain explicit. See [conversion support](conversion-support.md#mpeg-transport-stream-output).
