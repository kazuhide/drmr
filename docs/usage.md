# Using drmr

The examples below assume `drmr` is installed and the external tools are on `PATH`. Run `drmr doctor` first. Commands process existing media; they do not encode new video qualities.

## Inspect media and plan a conversion

```sh
drmr inspect media.mp4
drmr inspect master.m3u8 --json
drmr plan manifest.mpd --to cbcs --format dash,hls --json
```

`inspect` detects local MP4, DASH, and HLS by their contents. The JSON report has `schema_version: 1`, a format, protection per track or sample description, known key identifiers, DRM system labels, and explanation tokens in `findings`. It excludes encryption keys, initialization vectors, license URLs, and key URLs. Generated track identifiers are local to the report.

Manifest reports describe declarations. MP4 reports read initialization metadata and skip media payloads. Both return `media_verified: false`; they do not decrypt, decode, validate every sample, or prove device playback. Unsupported or incomplete protection remains `unknown`. An HLS `SAMPLE-AES` declaration alone does not establish the underlying MP4 encryption scheme.

HLS inspection follows only local child playlists inside the input playlist's directory tree. It never fetches remote playlists, media segments, or keys. Remote, missing, cyclic, and escaping child references remain unresolved. Limits are 4 MiB per manifest, 16 MiB total, 32 playlists, and 128 references. Each track permits at most 64 key identifiers; DASH also limits inherited protection declarations to 64, and HLS permits at most 64 active key formats. MP4 movie metadata is limited to 16 MiB. This command is separate from `analyze-hls`, which downloads and probes media under an explicit source policy.

`plan INPUT --to clear|cenc|cbcs|hls-aes128` describes the requested operations and independent input/output key roles. `--format mp4|dash|hls` selects output packaging; comma-separated values are accepted. Omitting it retains the input format. An HLS AES-128 target requires HLS output. CENC output to HLS is currently rejected. Unsupported source protection or unresolved encryption is reported as incompatible.

A compatible plan describes a valid format/protection combination. `execution_available` indicates whether a backend route exists for that request; execution still checks the actual source profile, keys, and media. Multiple requested output formats are planned together but must be converted in separate invocations. Plans contain no secret key values and cannot be submitted as trusted execution artifacts.

## Convert and verify local media

```sh
drmr convert source.mpd --to cbcs --format dash --output protected-dash --keys-file keys.json
drmr convert source.mpd --to hls-aes128 --format hls --output protected-hls --keys-file segment-keys.json
drmr verify protected-dash/manifest.mpd --source source.mpd --keys-file keys.json --json
```

`convert` detects local MP4, DASH, and HLS inputs. Choose one output format per invocation; omitting `--format` retains the input format. The output path is a new MP4 file or a new DASH/HLS directory. Existing destinations are refused. Input media is read into a private working area, and output is published only after content and timing checks pass. Nothing is re-encoded.

`clear`, `cenc`, and `cbcs` can be written to DASH; `clear` and `cbcs` can be written to fragmented MP4 HLS. HLS `AES-128` encrypts complete HLS resources and requires `--format hls`. A single-track MP4 can also be written as `clear`, `cenc`, or `cbcs`. Multiplexed MP4 output, live input, MPEG transport stream input, and unrestricted DASH/HLS profiles are not supported by this command. See [conversion support](conversion-support.md) for the tested routes and [processing and verification](media-processing.md) for timing limits.

`verify OUTPUT --source INPUT` independently compares the original source and result. It requires their respective keys when encrypted. Success means the encoded packets, decoded picture/sound, and packet timestamps match; it does not establish license acquisition or device playback. Use the generated `manifest.mpd` or `master.m3u8` as the output argument for a presentation. Both commands support `--json` for one versioned result object; progress and failures go to standard error.

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

`input.keys` maps MP4 key identifiers (KIDs) to decryption keys. `input.hls_keys` maps each exact HLS AES-128 key URI to a key supplied by you; drmr never retrieves those URIs. Omit unused input entries. For `cenc`/`cbcs`, provide `output.kid` and `output.key`. CBCS HLS also requires `output.hls_key_uri` for playlist signaling. For HLS AES-128, omit `output.kid` and provide `output.key` plus `output.hls_key_uri`; segment encryption does not use an MP4 key identifier. For clear output, omit `output` entirely.

The advertised HLS URI is written to output playlists, so choose a location suitable for publication. drmr generates fresh output initialization vectors and never publishes a key file. Input key URI mappings, raw keys, and initialization vectors are excluded from reports and diagnostics. The external packager accepts raw keys in child-process arguments; an operating-system process inspector with sufficient access can observe them.

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

On macOS/Linux, `drmr batch --help` lists work-list, storage, restart-log, and retry options. A work list contains one JSON object per line: first `{"schema_version":"1"}`, then rows with exactly `asset_id`, `kind`, `dash_manifest`, and `hls_master`. `kind` is `dash-cenc` or `hls-clear`; manifest references are relative storage object keys, with unused references set to `null`.

Use `--plan` to inspect planned work before processing. The default key provider reads the three environment variables. To use per-asset keys, pass `--key-provider file --key-file /protected/keys.jsonl`. Each line in that file has exactly `asset_id`, `kid`, `key`, and `iv`; there is no schema header. All three key fields use the hexadecimal formats above. The file must have mode `0600`, must not be a symbolic link, and must be outside the output directory.

## Experimental web console

The web console is a local operator interface without authentication. Keep it bound to loopback on a trusted machine.

```sh
DRMR_CATALOG_ROOT=/absolute/path/catalog \
DRMR_OUTPUT_ROOT=/absolute/path/output \
drmr serve
```

Open `http://127.0.0.1:8080`. Both root directories are required. `DRMR_BIND_PORT` changes the port; `DRMR_MAX_WORKERS` limits HLS packaging workers. The console requires catalog entries prepared for it; it is not a general file browser. See `drmr serve --help` for options. A standalone desktop application is not shipped.

Requests must address `localhost`, `127.0.0.1`, `::1`, or an explicitly configured bind hostname/address. Binding to a wildcard address does not permit arbitrary hostnames. Reverse proxies and remote access are outside this local console's default setup.

## Exit codes

Most commands return `0` for success, `1` for a failed operation or validation, and `2` for invalid usage or configuration. `verify-snapshot` returns `1` for changed content and `2` for an invalid snapshot. Batch processing can return `130` when interrupted.

`doctor` returns `0` when all four tools are found and `1` when any are missing. Its `--json` output is a single JSON object with `ready` and `tools`; each tool has `name`, `purpose`, `available`, and `required` fields. Finding a tool does not verify its version or media compatibility.
