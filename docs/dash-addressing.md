# DASH segment references

Use `--dash-addressing` when planning a DASH output. This setting controls how the media presentation description (MPD) names and times its resources. Select the encryption scheme and DRM signaling separately.

| Value | MPD structure | Media files |
| --- | --- | --- |
| `template` (default) | `SegmentTemplate` with a nominal `duration`, without `SegmentTimeline` | Initialization file and numbered media segments per rendition |
| `template-timeline` | `SegmentTemplate` with an exact `SegmentTimeline` | Initialization file and numbered media segments per rendition |
| `list` | `SegmentList` with an exact `SegmentTimeline` and explicit resource names | Initialization file and media segments per rendition |
| `base` | `SegmentBase` with initialization and index byte ranges | One indexed fragmented MP4 file per rendition |

`SegmentTimeline` describes segment timing inside a template or list; it is not a separate addressing format. Changing these references preserves the encoded samples. The processing choice controls whether source boundaries are retained or media is joined and segmented again; see [processing routes](processing-routes.md).

## Nominal timing

The default `template` form describes an approximate segment grid. Actual audio and video boundaries can differ because they fall on complete frames. For example, a video cadence can produce 6.006-second segments while audio stays close to six seconds.

drmr checks every actual segment edge against the nominal duration. The generated grid starts at the Period origin without a separate phase shift. Every actual edge must lie within half a nominal duration of its declared position, with the final interval clipped to the exact presentation end. drmr also proves that the common Period enumerates exactly the files present in each rendition. This prevents a shorter audio rendition from acquiring a nonexistent extra segment in players that derive the count from the Period alone. Actual sample timestamps and the complete final segment remain unchanged. `template-timeline` records each boundary explicitly when that is preferable. The timing allowance for nominal templates is not applied to exact timelines. [DASH-IF timing model, section 18](https://dashif.org/Guidelines-TimingModel/).

Segment processing retains the existing media boundaries and does not select a new segmentation target. In that case, drmr derives a nominal duration from the observed segments, then proves the same timing allowance against every actual edge. It prefers interior segments when available so that shorter boundary fragments do not determine the initial duration candidate.

Each nominal template declares its finite final segment number (`endNumber`) and exact presentation extent (`presentationDuration`). Generated nominal templates do not use `eptDelta`: output compatibility does not depend on a player implementing that phase attribute. Audio may need a different MPD timescale to express its extent exactly; a timescale is the number of time units per second. These attributes use the MPEG-DASH full profile. They do not claim qualification under the more restrictive DASH-IF timing model. [MPEG-DASH schema](https://github.com/MPEGGroup/DASHSchema/blob/6th-Ed/DASH-MPD.xsd).

A packaging request expressed in milliseconds can also require a finer MPD clock.
drmr uses the smallest common integer scale needed for the media edges and the
request's reduced fraction. For example, 2.083 seconds and media ticks at 12,288
units per second are both exact at 1,536,000 units per second. Whole-second
requests retain the existing scale. This changes manifest units only; media
timestamps and the checks against every actual edge remain unchanged. A scale
that exceeds the supported 32-bit range is refused.

Some unequal boundaries, including a short first segment followed by a long regular cadence, cannot satisfy all of these conditions with one nominal duration. The command then fails with `dash-nominal-duration-not-representable`. Create a new plan using `--dash-addressing template-timeline` to declare exact boundaries. drmr does not change formats silently.

## Indexed files and verification

The `base` form contains a flat segment index (`sidx`). drmr builds it from the final initialization and media files after timestamp, encryption signaling, and placement adjustments. Index entries identify exact byte extents and presentation intervals. Presentation intervals include supported MP4 edits, including a zero-duration normal-rate edit that offsets all following fragments; see [media clocks](media-processing.md#conversion-and-timing-limits). Native verification rereads the final MPD, initialization, index, and media, then compares sample payloads and clocks with the original.

DASH inputs can use finite templates, lists, or single-file indexes. List inputs may identify separate objects or disjoint byte ranges in one object. Every retained original object is hashed in full, including an indexed file accessed through ranges. Unsupported index hierarchies, external index objects, invalid ranges, and nonrelocatable absolute fragment offsets are refused.

Manifest linting, native media comparison, optional complete decoding, and player or license-service qualification provide different evidence. See [manifest validation](manifest-validation.md) and [verification](verification.md).
