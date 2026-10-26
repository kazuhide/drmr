# Retain source directories

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
HLS child playlist paths and HLS/DASH media paths retain their relative names in
source mode. Original manifest bytes and acquisition records remain in the
private `raw/` directory; local manifests refer to the retained local files.
Only the local projection changes. Acquisition does not retrieve keys from
manifest key URIs.

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

Media filenames can change. Each generated track uses a distinct filename prefix
such as `track-0000-` within its source media directory. This also permits several
tracks to share a directory without mixing their files. A TS-to-fMP4 conversion,
for example, can create `video/1080p-high/track-0000-init.mp4` and
`video/1080p-high/track-0000-seg_1.m4s`. Source layout does not promise identical
filenames or segment boundaries after repackaging.

Conversion currently requires one media parent directory per source rendition.
It refuses a rendition whose media is spread across several directories instead
of silently flattening those directories. With `--processing segments`, a TS
playlist containing video and audio continues to refer to one shared set of media
files. With `--processing stitch`, the existing packaging route writes separate
video and audio playlists with distinct generated filename prefixes in the
original media directory. Local resource references remain confined to the
selected source or output root; a layout requiring an upward `..` reference is
refused.

Saved source-layout plans bind the resolved source selectors, output directories,
playlist paths and generated filename prefixes. Execution revalidates the source
graph and the saved placement. Earlier plan schemas 1–6 and the Rust API's omitted layout choice retain their
previous generated placement.

Re-layout moves files inside the unpublished private stage without copying media
bytes. Native media comparison, DRM signaling checks and manifest linting run
against the final layout before publication. Directory preservation does not
relax sample, timestamp, playback-window or manifest requirements.
