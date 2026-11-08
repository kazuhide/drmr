# Resuming URL acquisition

Available from version 1.3.0.

Use `--resume-download` with `analyze`, a saved `plan`, or direct `convert` when
you want to keep completed original resources after an interrupted download.
The flag must be present on the **first attempt**. For example, for clear input:

```sh
drmr analyze https://example.com/vod/stream.m3u8 \
  --download-dir source --resume-download --json
```

If acquisition fails, repeat the same command with the same URL and download
directory. The usual fetch settings, input keys and output settings still apply;
add `--fetch-config`, `--keys-file` or output options as appropriate. Do not add
keys when the input and requested outputs are clear.

For a saved plan, keep the normal plan options and add the flag:

```sh
drmr plan https://example.com/vod/stream.m3u8 \
  --download-dir source --resume-download \
  --encryption clear --output-format hls-fmp4 \
  --output-dir result --save conversion.plan.json
```

Direct conversion uses the same option:

```sh
drmr convert https://example.com/vod/stream.m3u8 \
  --download-dir source --resume-download \
  --output-config outputs.json --keys-file keys.json --output-dir result
```

The [configuration guide](configuration.md) describes the output and key files.
This does not extend the supported [URL input profile](conversion-support.md#url-input).

## What is reused

- HLS playlists and DASH manifests are downloaded on each attempt. Their original
  bytes and effective URLs after redirects must still match the first attempt.
- A completely downloaded media resource with a strong ETag can be checked with
  a conditional GET. An ETag is a server-supplied identifier for a particular
  representation. drmr sends its validator only to the same effective URL. A
  matching `304 Not Modified` response permits copying the cached original;
  drmr still verifies its local bytes, size and file identity.
- With no supported strong ETag, the whole resource is downloaded again and
  compared against the saved bytes. Weak ETags do not authorize reuse.
- Self-contained MP4 input uses the same completed-resource checks. A partial
  file is downloaded again from the beginning; this is not byte-range resume.

Changed manifests, effective URLs, media bytes or recorded strong validators are
refused rather than mixed into an older source. Choose a new download directory
to acquire the changed source. A changed signed URL also needs a new destination because the
cache is bound to the complete original URL, including its query string.

Every request still uses the current allowed origins, authentication, redirect
rules, pinned network addresses, timeouts, cancellation and resource limits.
Reused bytes count against the current logical source-size limits even when no
media body is received. Existing files and completed download directories are
never overwritten or adopted.

## Storage and completion

For `--download-dir source`, completed originals are stored in a private hidden
sibling directory named `.source.drmr-download`. The cache contains independent
copies, not hard links into the working source. This requires extra disk space
while downloading, potentially another copy of the retained source. Copies use
bounded buffers and share the normal concurrent free-disk floor.

An ordinary failed acquisition removes only its owned incomplete source files;
the completed-resource cache remains. After every worker has stopped and the
complete retained source has passed its publication checks, drmr removes the
recognized cache. Modified, replaced or unknown files prevent automatic cleanup
and are preserved. Only one process may use a given cache at a time. Missing or
unsafe cache metadata is refused, not repaired.

Each cache-owned object is bound to both its native file identifier and a random
identifier assigned when it is created. This detects replacements even when the
filesystem reuses a deleted file's identifier. The random identifier is stored
in a filesystem extended attribute; missing or changed attributes prevent reuse
and cleanup. Resume therefore requires a filesystem that supports these
attributes. If that support is unavailable, use another destination or omit
`--resume-download` to start a normal acquisition.

Abrupt process termination can leave an incomplete download directory or private
working directories. drmr does not adopt these leftovers. After confirming no
acquisition is active, move the incomplete download directory aside before
retrying the same destination; keep its sibling cache. Inspect abandoned working
directories separately rather than deleting unknown files automatically.

The cache holds original media, which may be clear or encrypted, and original
manifests, which may include signed URLs. Keep it private just like the retained
source. Ownership records use URL hashes and bounded validators; they do not
save request headers or encryption keys. These records detect unexpected changes
under a trusted account; they do not protect against an attacker controlling
that account or provide encrypted storage. See [security](security.md).

If acquisition completed and a later analysis, planning or conversion step
failed, use the retained local entrypoint as input. Omit `--download-dir` and
`--resume-download` in that case. This flag resumes acquisition, not conversion
or a saved plan's execution. It is unavailable with `--metadata-only`, local
input or `convert --plan-file`.
