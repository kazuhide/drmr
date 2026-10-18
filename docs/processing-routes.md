# Understand a processing route

`inspect` reports what local metadata says. `plan` lists conditional ways to satisfy the requested output. A successful `convert` reports the handler it actually used in `processing.route`, with a fixed reason and its input preparation. The identifiers below connect those reports to this guide.

```text
Original local file or manifest resources
  -> private complete tracks, with decryption when needed
  -> requested packaging and output protection
  -> metadata, every packet's timing, encoded content, and full decode checks
  -> new output, published only after verification
```

The finite progressive route copies the original encoded tracks directly, retaining their finite presentation window. It avoids fragmenting and then reconstructing that window. Other routes prepare complete tracks before packaging new segments; they do not transcrypt each original segment independently.

## Route conditions

| Identifier | Conditions and behavior | Required external tools |
| --- | --- | --- |
| `progressive-mp4` | Qualified clear or `cenc` progressive AAC-LC, optionally with H.264 from the same file, to clear MP4. Direct copying preserves the finite audio end. | FFmpeg, FFprobe |
| `fragmented-mp4` | One supported track to clear, `cenc`, or `cbcs` MP4. Shaka Packager creates fragmented intermediate/output media. Finite audio tails are refused here. | FFmpeg, FFprobe, Shaka Packager |
| `dash-presentation` | Supported complete local tracks to static DASH. Source manifest resources are localized and assembled into tracks before new segmentation. | FFmpeg, FFprobe, Shaka Packager |
| `fmp4-hls-presentation` | Supported complete tracks to fragmented MP4 HLS. Sample encryption applies within media samples; HLS AES-128 separately encrypts each complete output resource. | FFmpeg, FFprobe, Shaka Packager |
| `mpeg-ts-hls` | The narrow local MPEG transport stream profile to fragmented MP4 HLS. Tracks are demultiplexed, and the actual final playlists are checked against the original transport streams. | FFmpeg, FFprobe, Shaka Packager |

These are dependency requirements, not a report that tools are installed or were version-checked. See [supported input profiles](conversion-support.md) for restrictions and [tool requirements](requirements.md#media-tools) for supported versions.

### progressive-mp4

Complete original MP4 → direct encoded-track copy with its presentation edits → every original track checked → new clear MP4. No Shaka Packager step is used.

### fragmented-mp4

Complete original track → Shaka decryption/repackaging → requested output encryption/repackaging → original-to-output checks. Finite audio edits must use the qualified progressive route instead.

### dash-presentation

Local track files, or local manifest resources assembled into tracks → track normalization → Shaka segmentation and DASH manifest → original-to-output checks.

### fmp4-hls-presentation

Complete local tracks → Shaka segmentation and HLS playlists → optional whole-resource AES-128 encryption → original-to-output checks. Sample encryption is applied by the packaging step instead of the whole-resource step.

### mpeg-ts-hls

Original local HLS transport-stream resources → native structure admission and FFmpeg demultiplexing → fragmented MP4 HLS packaging → actual final playlist checks against the original streams. The intermediate MP4 is not used as the final reference.

Metadata alone may not qualify a finite MP4 profile or distinguish supported fragmented MP4 HLS from transport-stream HLS. A plan can therefore contain several conditional candidates. Each has `revalidation_required: true` and fixed condition codes. Actual source validation selects a handler or refuses the input; a rejected progressive profile does not imply permission to fall back to another route. Multiple output formats still require separate conversion commands.

The existing `compatible` and `execution_available` fields are coarse intent checks. Declared text tracks have no conversion candidate even when those fields are true: the media handlers support audio and video. Unknown HLS roles retain conditional candidates until actual resource inspection establishes their roles.

## Encryption facts and their limits

`details.track_protection` uses the same cipher and encryption-unit terms for MP4, DASH, and HLS. `container-default` evidence describes sample-description defaults; `manifest-declaration` describes signaling. Neither proves that all media samples follow those defaults. In MP4, the reported entries retain the existing sample-description granularity, which is not necessarily one entry per actual track.

`sample_coverage: unknown` means that inspection did not establish whether whole samples or only parts of samples are encrypted. A scheme label, a `tenc` default, or a pattern such as one encrypted block followed by nine clear blocks cannot establish that distinction. Pattern counts describe 16-byte blocks. `not-applicable` means the question does not apply to the declared unit, such as complete-resource HLS AES-128 encryption; it is different from unknown.

The fixed `sample_coverage_reason` explains unknown evidence: `sample-auxiliary-data-not-inspected` for input inspection, or `target-intent-not-sample-evidence` for a requested output. Neither is evidence about actual produced samples.

Inspection reads at most 16 MiB of MP4 movie metadata and skips media and fragment payloads. HLS inspection follows only bounded local child playlists, never media segments or keys. The additional facts do not widen those reads. DASH period counts describe direct, supported-namespace `Period` declarations, not verified durations or a complete media timeline. MP4 and HLS have no DASH period count.

The version-one JSON additions are optional: `Inspection.details`, `ConversionPlan.processing`, and `ConversionReport.processing`. An absent field in an older report means the information was not supplied. It never means clear media, a selected route, or successful verification. Plans and saved reports are not executable instructions. License and device playback remain outside the media-identity checks.
