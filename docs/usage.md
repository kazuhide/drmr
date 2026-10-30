# Using drmr

Install `drmr` before following these examples. Rust checks complete supported media by default. Native analysis and verification need no external media tools. Complete-track packaging uses Shaka Packager; progressive MP4 copying and decryption run in Rust. Optional `--decode` adds complete FFmpeg decoding. Run `drmr doctor` to check installed external tools. The [Docker guide](docker.md) provides an alternative with those tools included. Commands process existing media; they do not encode new video qualities. For PowerShell examples and Windows file permissions, see the [Windows guide](windows.md).

Use the [multiple-output workflow](multiple-outputs.md) to produce HLS for FairPlay
and CBCS DASH for Widevine/PlayReady from one qualified source. It uses the same
`plan`, `convert`, and `verify` commands, with explicit per-output formats, DRM
information, native manifest linting, and optional [external manifest checks](manifest-validation.md).

## Choose a workflow

For qualified local MP4, DASH, or HLS media, start with the common commands:
`analyze --metadata-only` reads metadata, `analyze` checks complete source media, `plan` describes
intent or saves one fixed conversion, `convert` writes new media, and `verify`
compares a result with its original source.

All four media operations require the native checks of every selected sample,
codec configuration, clock and supported playback window. `--decode` adds
complete source decoding or decoded source-to-result comparison. It does not
replace native checks. See [verification depth and evidence](verification.md).
Metadata-only analysis, planning without `--save`, and manifest-only verification
refuse `--decode` because they do not perform complete media checks.

For URL preflight, a saved executable plan, offline conversion, and independent
verification, follow [analyze, save a plan, convert, and verify](executable-plans.md).
Use `analyze URL --download-dir ./source`,
`plan ./source/input.m3u8 --to cbcs --format dash --output ./result --save ./conversion.plan.json`,
then `convert --plan ./conversion.plan.json` and
`verify --plan ./conversion.plan.json`, with keys supplied separately when needed.
For DASH and MP4 URLs, plan from `./source/input.mpd` and `./source/input.mp4`
respectively. The [URL input table](#convert-a-url) lists the acquisition limits.

## Select a processing mode

Use `--processing auto|segments|stitch` with direct `convert` or `plan --save`:

| Mode | Behavior |
| --- | --- |
| `auto` | Select a qualified route for the input and requested output. This is the default when the option is omitted. |
| `segments` | Process the original segments, retaining their sample membership, timestamps and boundaries. This requires a supported segmented source and matching media containers. |
| `stitch` | Join source segments when needed, then package the media in the requested output format. MP4 output is a file; HLS/DASH output is segmented again. Original segment boundaries are not retained. |

For a supported clear TS HLS source, preserve its segment boundaries with:

```sh
drmr plan source/input.m3u8 --to clear --format hls-ts --processing segments \
  --output result --save conversion.plan.json
drmr convert --plan conversion.plan.json
drmr verify --plan conversion.plan.json
```

For TS to fragmented MP4 DASH, select `--processing stitch` instead. The output
format and protection remain separate `--format` and `--to` choices. A newly
planned HLS/DASH output inherits the admitted HLS source's common
`EXT-X-TARGETDURATION`; a six-second source therefore supplies a six-second
selected bound. The actual packaging request also follows the admitted video
resource cadence when it is shorter; see [segment timing](media-processing.md#generic-conversion-and-timing-limits).
Different audio/video target declarations are refused with
`hls-target-duration-mismatch`. MP4 and DASH inputs retain the two-second fallback.
Actual cuts follow sample clocks, video access points and audio-frame boundaries;
the first or last segment can be shorter. `stitch` does not preserve the original
boundary schedule. Existing plans keep their recorded duration, including the
historical two-second target; create a new plan to inherit a source target.
An explicitly selected mode that cannot satisfy the profile is refused rather
than replaced. Saved plans fix the selected mode and boundary behavior, so
`convert --plan` refuses `--processing`, including `auto`. No mode changes the
required native checks or the meaning of `--decode`. See
[processing and timing](media-processing.md#generic-conversion-and-timing-limits).

## Resource directories

New downloads and segmented outputs retain the [source directory structure](source-layout.md).
For example, media in `video/1080p/` stays in that directory. Generated filenames
can change, and several tracks sharing a directory use distinct filename prefixes.
Old saved plans retain their previous placement.

## Select DASH references

Use `--dash-addressing template|template-timeline|list|base` with direct `convert`
or `plan --save`. The default `template` uses numbered segments and a nominal
duration. `template-timeline` records exact boundaries, `list` names every
resource, and `base` writes one indexed fragmented MP4 per rendition. This option
applies only to DASH outputs. For example:

```sh
drmr plan source/input.m3u8 --to clear --format dash --dash-addressing list \
  --output result --save conversion.plan.json
drmr convert --plan conversion.plan.json
drmr verify --plan conversion.plan.json
```

The saved plan fixes this choice; `convert --plan` refuses an addressing override.
For several outputs, use the [schema-3 output list](multiple-outputs.md#select-dash-references-for-each-output).
See [DASH references and timing](dash-addressing.md) for each form's conditions.

## Read metadata and plan a conversion

```sh
drmr analyze media.mp4 --metadata-only
drmr analyze master.m3u8 --metadata-only --json
drmr plan manifest.mpd --to cbcs --format dash,hls --json
```

`analyze --metadata-only` detects MP4, DASH, and HLS by their contents. It needs no keys, decoder or Shaka Packager. The JSON report has `schema_version: 1`, a format, protection per track or sample description, known key identifiers, DRM system labels, and explanation tokens in `findings`. It excludes encryption keys, initialization vectors, license URLs, and key URLs. Generated track identifiers are local to the report.

Manifest reports describe declarations. MP4 reports read initialization metadata and skip media payloads. Both return `media_verified: false`; they do not decrypt, decode, validate every sample, or prove device playback. Unsupported or incomplete protection remains `unknown`. An HLS `SAMPLE-AES` declaration alone does not establish the underlying MP4 encryption scheme.

For a local HLS input, only child playlists inside the input playlist's directory tree are followed. Remote, missing, cyclic, and escaping child references remain unresolved. Metadata reading does not acquire media segments or keys. Limits are 4 MiB per manifest, 16 MiB total, 32 playlists, and 128 references. Each track permits at most 64 key identifiers; DASH also limits inherited protection declarations to 64, and HLS permits at most 64 active key formats. MP4 movie metadata is limited to 16 MiB.

The same mode accepts HTTP(S) URLs without a download directory:

```sh
drmr analyze "https://example.com/vod/master.m3u8" --metadata-only --json
drmr analyze "https://example.com/vod/manifest.mpd" --metadata-only --json
drmr analyze "https://example.com/vod/media.mp4" --metadata-only --json
```

Remote HLS reads playlist declarations and needed child playlists; DASH reads the MPD. Neither fetches initialization files, media segments, key addresses or licenses. MP4 uses HTTP Range requests for selected byte positions. Validated responses provide box headers and movie metadata (`moov`), skipping `mdat` media payloads even when `moov` is at the end. A server that ignores Range is refused with `remote-metadata-not-supported` instead of triggering a full MP4 download. Temporary private metadata is cleaned up; it is not a retained original and cannot be used as a saved-plan source.

The [fetch policy](#convert-a-url) applies to remote metadata too, including allowed origins, authentication headers and resource limits. Metadata parser limits still apply when a policy allows larger downloads. Live HLS or dynamic DASH declarations may be readable, but this mode does not follow updates, establish a complete VOD asset or admit a conversion. Use normal `analyze URL --download-dir NEW_DIR` for full acquisition and media checks.

`--metadata-only` refuses `--download-dir`, `--keys-file`, `--packager-bin`, `--max-workers`, `--decode` and `--measure`. `--fetch-policy` is accepted only with a URL. The former `inspect INPUT` command is removed; replace it with `analyze --metadata-only INPUT`. See the [pre-release migration](releases.md#pre-release-cli-consolidation).

`plan INPUT --to clear|cenc|cbcs|hls-aes128|hls-sample-aes` describes the requested operations and independent input/output key roles. `--format mp4|dash|hls|hls-ts` selects output packaging; comma-separated values are accepted. Omitting it retains the input format. An HLS AES-128 target requires `hls` or `hls-ts` output. SAMPLE-AES TS requires `hls-ts`. `cenc` and `cbcs` are refused with TS output. CENC output to HLS is currently rejected. Unsupported source protection or unresolved encryption is reported as incompatible, except that a consistent SAMPLE-AES declaration can produce conditional HLS routes. Those routes still require the appropriate keys, IVs, and actual media validation.

A compatible plan has a valid format/protection combination and a conditional processing route for each requested output format. Known unsupported declarations, such as a text track that no media handler can retain, produce `compatible: false`, `execution_available: false`, and exit code `1`. A plan is also unsupported if only some requested formats have a route. This corrects older version-one reports that could advertise executable intent with an empty or incomplete candidate list; supported plans retain their existing JSON fields and values.

`execution_available` additionally requires one output format. It never proves a qualified media profile, usable keys, installed tools, or a selected handler. Execution still checks the actual source profile, keys, and media. Multiple requested output formats are planned together but must be converted in separate invocations. Such a conditional plan can succeed with `execution_available: false`. This metadata-only JSON cannot be passed to `convert --plan`. Use `plan --save` to create the separate strict execution-plan format after complete source admission. Both formats exclude secret keys; neither is a trusted authorization artifact.

Optional `details` distinguish declared ciphers, encryption units, default patterns, and unknown sample coverage. Plans add conditional routes and required toolsets; successful conversion reports add the actual route and reason. See [processing routes](processing-routes.md) to compare the plan with the result. A tool requirement does not mean its availability was checked.

## Convert and verify local media

```sh
drmr convert source.mpd --to cbcs --format dash --output protected-dash --keys-file keys.json
drmr convert source.mpd --to hls-aes128 --format hls --output protected-hls --keys-file segment-keys.json
drmr verify protected-dash/manifest.mpd --source source.mpd --keys-file keys.json --json
```

`convert` detects local MP4, DASH, and HLS inputs. It also accepts the supported MP4, static DASH and completed HLS URLs described below. For one output, choose `--to` and `--format`; omitting `--format` retains the input format. Use `--outputs FILE` for several named outputs. The output path is a new MP4 file or a new result directory. Existing destinations are refused. Input media is read into a private working area, and output is published only after content and timing checks pass. Nothing is re-encoded.

`clear`, `cenc`, and `cbcs` can be written to DASH; `clear` and `cbcs` can be written to fragmented MP4 HLS. HLS `AES-128` encrypts complete HLS resources and requires `--format hls` or `hls-ts`. `hls-ts` also supports clear and SAMPLE-AES output from qualified local TS inputs. A single-track MP4 can also be written as `clear`, `cenc`, or `cbcs`.

A qualified progressive MP4 with one AAC-LC audio track and optional H.264 video preserves its audio start and end when converted to MP4, DASH, or fMP4 HLS with the protection choices above. Clear, `cenc`, and `cbcs` input are supported within the [finite audio profile](conversion-support.md#progressive-mp4-with-a-finite-audio-range). Both tracks must come from the same original file. Encrypted input requires every track key and refuses source DRM initialization headers (PSSH). Clear MP4 output remains progressive; encrypted MP4 output is fragmented and can retain both qualified tracks in one file.

A narrow local MPEG transport stream input profile can produce clear, `cenc`, or `cbcs` DASH, and clear, `cbcs`, or AES-128 fragmented MP4 HLS. Use `--format hls-ts` to retain transport-stream packaging, including SAMPLE-AES encryption for FairPlay media. Other multiplexed MP4 output, live input, and unrestricted DASH/HLS profiles remain unsupported. See [conversion support](conversion-support.md) for the tested routes and [processing and verification](media-processing.md) for timing limits.

`verify OUTPUT --source INPUT` independently compares the original source and result. It requires their respective keys when encrypted. Success means the required native stream metadata, complete encoded content, packet clocks and playback windows match. `--decode` additionally requires complete decoded picture/sound equality. Neither depth establishes license acquisition or device playback. Use the generated `manifest.mpd` or `master.m3u8` as the output argument for a presentation. Both commands support `--json` for one versioned result object. New complete-media JSON reports use schema version 2 with explicit required depth, scope and decode evidence; metadata-only and manifest-only reports retain their separate schemas. Progress is separate, as described below.

For an operational failure after a valid `convert` or `verify` command with `--json`, stdout contains one object with `schema_version: 1`, `command`, `status` (`failed` or `cancelled`), and `error` containing a fixed `code`, safe `hint`, and `last_activity` (a fixed label or `null`). A completed verification that finds a mismatch retains its existing `checks` and comparison fields and adds that failure description. With default `auto` progress and `--json`, stderr carries a fixed failure token and shows text progress only when it is a terminal. Exit codes remain `1` for processing or comparison failure, `2` for the existing configuration errors, and `130` for confirmed cancellation. Cleanup-unconfirmed failures remain failures, even if cancellation was requested. Clap argument parsing errors still use stderr and exit `2`, before this JSON command contract applies. If stdout itself cannot be written, a complete JSON object cannot be guaranteed: the command reports `result-output-unavailable` on stderr and exits `1`. No raw tool output, source URL, key, or operator path is added to progress or failure JSON. Successful URL conversion retains its explicit saved-source locations in the existing result.

## Progress output

Global `--progress` controls observations on standard error. It never adds progress to standard output or changes the final result format, so existing pipes keep working.

| Mode | Behavior |
| --- | --- |
| `auto` | Default: text with human-readable results; with `--json`, text only when stderr is a terminal, otherwise off |
| `text` | One live terminal dashboard; concise log lines when redirected, also with `--json` results |
| `json` | Progress records as JSON Lines: one JSON object per line, also with `--json` results |
| `off` | No progress records; final results and errors still apply |

The automatic check uses stderr, not stdout. For example, `--json >result.json`
still shows progress on an attached terminal; `--json 2>errors.log` does not.
Explicit `text`, `json` or `off` takes precedence, including when streams are
redirected.

```sh
drmr analyze media.mp4 --metadata-only --json --progress text
drmr analyze media.mp4 --metadata-only --json --progress json \
  >metadata.json 2>progress.jsonl
drmr analyze media.mp4 --metadata-only --progress off
```

An interactive terminal updates one dashboard below the command, without switching screens or clearing earlier output. Named-output conversion shows a command and elapsed-time header, a `Source` row for acquisition and checks, an `Overall` row for the phase or available work estimates, and one row per output. Completed outputs remain in their row as `Checked` until the complete result is published. Internal milestones and retained-download notices do not accumulate. Only the dashboard rows clear before final results appear. After a terminal resize, the previous snapshot stays in history and the dashboard starts again on a new line. Normal text omits internal tool names. Redirected text retains milestones and spaced updates, including elapsed-time notices during unchanged work.

Terminal refreshes, including phase and output-state changes, run at most once per second. Final success/failure/cancellation appears immediately. Below 60 columns, bars are omitted while state and available percentages remain. Counts and step estimates appear only when they fit. These display limits do not throttle media processing or reduce JSON progress observations.

Named-output task labels distinguish copying source tracks, checking source conversion,
preparing clear tracks, comparing converted media and independently comparing
the original with the result. Observable Rust jobs report `active` (started but
unfinished), `waiting` (not yet started), and completed/total counts for that
stage. An active job can be waiting for shared CPU or memory admission; this is
not CPU utilization. `stopped` counts jobs that exited without successful
completion, not media-corruption findings. A stage's completed count does not
complete the conversion. Shaka Packager's internal task counts are not exposed,
so its worker reservation is not displayed as actual active renditions.

Final checks are part of publication, so their durations overlap and completion is reported after the result is committed.

Qualified clear HLS TS conversion to two or more `stitch` fMP4 HLS/DASH outputs at native depth reports approximate conversion work when every output uses clear, cenc or cbcs. This applies to direct conversion and execution of a saved qualifying plan, excluding the separately displayed URL download. Logical work weights preparation counts, generated bytes and media comparison. File-integrity checks contribute to remaining-time estimates. Work percentages do not measure disk I/O, playback time or the fraction of validation checks passed. Generated-byte totals remain approximate because encryption and container metadata add bytes beyond the input payload.

Remaining time uses rates for each kind of step observed during the current operation. It follows the longest parallel output path and the later checks rather than extrapolating from total elapsed time or adding overlapping output durations. No previous runs or asset-specific timings are used. `ETA` means estimated time remaining: `ETA ~4s` is about four seconds. If a future step has no observed rate, the display says `ETA estimating` and the overall JSON `remaining_ms` is `null`. An output's `step ~4s` estimates its current step only and can appear before an overall estimate. Estimates can change with the step and host load. Decoded verification, required external validators and other processing routes do not receive a guessed overall work total or remaining time; their task, observed counts and elapsed time remain visible. Overall `100%` is emitted only after the complete result has been published.

When resource discovery has finished, the download can show its completed count and percentage. TS preparation counts segments across the entire media playlist currently being processed. Approximate time remaining appears only after at least one second and two more completed resources or segments have been observed since the total became known. The estimate covers that download or playlist's preparation, not the entire command. After all TS segments have been read, boundary checks continue in playlist order without a percentage or time estimate for that work.

Discovery and other work with an unknown total show the task and elapsed time. A single MP4 URL shows received bytes during transfer, with no running percentage or remaining-time estimate. Its final resource count becomes `1/1` after the transfer finishes.

Download resource totals include completed discovery requests and the distinct media resources. Received bytes include data received again during retries and can differ from the retained file size. Worker selection applies to bounded independent jobs and does not mean every analysis stage runs with that many workers.

Use `--progress json` for numeric resource and scope observations. Named-output conversion adds a `pipeline` with phases, output states/counts and optional work estimates. Optional `source_renditions` and `outputs[].renditions` contain typed operations and observed `started_units`, `active_units`, `waiting_units`, `completed_units`, `stopped_units` and `total_units`. Work is marked `estimated: true`, `basis: logical-media-passes`; remaining time uses `eta_basis: observed-step-rates`. Its phases are `source`, `outputs`, `final-check` and `publication`. Existing activity and scope fields remain available. A closed scope can overlap, nest or end with failure; the last activity is not a diagnosis. Progress works without `--measure`, whose separate phase durations can overlap.

Stages include configuration, acquisition, inspection, input preparation, normalization, packaging, verification and publication. `doctor` reports tool checks; `serve` reports startup and waiting. Quick metadata reads and plans can finish before a heartbeat. Help and version output do not invent work records.

Progress contains no source URLs, keys, authentication headers, operator paths, raw media metadata or raw tool output. For a valid parsed operational command, explicit `--progress json` keeps operational stderr in JSON Lines, including a fixed error code on failure or cancellation. Argument parsing errors occur before that contract; existing `serve` startup diagnostics and its loopback address notice can also be separate text. The final result remains on stdout; progress records alone do not prove success.

`convert` already compares the original and final media and runs the applicable native manifest and requested DRM declaration checks before publication. For named-output conversion (`--outputs` or a corresponding saved plan), human-readable results name these checks; external manifest results appear only when requested, and each output's JSON retains `conformance: null` for an unrequested external check. A separate `verify` rereads the saved files for fresh evidence; it is not required to enable conversion-time verification.

## Convert a URL

For a supported HLS, static DASH or MP4 source, pass its URL directly to `convert` and choose
where to retain the downloaded source:

```sh
drmr convert "https://example.com/vod/master.m3u8" --download-dir ./source --to cbcs --format hls --output ./converted --keys-file keys.json
```

`--download-dir` is required for URL input. Both it and `--output` must be new,
separate paths under existing parent directories. Existing downloads are never
replaced or silently reused. Local input needs neither `--download-dir` nor
`--fetch-policy`.

The command identifies the format from the response content, saves the original
resources and creates a local entrypoint before conversion and verification:

| URL input | Retained entrypoint | Acquisition conditions |
| --- | --- | --- |
| MP4 | `source/input.mp4` | One complete MP4 within the local media profiles |
| DASH | `source/input.mpd` | One static `Period`, finite template or list references, or a supported single-file index |
| HLS | `source/input.m3u8` | Completed TS/fMP4 playlists within the supported input profiles |

The same URL input and `--download-dir` options work with normal `analyze` when you want
to check the source before [saving a plan](executable-plans.md). URLs need no
filename extension; queries and allowed redirects do not select the format.
Use the corresponding local entrypoint for `plan` and offline reuse. The separate
`analyze --metadata-only URL` mode retains no such entrypoint.

For DASH, drmr saves every referenced initialization and media file and creates a
local MPD. Only resource addressing is localized; protection declarations,
PSSH DRM initialization data and timing declarations are preserved. Original
manifest bytes remain separate. Local HLS playlists localize media references
and resolve relative key identifiers against the effective remote playlist
address. Encrypted media and initialization bytes are retained without
decryption during acquisition. No separate snapshot command is required.

A completed download remains available even if conversion fails. Once the source
is saved, the server is no longer needed. Retry with its local entrypoint, or
compare the result independently:

```sh
drmr convert source/input.m3u8 --to cbcs --format hls --output ./retry --keys-file keys.json
drmr verify converted/master.m3u8 --source source/input.m3u8 --keys-file keys.json
```

An incomplete download is not admitted as input. Cancellation stops acquisition
and media work before cleanup; a completed source is retained. Downloaded source
files are private to their owner. Result media is published only after content,
timing, and saved-source integrity checks pass.

The [input table](conversion-support.md#url-input) states the qualified container
and encryption combinations, including encrypted MP4/DASH and completed HLS
with clear, AES-128 or sample-encrypted media. The same local codec, track,
encryption and timing restrictions still apply. Live input, HLS byte ranges,
multiple DASH periods, hierarchical or external DASH indexes
and negative timeline repeats remain outside this acquisition profile.

Supply known input keys through `--keys-file` for full `analyze`, source checks when
saving a plan, conversion, and independent verification. For AES-128 and
SAMPLE-AES TS, `input.hls_keys[].uri` must exactly match the resolved absolute key
identifier. For example, `keys/key.bin` in
`https://example.com/vod/media.m3u8` becomes
`https://example.com/vod/keys/key.bin`. Resolution uses the final playlist address
after redirects, so the same relative name in different directories can select
different keys. An absolute `skd:` identifier remains an identifier. For
sample-encrypted fMP4, provide the existing KID-to-key mappings in `input.keys`.
No key URI or license endpoint is fetched.

#### Optional fetch settings

By default, retrieval uses HTTPS and the input URL's origin: its scheme, host,
and port. A manifest that references another origin needs explicit permission.
Use an owner-only JSON file with `--fetch-policy` for additional origins,
authentication, or resource limits. The input URL stays on the command line;
it does not need to be repeated in the file.

```json
{
  "allowed_origins": ["https://media.example.com"],
  "headers_by_origin": {
    "https://example.com": {"Authorization": "Bearer <your token>"}
  },
  "max_total_bytes": 1073741824,
  "connections": 4
}
```

```sh
drmr convert "https://example.com/vod/master.m3u8" --download-dir ./source --fetch-policy fetch.json --to clear --format hls --output ./clear-result
```

Headers are sent only to their configured origin; redirects and child references
must also use allowed origins. HTTP and private-network access are disabled
unless the file explicitly sets `allow_http` and `allow_private_network` to
`true`, respectively. Credentials and source URLs are not printed in reports.
The private original files can contain signed URLs, so keep that directory private.

Optional limits also include `max_playlist_bytes`, `max_playlists`,
`max_segments_per_track`, `max_resource_bytes`, `max_total_resources`,
`min_free_disk_bytes`, `timeout` in seconds, and `attempts`. Counts are integers;
unknown fields, duplicate fields, incorrect types, and invalid limits are refused.
`connections` is a fetch upper bound; `--max-workers` and CPU/memory admission
can reduce actual concurrency.

`max_playlist_bytes` bounds each HLS playlist or DASH MPD. MP4 and media resource
sizes use `max_resource_bytes`; `max_total_bytes` applies to the whole download.

URL conversions add a `download` object to the JSON result, containing local
`directory` and `entrypoint` display paths, the relative `entrypoint_relative`,
and `retained: true`. Source URLs and request headers are excluded. Local
conversions retain their existing result shape.

### Separate input and output keys

Supply keys in an owner-only JSON configuration; there is no environment fallback. Store this outside the output tree and version control. Here is its structure using placeholders; replace each placeholder with your own hexadecimal value before use:

Read [keys and local security](security.md) before using confidential media. It explains which values are masked, which metadata is published, and how keys reach external tools, temporary files, and local interfaces.

```json
{
  "schema_version": 1,
  "input": {
    "keys": [{"kid": "<32 hexadecimal characters>", "key": "<32 hexadecimal characters>"}],
    "hls_keys": [{"uri": "<exact URI from the input playlist>", "key": "<32 hexadecimal characters>"}]
  },
  "output": {
    "kid": "<32 hexadecimal characters>",
    "key": "<32 hexadecimal characters>",
    "hls_key_uri": "<key location to advertise in HLS>"
  }
}
```

`input.keys` maps MP4 key identifiers (KIDs) to decryption keys. `input.hls_keys` maps each exact HLS AES-128 or SAMPLE-AES TS key URI to a key supplied by you; drmr never retrieves those URIs. Omit unused input entries. For `cenc`/`cbcs`, provide `output.kid` and `output.key`. CBCS HLS also requires `output.hls_key_uri` for playlist signaling. For HLS AES-128, omit `output.kid` and provide `output.key` plus `output.hls_key_uri`; segment encryption does not use an MP4 key identifier. For clear output, omit `output` entirely.

To explicitly generate PlayReady signaling, add this object inside `output`
alongside its `kid` and `key`, then use `--to cbcs --format dash`:

```json
"signaling": {"system": "playready", "license_url": "https://example.invalid/license"}
```

This first profile accepts a qualified clear MP4 containing H.264 video and
optional AAC audio from the same complete file. Existing PSSH/protection and
unknown opaque metadata are refused, including encoder tags whose preservation
has not been qualified. Other input formats, encryption targets, and DRM systems
are outside this profile; see [conversion support](conversion-support.md#explicit-playready-signaling).
Omitting `signaling` retains the existing conversion behavior.

For Widevine/PlayReady DASH or FairPlay HLS from supported clear transport-stream
HLS or the qualified clear MP4 profile, use the distinct `multi-drm` setting.
See [DRM signaling](drm-signaling.md) for complete settings and examples. The
older `playready` setting above keeps its original input restrictions.

The license URL must be an absolute HTTP(S) address without credentials,
fragments, whitespace, or control characters, at most 4096 UTF-8 bytes. Its exact
text is retained in the generated metadata; drmr does not canonicalize or request
it. It can contain sensitive query parameters: keep the operator JSON private
and treat the generated MPD and initialization segments as publishing that
address. The URL, PRO, and PSSH are excluded from logs and JSON reports.
The external packager's live process arguments can expose the PSSH and its
embedded URL to operating-system process inspection, as they can expose keys.
Generated metadata and known-key media verification do not establish license
acquisition or device playback; `license_playback_verified` remains false.

For SAMPLE-AES TS, the content key also needs its associated initialization
vector (IV). Supply `input.hls_keys[].iv` when the playlist does not provide an
explicit IV. If both are supplied, they must match. Use the IV associated with
that key; the segment number is not a substitute. A clear-output configuration
can contain only:

```json
{
  "schema_version": 1,
  "input": {
    "hls_keys": [{
      "uri": "<exact URI from the input playlist>",
      "key": "<32 hexadecimal characters>",
      "iv": "<32 hexadecimal characters>"
    }]
  }
}
```

```sh
drmr convert encrypted-ts/media.m3u8 --to clear --format hls --output clear-hls --keys-file keys.json
drmr verify clear-hls/master.m3u8 --source encrypted-ts/media.m3u8 --keys-file keys.json --json
```

To create FairPlay SAMPLE-AES transport-stream media from a supported clear TS
playlist, omit `output.kid` and supply the associated output IV:

```json
{
  "schema_version": 1,
  "output": {
    "key": "<32 hexadecimal characters>",
    "iv": "<32 hexadecimal characters>",
    "hls_key_uri": "skd://<content-identifier>"
  }
}
```

```sh
drmr convert clear-ts/media.m3u8 --to hls-sample-aes --format hls-ts --output protected-ts --keys-file keys.json
drmr verify protected-ts/master.m3u8 --source clear-ts/media.m3u8 --keys-file keys.json --json
```

For encrypted input, add its own entries in `input.hls_keys`. Input and output
keys and IVs remain separate, even when their advertised URI is identical.
`output.iv` is required for SAMPLE-AES output and refused for AES-128, MP4
sample encryption, or clear output. An associated IV from the protected
configuration also lets verification read a playlist that omits an explicit IV.

The advertised HLS URI is written to output playlists, so choose a location suitable for publication. AES-128 output uses fresh per-resource IVs; SAMPLE-AES output uses the explicit associated IV supplied by you. drmr never publishes a key file. Input key URI mappings, raw keys, and initialization vectors are excluded from reports and diagnostics. The external packager accepts raw keys in child-process arguments; an operating-system process inspector with sufficient access can observe them.

On macOS/Linux, use `chmod 600 keys.json`. Unknown fields, duplicate fields, unsupported schema versions, oversized configurations, links, and permissions readable by other users are refused. Multiple-output conversions use a schema-version-2 key file with shared `input` and output-ID-keyed `outputs`; see [multiple outputs](multiple-outputs.md).

## Experimental web console

The web console is a local operator interface without authentication. Keep it bound to loopback on a trusted machine.

```sh
DRMR_INPUT_ROOT=/absolute/path/media \
DRMR_OUTPUT_ROOT=/absolute/path/output \
drmr serve
```

Open `http://127.0.0.1:8080`. Both root directories are required. `DRMR_BIND_PORT` changes the port; `DRMR_MAX_WORKERS` caps workers for saved-plan operations. Automatic allocation stays within that cap. Select media and saved plans from the allowed input folder. See `drmr serve --help` for options. The optional [Desktop host](desktop.md) opens this same console in a native window.

### Saved-plan workflow

Choose **New conversion** to use the same saved plans as the CLI:

1. Select **Local media**, **Source URL** or **Saved CLI plan**, then **Open input**. URL input accepts the supported MP4, static DASH and completed HLS profiles. The picker lists only server/native allowed folders; it does not upload files or accept arbitrary filesystem paths.
2. For new media, select a **Key file** if needed and choose **Analyze**. URL input can select a **Fetch policy file** from the private configuration folder. The browser never retrieves either file's contents.
3. Add 1–8 named outputs, selecting format, encryption, DRM signaling and optional required manifest checks separately. Choose the output parent and a new folder name, then **Save plan**.
4. **Download original plan** preserves the original bytes and absolute paths. Keep the file private because it identifies local files and output locations. **Convert** creates new output and refuses an existing destination. Editing output settings requires another saved plan.
5. **Verify again** independently compares the plan and existing output without conversion. To verify later, open the exported CLI plan through **Saved CLI plan**; no new analysis or conversion is needed.

The server operator can set `DRMR_COMMON_CONFIG_ROOT` to a private configuration folder separate from input, output and custom static folders. The picker sends role-bound references; keys, configuration contents and executable paths are not HTTP inputs. No configuration folder is required for a clear flow without fetch settings.

The private configuration picker replaces file and folder names with numbered labels. Each page follows filename order; check the corresponding local folder before choosing a file. Configuration contents and original names are not shown in the console.

URL sessions use a generic **Source URL** label. The server does not return the submitted URL to the page; credentials can occur in the path as well as in query parameters. The input field is cleared when submitted.

Every newly saved console plan is v10, including a one-output list, and binds its processing, boundary behavior, source directories and media filenames, DASH references and minimum verification depth. The additional complete-decoding checkbox has the same meaning as CLI `--decode`; an imported plan can require it even when the checkbox is clear. When supplying a key file, analyze accepts either key-file schema; new console plan creation and named-output plan conversion/verification use v2 keys. Imported single-output v1/v3/v5/v7 plans use v1 keys. Legacy plan versions 1/2 retain a decoded minimum. See [multiple outputs](multiple-outputs.md) and the [key-file reference](https://kazuhide.github.io/drmr/guide/#cli-reference).

The common workflow fixes a request identifier before submitting an operation. A lost reply is recovered by GET lookup of that exact request and job; a missing live receipt remains unknown. **Check status** never repeats the POST. Stop acknowledgment is separate from confirmed process cleanup, and unconfirmed cleanup retains the global execution slot. Selection and language changes do not release it.

Each session admits at most 32 requests, including claimed requests later refused. At the limit, download the plan, explicitly **Close this conversion**, and open the saved plan in a new session for verification. Closing uses an admission fence: delayed requests for the old session cannot start work. A pending, running or cleanup-unconfirmed session cannot close. Closing the page alone is not this explicit action.

### Job state and shutdown

A lost launch response remains unknown until the same request and job can be looked up. **Check status** does not resubmit the operation. Stop requests termination of the owned job and its media tools; an acknowledgment alone does not establish cleanup. Input media, retained URL originals, saved plans and completed results are not deleted by Stop.

The server owns the job CLI and its managed tools. Unconfirmed process cleanup retains the execution slot and private configuration references, and prevents new work. Status and logs remain available for diagnosis. Use the same version of `drmr` for the server and its job CLI; if you set `--cli`, supply the executable directly.

On interruption or termination, the server closes submissions, stops active work, and waits for confirmed process cleanup before reporting successful shutdown. Rust embedding hosts should call `RunningServer::shutdown(&mut self)` and retain the owner if it returns an error; dropping a host is best effort. If shutdown remains incomplete, fix the reported cause and retry. Force-killing the server bypasses this confirmation.

Keep retained originals, plans, results and logs private. Closing a browser tab does not erase saved files or establish that a running operation has stopped.

## Exit codes

Most commands return `0` for success, `1` for a failed operation or validation, and `2` for invalid usage or configuration. `convert` and `verify` also return `130` after cooperative cancellation: on Unix, send SIGINT (Ctrl+C) or SIGTERM. They stop queued work and terminate managed media subprocesses before returning. Cancellation observed before publication prevents publishing the result. Force-killing the CLI bypasses cooperative cleanup; the measurement tool reports that fallback as cleanup unconfirmed.

`doctor` discovers executable files on the current launch environment's `PATH` without executing them. Add `--probe` to run required tools' version and help checks with a three-second deadline per call and a 1 MiB output limit per stream. Optional tools are discovered but not launched. Raw tool output is excluded from the report. Discovery returns `0` when required tools are found; probing additionally requires recognized supported versions and the advertised options. A missing or failed required check returns `1`; invalid arguments return `2`.

`doctor` checks native conversion requirements by default. `--route progressive-mp4`, `--route fmp4-segments` and `--route mpeg-ts-segments` need no external media tools; packaging routes require Shaka Packager. The default `--route all` requires the union of native conversion tools, currently Shaka Packager. Add `--decode` to check FFmpeg and ffprobe as well, plus the existing Shaka preparation requirements for TS decoding. Unrelated tools remain optional. Unconfirmed process cleanup always blocks readiness. For example:

```sh
drmr doctor --probe --route dash-presentation --json
drmr doctor --probe --route progressive-mp4 --decode --json
```

The single JSON object retains `ready` and `tools`, with each tool's `name`, `purpose`, `available`, and `required`. Added version-one fields include `mode` (`discovery` or `probe`), `profile`, resolved `path`, `path_status`, `version`, `version_status`, `feature_status`, individual `features`, fixed `failure_code`, `cancelled`, and `cleanup_status` (`not-required`, `confirmed`, or `unconfirmed`). `ready` covers the selected checks only; `media_compatibility_checked` remains false. Non-UTF-8 executable paths are reported without an ambiguous replacement path and are refused for probing. A successful diagnostic does not validate media, keys, licensing, or device playback.

If a diagnostic cannot confirm tool cleanup, the CLI retains the process owner
and retries cleanup in the foreground, pausing between failures. Ctrl+C or
termination requests Stop; it does not discard an unconfirmed owner. The final
report is printed after cleanup is confirmed, while the original probe failure
and `ready: false` remain. `cleanup_status` describes that final cleanup state.
An operating-system hard kill or power loss cannot preserve this ownership.

### Automatic worker limits

Full `analyze`, `plan --save`, `convert` and `verify` choose concurrency from
processor capacity, available memory and observed media dimensions. Use
`--max-workers 1` to run independent jobs one at a time, or another value from 1
to 64 as an upper bound. Resource admission may choose fewer.
The same content and timing checks run in either mode. See [resource limits and
measurements](resources.md) for the estimates and a repeatable comparison.

Full `analyze` also performs work after the source download: it prepares tracks
and checks all selected encoded samples, configuration, clocks and playback windows in Rust. `--decode` also checks complete decoded media. The worker count does not
mean every stage runs concurrently. For a quick metadata or declaration check,
use `analyze --metadata-only`; this does not establish that complete source media
can be converted. See [full analysis time](resources.md#full-analysis-time).
