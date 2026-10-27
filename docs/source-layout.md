# Retain source directories and segment names

The CLI workflow keeps source-relative directories when downloading HLS or DASH
and when writing a segmented output. Directory placement is independent of
`--processing`: `segments` retains supported original segment
boundaries, while `stitch` packages complete tracks and segments them again.
See [media processing](media-processing.md#generic-conversion-and-timing-limits).

For example, a master playlist can reference `video.m3u8`, whose media is stored
in `video/1080p-high/`. Source layout retains that media directory. Another
1080p rendition in `video/1080p-low/` remains separate; the directory is taken
from actual media references rather than inferred from the resolution.

## Downloaded originals

The retained local entrypoint remains `input.m3u8`, `input.mpd`, or `input.mp4`.
HLS child playlist paths and HLS/DASH media paths retain their relative names.
The entrypoint and child playlists are localized manifests: their references
point to the downloaded files so they work offline. The private `raw/` directory
separately retains the manifest bytes received from the server and
`raw/index.json`, an acquisition index containing source URLs, sizes and hashes.
Media is stored once; `raw/` does not contain a second media copy. Keep it with
the retained source and protect the index because its URLs may be confidential.
Acquisition does not download keys from manifest key URIs.

For downloads made by earlier versions, conversion uses the local paths already
on disk. Acquire the URL again into a new download directory to retain the
remote directories.

The final root URL's directory defines the source-relative root. Source layout
requires an unambiguous local path for each resource. It refuses resources
outside that directory, query variants that select different resources
at the same path, unsafe encoded components, and names that would collide on a
case-insensitive filesystem. It also refuses file/directory overlaps and reserved
control names. The fetch policy independently controls which origins can be
accessed. An explicitly permitted additional origin can use distinct paths under
the same source-relative directory; different URLs cannot claim the same local path.

## Converted output

The output entrypoint remains `master.m3u8` or `manifest.mpd`. A source HLS child
playlist retains its relative path where possible. If that path is reserved or
shared by several output tracks, drmr chooses a distinct generated playlist name
in the same parent directory. Every rewritten reference points to the actual
output file.

Separate source media directories need no `track-0000-` prefix. For example,
`video/1080p-high/r000600.ts` becomes `video/1080p-high/r000600.m4s` plus
`video/1080p-high/init.mp4` for fMP4 output. HLS TS output keeps `.ts`.
Numbered sequences retain their starting number and zero-padding. If joining
and segmenting again changes the segment count, the sequence extends or ends
earlier; preserving names does not promise the original boundaries.
For the example above, numbered DASH references use `r$Number%06d$.m4s` with
`startNumber="600"` and `init.mp4` as the initialization resource.

Only physical output tracks sharing a media directory need a distinguishing
prefix derived from the source playlist name and track kind. This prevents
initialization and media files from colliding, including when stitching separates
a multiplexed TS rendition into video and audio.

HLS and DASH `list` output retain arbitrary segment stems when the output count
matches the source count, changing the extension for the output container.
Otherwise, they use generated names such as `seg_1.m4s` or `seg_1.ts`.
DASH `template` and `template-timeline` require a numeric filename pattern and
use `seg_1.m4s` when the source names cannot express one. Choose
`--dash-addressing list` to retain arbitrary names. `base` still writes one
indexed fragmented MP4 per representation. MP4 inputs have no source segment
names, so segmentation starts with `seg_1` in the source directory; the shared
directory collision rule still applies.

Conversion currently requires one media parent directory per source rendition.
It refuses a rendition whose media is spread across several directories instead
of silently flattening those directories. With `--processing segments`, a TS
playlist containing video and audio continues to refer to one shared set of media
files. With `--processing stitch`, separate video and audio tracks share the
original media directory and use the distinguishing prefixes described above.
Local resource references remain confined to the
selected source or output root; a layout requiring an upward `..` reference is
refused.

New single-output plans use schema 9 and named-output plans use schema 10. They
bind source selectors, directories, playlist paths and segment naming rules.
Execution revalidates the source graph and saved placement. Schemas 1–8 retain
their saved choices; schemas 7/8 keep their recorded `track-0000-` prefixes.
The Rust API's omitted layout choice retains its previous generated placement.

Re-layout moves files inside the unpublished private stage without copying media
bytes. Native media comparison, DRM signaling checks and manifest linting run
against the final layout before publication. Directory preservation does not
relax sample, timestamp, playback-window or manifest requirements.
