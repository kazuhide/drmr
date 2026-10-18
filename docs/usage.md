# Using drmr

The examples below assume `drmr` is installed and the external tools are on `PATH`. Run `drmr doctor` first. Commands process existing media; they do not encode new video qualities. For PowerShell examples and Windows file permissions, see the [Windows guide](windows.md).

## Choose a workflow

For qualified local MP4, DASH, or HLS media, start with the common commands:
`inspect` reads metadata, `plan` describes intent, `convert` writes new media,
and `verify` compares a result with its original source. HLS input does not
require `analyze-hls` first.

The separate HLS snapshot workflow saves clear media from a local or HTTPS
source under an explicit access policy. `analyze-hls` downloads or copies the
resources, probes them, and writes a snapshot: saved media plus its analysis
record. `verify-snapshot --files` checks that those saved bytes still match the
record. `package-hls` packages that verified snapshot with DRM signaling and can
produce **both DASH and HLS**, despite its name. This workflow also has supported
subtitle profiles; it is not an alias for generic `convert`.

A snapshot integrity check detects changed saved files. It does not replace
`verify OUTPUT --source INPUT`, which compares complete media and timing.
`run` and `batch` serve the separately prepared catalog layouts described below.

## Inspect media and plan a conversion

```sh
drmr inspect media.mp4
drmr inspect master.m3u8 --json
drmr plan manifest.mpd --to cbcs --format dash,hls --json
```

`inspect` detects local MP4, DASH, and HLS by their contents. The JSON report has `schema_version: 1`, a format, protection per track or sample description, known key identifiers, DRM system labels, and explanation tokens in `findings`. It excludes encryption keys, initialization vectors, license URLs, and key URLs. Generated track identifiers are local to the report.

Manifest reports describe declarations. MP4 reports read initialization metadata and skip media payloads. Both return `media_verified: false`; they do not decrypt, decode, validate every sample, or prove device playback. Unsupported or incomplete protection remains `unknown`. An HLS `SAMPLE-AES` declaration alone does not establish the underlying MP4 encryption scheme.

HLS inspection follows only local child playlists inside the input playlist's directory tree. It never fetches remote playlists, media segments, or keys. Remote, missing, cyclic, and escaping child references remain unresolved. Limits are 4 MiB per manifest, 16 MiB total, 32 playlists, and 128 references. Each track permits at most 64 key identifiers; DASH also limits inherited protection declarations to 64, and HLS permits at most 64 active key formats. MP4 movie metadata is limited to 16 MiB. This command is separate from `analyze-hls`, which downloads and probes media under an explicit source policy.

`plan INPUT --to clear|cenc|cbcs|hls-aes128|hls-sample-aes` describes the requested operations and independent input/output key roles. `--format mp4|dash|hls|hls-ts` selects output packaging; comma-separated values are accepted. Omitting it retains the input format. An HLS AES-128 target requires `hls` or `hls-ts` output. SAMPLE-AES TS requires `hls-ts`. `cenc` and `cbcs` are refused with TS output. CENC output to HLS is currently rejected. Unsupported source protection or unresolved encryption is reported as incompatible, except that a consistent SAMPLE-AES declaration can produce conditional HLS routes. Those routes still require the appropriate keys, IVs, and actual media validation.

A compatible plan describes a valid format/protection combination. `execution_available` is a coarse version-one packaging check, not proof of a qualified media profile or a selected handler. The additional candidate list can be empty when declared track roles are unsupported. Execution still checks the actual source profile, keys, and media. Multiple requested output formats are planned together but must be converted in separate invocations. Plans contain no secret key values and cannot be submitted as trusted execution artifacts.

Optional `details` distinguish declared ciphers, encryption units, default patterns, and unknown sample coverage. Plans add conditional routes and required toolsets; successful conversion reports add the actual route and reason. See [processing routes](processing-routes.md) to compare the plan with the result. A tool requirement does not mean its availability was checked.

## Convert and verify local media

```sh
drmr convert source.mpd --to cbcs --format dash --output protected-dash --keys-file keys.json
drmr convert source.mpd --to hls-aes128 --format hls --output protected-hls --keys-file segment-keys.json
drmr verify protected-dash/manifest.mpd --source source.mpd --keys-file keys.json --json
```

`convert` detects local MP4, DASH, and HLS inputs. It also accepts the completed clear HLS URLs described below. Choose one output format per invocation; omitting `--format` retains the input format. The output path is a new MP4 file or a new DASH/HLS directory. Existing destinations are refused. Input media is read into a private working area, and output is published only after content and timing checks pass. Nothing is re-encoded.

`clear`, `cenc`, and `cbcs` can be written to DASH; `clear` and `cbcs` can be written to fragmented MP4 HLS. HLS `AES-128` encrypts complete HLS resources and requires `--format hls` or `hls-ts`. `hls-ts` also supports clear and SAMPLE-AES output from qualified local TS inputs. A single-track MP4 can also be written as `clear`, `cenc`, or `cbcs`.

A qualified clear or counter-mode encrypted (`cenc`) MP4 containing one AAC-LC audio track and optional H.264 video can be copied into one clear progressive MP4 while preserving its finite audio end edit. Both tracks must come from the same original presentation. Encrypted input requires every track key in the input key section; this finite profile produces clear output and refuses DRM initialization headers (PSSH).

A narrow local MPEG transport stream input profile can produce clear, `cbcs`, or AES-128 fragmented MP4 HLS. Use `--format hls-ts` to retain transport-stream packaging, including SAMPLE-AES encryption for FairPlay media. Other multiplexed MP4 output, live input, and unrestricted DASH/HLS profiles remain unsupported. See [conversion support](conversion-support.md) for the tested routes and [processing and verification](media-processing.md) for timing limits.

`verify OUTPUT --source INPUT` independently compares the original source and result. It requires their respective keys when encrypted. Success means the stream metadata, encoded packets, decoded picture/sound, and packet timestamps match; it does not establish license acquisition or device playback. Use the generated `manifest.mpd` or `master.m3u8` as the output argument for a presentation. Both commands support `--json` for one versioned result object; progress and failures go to standard error.

### Convert a URL

For a completed clear HLS source, pass its URL directly to `convert` and choose
where to retain the downloaded source:

```sh
drmr convert "https://example.com/vod/master.m3u8" --download-dir ./source --to cbcs --format hls --output ./converted --keys-file keys.json
```

`--download-dir` is required for URL input. Both it and `--output` must be new,
separate paths under existing parent directories. Existing downloads are never
replaced or silently reused. Local input needs neither `--download-dir` nor
`--fetch-policy`.

The command downloads the original playlists and resources, creates
`source/input.m3u8` with local references, then converts and verifies the media
against that saved source. Original playlist bytes remain separate; the local
playlists change only resource references. No separate analysis or snapshot
command is required.

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

The initial URL profile covers clear, completed HLS with supported MPEG-TS or
fragmented MP4 media. Existing local-media restrictions still apply. Remote DASH,
standalone MP4, encrypted input, live playlists, and byte ranges are outside this
URL profile; this is an implementation limit, not a limitation of those formats.
No playlist key URI is retrieved.

#### Optional fetch settings

By default, retrieval uses HTTPS and the input URL's origin: its scheme, host,
and port. A playlist that references another origin needs explicit permission.
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

URL conversions add a `download` object to the JSON result, containing local
`directory` and `entrypoint` display paths, the relative `entrypoint_relative`,
and `retained: true`. Source URLs and request headers are excluded. Local
conversions retain their existing result shape.

### Separate input and output keys

New commands use an owner-only JSON configuration, with no environment fallback. Store this outside the output tree and version control. Here is its structure using placeholders; replace each placeholder with your own hexadecimal value before use:

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

On macOS/Linux, use `chmod 600 keys.json`. Unknown fields, duplicate fields, unsupported schema versions, oversized configurations, links, and permissions readable by other users are refused. Legacy commands below retain their existing configuration interfaces.

## List a prepared DASH asset

`plan` and `run` use a downloaded media tree. URLs in the DASH manifest resolve to local files under `download/<host>/<path>`. For example:

```text
sample/
  download/
    manifest.example.test/sample/dash/manifest.mpd
    manifest.example.test/sample/hls/manifest.m3u8
    cdn.example.test/sample/video/v0/init.mp4
    cdn.example.test/sample/video/v0/seg_1.m4s
    cdn.example.test/sample/video/v0/seg_2.m4s
```

The DASH manifest needs a `BaseURL`, MP4 adaptation sets, and a `SegmentTemplate` on each representation with `initialization`, `media`, `timescale`, and `duration`. Media templates use `$Number$`; segment filenames need numeric indices. For the tree above, a `BaseURL` of `https://cdn.example.test/sample/` and an initialization path of `video/v0/init.mp4` resolve to the shown local file. The source HLS master playlist is also needed when generating output playlists.

```sh
drmr plan --mpd sample/download/manifest.example.test/sample/dash/manifest.mpd
```

The command lists supported video and audio renditions and their segment counts. It writes nothing and needs no keys. It is not a full media validation.

## Provide key material

`run` and `package-hls` read these environment variables:

| Variable | Meaning | Format |
| --- | --- | --- |
| `DRM_KID` | Key identifier | 16 bytes written as 32 hexadecimal characters |
| `DRM_KEY` | Content encryption key | 16 bytes written as 32 hexadecimal characters |
| `DRM_IV` | Initialization vector used for output encryption | 16 bytes written as 32 hexadecimal characters |

Supply them through your local secret-management process before running the command. drmr has no command-line flags for key values. Keep keys out of shell history, work lists, and version control. The DASH conversion retains the supplied key identifier and key while changing the encryption scheme.

`package-hls --drm-config` contains signaling, not encryption keys. Batch mode additionally supports a protected key file, described below.

## Repackage a local DASH asset

With the three environment variables already set:

```sh
drmr run \
  --mpd sample/download/manifest.example.test/sample/dash/manifest.mpd \
  --hls-master sample/download/manifest.example.test/sample/hls/manifest.m3u8 \
  --output output \
  --asset-id sample
```

Output is created beneath `output/sample/`. Use `--log-format jsonl` for one JSON record per line. `--max-workers` limits how many renditions run concurrently. Use `--packager-bin` to specify a directory containing an executable named `packager`, or the executable itself.

`--no-manifests`, `--no-round-trip`, and `--limit-renditions` skip work or checks for debugging. A run using them provides less validation or incomplete output. See `drmr run --help` for details.

## Analyze clear HLS

Save this as `source.json`, replacing the paths with absolute paths to your media:

```json
{
  "url": "/absolute/path/media/master.m3u8",
  "policy": {
    "allowed_local_roots": ["/absolute/path/media"]
  }
}
```

The configuration must be a JSON object readable only by its owner. On macOS/Linux:

```sh
chmod 600 source.json
drmr analyze-hls --source-config source.json --asset-id sample --output output
```

The snapshot, including `analysis.json`, is saved under `output/sample/clear/source/`. Use a fresh asset identifier or output location for another analysis; an existing snapshot is not overwritten.

For HTTPS input, replace the configuration with:

```json
{
  "url": "https://media.example.test/master.m3u8",
  "policy": {
    "allowed_origins": ["https://media.example.test"]
  }
}
```

Allow every origin used by playlists, segments, and redirects. An origin is the URL scheme plus hostname and optional port. Local resources and remote origins are denied unless explicitly allowed. HTTP and private-network destinations are disabled by default.

Optional policy fields include `headers_by_origin` (an object mapping allowed origins to request-header objects), `connections` and `probe_workers` (1–64), `attempts` (1–5), `timeout` in seconds, `max_playlist_bytes`, `max_playlists`, `max_segments_per_track`, `max_resource_bytes`, `max_total_bytes`, `max_total_resources`, and `min_free_disk_bytes`. Limits must be positive except the minimum free-space value, which may be zero. `allow_http` and `allow_private_network` can explicitly enable those destinations. Keep any configuration containing request credentials owner-only.

A server may close an idle connection between segment downloads. If a reused connection
fails before response headers arrive, drmr replaces it once without spending a configured
attempt. Fresh-connection failures, response-body errors, and request timeouts still use
the normal attempt limit. Windows can report the closed idle socket as an aborted connection;
drmr recognizes this platform difference. See [Windows socket errors](https://learn.microsoft.com/en-us/windows/win32/winsock/windows-sockets-error-codes-2).

Analysis supports only clear, completed on-demand presentations. Encrypted playlists, live input, discontinuities, unsupported tracks, and unsupported subtitle profiles are rejected.

## Package the HLS snapshot

Save DRM signaling in `drm.json` using the values issued for your content. This template contains placeholders and is not ready to run:

```json
{
  "widevine_pssh": "<hex or base64 Widevine initialization data>",
  "playready_la_url": "https://license.example.test/rightsmanager.asmx",
  "fairplay_uri": "skd://<content-identifier>"
}
```

`widevine_pssh` is the Protection System Specific Header (PSSH), initialization data used by a DRM system. A full Widevine PSSH box or a Widevine payload containing key identifiers is accepted in hexadecimal or base64. Its identifiers must match `DRM_KID`. `playready_la_url` is the PlayReady license acquisition URL; `fairplay_uri` identifies the FairPlay content key.

With valid signaling and the three key environment variables set:

```sh
chmod 600 drm.json
drmr package-hls \
  --analysis output/sample/clear/source/analysis.json \
  --drm-config drm.json
```

The command verifies the snapshot before packaging and writes output under the same asset directory. It emits progress as one JSON object per line. `--max-workers` limits concurrent segment packaging; `--packager-bin` accepts the packager executable path.

When audio selection is required, create an owner-only JSON file containing `audio_source_id` from the snapshot's track identifiers and pass `--selection selection.json`. You can also include `analysis_id` and `snapshot_digest` from that snapshot to detect a stale selection.

```sh
drmr verify-snapshot --files output/sample/clear/source
```

`--files` verifies the saved playlist and segment bytes as well as the analysis record.

## Batch processing

`drmr batch --help` lists work-list, storage, restart-log, and retry options. On Windows, use the [regular-user and private-directory conditions](windows.md). A work list contains one JSON object per line: first `{"schema_version":"1"}`, then rows with exactly `asset_id`, `kind`, `dash_manifest`, and `hls_master`. `kind` is `dash-cenc` or `hls-clear`; manifest references are relative storage object keys, with unused references set to `null`.

Use `--plan` to inspect planned work before processing. The default key provider reads the three environment variables. To use per-asset keys, pass `--key-provider file --key-file /protected/keys.jsonl`. Each line in that file has exactly `asset_id`, `kid`, `key`, and `iv`; there is no schema header. All three key fields use the hexadecimal formats above. The file must not be a symbolic link and must stay outside the output directory. Use mode `0600` on macOS/Linux; on Windows, apply the [protected current-user-only access control](windows.md#protect-configuration-files).

## Experimental web console

The web console is a local operator interface without authentication. Keep it bound to loopback on a trusted machine.

```sh
DRMR_CATALOG_ROOT=/absolute/path/catalog \
DRMR_OUTPUT_ROOT=/absolute/path/output \
drmr serve
```

Open `http://127.0.0.1:8080`. Both root directories are required. `DRMR_BIND_PORT` changes the port; `DRMR_MAX_WORKERS` limits HLS packaging workers. The console requires catalog entries prepared for it; it is not a general file browser. See `drmr serve --help` for options. A standalone desktop application is not shipped.

For clear HLS added through the console, the server saves source URLs, original playlists, and downloaded media in `DRMR_OUTPUT_ROOT/<asset-id>/clear/source/`. Keep this private folder protected: the saved information can contain signed URLs. Temporary request configuration files are separate; the server attempts to remove them after jobs finish, without deleting the saved source. Removing a job from history hides its record; saved sources, packaged media, and logs remain on disk.

Requests must address `localhost`, `127.0.0.1`, `::1`, or an explicitly configured bind hostname/address. Binding to a wildcard address does not permit arbitrary hostnames. Reverse proxies and remote access are outside this local console's default setup.

## Exit codes

Most commands return `0` for success, `1` for a failed operation or validation, and `2` for invalid usage or configuration. `verify-snapshot` returns `1` for changed content and `2` for an invalid snapshot. Batch processing can return `130` when interrupted. `convert` and `verify` also return `130` after cooperative cancellation: on Unix, send SIGINT (Ctrl+C) or SIGTERM. They stop queued work and terminate managed media subprocesses before returning. Cancellation observed before publication prevents publishing the result. Force-killing the CLI bypasses cooperative cleanup; the measurement tool reports that fallback as cleanup unconfirmed.

`doctor` returns `0` when all four tools are found and `1` when any are missing. Its `--json` output is a single JSON object with `ready` and `tools`; each tool has `name`, `purpose`, `available`, and `required` fields. Finding a tool does not verify its version or media compatibility.

### Automatic worker limits

`convert` and `verify` choose concurrency from processor capacity, available memory,
and observed media dimensions. Use `--max-workers 1` to process one track at a time,
or another value from 1 to 64 as an upper bound. Resource admission may choose fewer.
The same content and timing checks run in either mode. See [resource limits and
measurements](resources.md) for the estimates and a repeatable comparison.
