# Understand a processing route

`inspect` reports what local metadata says. `plan` lists conditional ways to satisfy the requested output. A successful `convert` reports the handler it actually used in `processing.route`, with a fixed reason and its input preparation. The identifiers below connect those reports to this guide.

```text
Original local file or manifest resources
  -> private complete tracks, with decryption when needed
  -> requested packaging and output protection
  -> metadata, every packet's timing, encoded content, and full decode checks
  -> new output, published only after verification
```

The progressive MP4 route copies the original encoded tracks directly, retaining their finite presentation window. Other routes prepare complete tracks before packaging new fragments and preserve qualified playback windows in the resulting metadata. They do not transcrypt each original segment independently.

## Route conditions

| Identifier | Conditions and behavior | Required external tools |
| --- | --- | --- |
| `progressive-mp4` | Qualified clear, `cenc`, or `cbcs` progressive AAC-LC, optionally with H.264 from the same file, to clear MP4. Direct copying preserves the finite audio end. | FFmpeg, FFprobe |
| `fragmented-mp4` | One supported track, or qualified finite audio and optional video from the same progressive source, to clear, `cenc`, or `cbcs` MP4. Shaka Packager creates fragments; qualified finite playback windows are preserved. | FFmpeg, FFprobe, Shaka Packager |
| `dash-presentation` | Supported complete local tracks to static DASH. Source manifest resources are localized and assembled into tracks before new segmentation. | FFmpeg, FFprobe, Shaka Packager |
| `fmp4-hls-presentation` | Supported complete tracks to fragmented MP4 HLS. Sample encryption applies within media samples; HLS AES-128 separately encrypts each complete output resource. | FFmpeg, FFprobe, Shaka Packager |
| `mpeg-ts-hls` | The narrow local MPEG transport stream profile to fragmented MP4 HLS. Tracks are demultiplexed, and the actual final playlists are checked against the original transport streams. | FFmpeg, FFprobe, Shaka Packager |
| `mpeg-ts-sample-aes-hls` | Known-key SAMPLE-AES transport streams to fragmented MP4 HLS. Decrypted encoded frames and original clocks are checked before packaging; final playlists are verified against the original encrypted source. | FFmpeg, FFprobe, Shaka Packager |
| `mpeg-ts-dash` | Supported clear, AES-128, or SAMPLE-AES TS input to clear, `cenc`, or `cbcs` static DASH. Video, audio, or both retain their original frame and clock evidence; the final MPD and referenced media are read again before publication. | FFmpeg, FFprobe, Shaka Packager |
| `mpeg-ts-hls-ts` | Supported original TS input to TS HLS, with clear, whole-resource AES-128, or SAMPLE-AES protection. Original frame and clock evidence qualifies the private reference; final playlists are compared against it. | FFmpeg, FFprobe, Shaka Packager |

These are dependency requirements, not a report that tools are installed or were version-checked. See [supported input profiles](conversion-support.md) for restrictions and [tool requirements](requirements.md#media-tools) for supported versions.

### progressive-mp4

Complete original MP4 → direct encoded-track copy with its presentation edits → every original track checked → new clear MP4. No Shaka Packager step is used.

### fragmented-mp4

Complete original tracks → decryption when required → Shaka packaging and requested output encryption → preserve qualified playback windows → original-to-output checks. Finite audio and optional video from one qualified progressive source can share the resulting encrypted MP4. Its full coded content, native windows, playback settings, and each file's own presented samples must match the original; see [finite audio checks](media-processing.md#finite-audio-presentation-windows).

### dash-presentation

Local track files, or local manifest resources assembled into tracks → track normalization → Shaka segmentation and DASH manifest → original-to-output checks.

### fmp4-hls-presentation

Complete local tracks → Shaka segmentation and HLS playlists → optional whole-resource AES-128 encryption → original-to-output checks. Sample encryption is applied by the packaging step instead of the whole-resource step.

### mpeg-ts-hls

Original local HLS transport-stream resources → native structure admission and complete-track demultiplexing with Shaka Packager → fragmented MP4 HLS packaging → actual final playlist checks against the original streams. FFmpeg and FFprobe supply probing, decoding, and comparison evidence. The intermediate MP4 is not used as the final reference.

This includes audio-only 48 kHz AAC-LC: original PES clocks and ADTS frame evidence
anchor all content, sample counts, and timing checks. Complete frames determine
audio segment boundaries; a two-second target does not permit trimming samples.
See [audio-only fMP4 HLS](media-processing.md#audio-only-fmp4-hls-from-transport-streams).

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

Metadata alone may not qualify a finite MP4 profile or distinguish supported fragmented MP4 HLS from transport-stream HLS. A plan can therefore contain several conditional candidates. Each has `revalidation_required: true` and fixed condition codes. Actual source validation selects a handler or refuses the input; a rejected progressive profile does not imply permission to fall back to another route. Multiple output formats still require separate conversion commands.

`compatible` requires a valid target intent and a candidate for every requested output format. A known text track has no conversion candidate and makes the plan unsupported: the media handlers support audio and video. A plan with a supported DASH candidate but no candidate for its requested MP4 output is also unsupported. `execution_available` additionally requires one output format; neither field establishes that the source, keys, or tools have passed execution checks. Unknown HLS roles retain conditional candidates until actual resource inspection establishes their roles.

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

Inspection reads at most 16 MiB of MP4 movie metadata and skips media and fragment payloads. HLS inspection follows only bounded local child playlists, never media segments or keys. The additional facts do not widen those reads. DASH period counts describe direct, supported-namespace `Period` declarations, not verified durations or a complete media timeline. MP4 and HLS have no DASH period count.

The version-one JSON additions are optional: `Inspection.details`, `ConversionPlan.processing`, and `ConversionReport.processing`. An absent field in an older report means the information was not supplied. It never means clear media, a selected route, or successful verification. Plans and saved reports are not executable instructions. License and device playback remain outside the media-identity checks.
