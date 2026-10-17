# Resource limits and measurements

`--max-workers` is an upper bound. The subprocess backend can choose fewer workers when
the media or available memory makes that safer. It also shares subprocess reservations
between track, segment, and source-probe pools in the same process.

## Choosing workers

The budget uses the following facts. A MiB is 1,048,576 bytes.

- Processor (CPU) parallelism available to the process, including readable Linux CPU quotas.
- Available physical memory, limited by the current process's Linux control group and
  its ancestors when those limits are readable. A control group is an operating-system
  resource allocation used by containers and services.
- The largest admitted video dimensions, source resource size, and track count.

For known memory, the budget reserves a quarter of available memory, at least 256 MiB,
plus an estimated 8 MiB of retained state per track. It reserves a quarter of CPU
parallelism, rounded up, while always allowing at least one CPU token. Default worker
recommendations stop at 32; explicit maximums must be between 1 and 64.

Each child is estimated to need 64 MiB, 24 bytes per video pixel, twice the largest
source resource size, and four times its configured output capture caps. Unknown video
dimensions assume 3840 × 2160 pixels; an unknown resource size assumes 32 MiB. Audio-only
sources omit the video term. If memory cannot be queried, the allowance is 512 MiB,
including estimated track state, with one child at a time. A workload that cannot fit
one estimate fails with `resource-budget`.

These are conservative admission estimates, not operating-system memory or CPU quotas.
Codecs, allocators, process helpers, and retained result metadata can use more memory.
The shared capacity is a point-in-time snapshot; memory pressure can change afterward.
Pool recommendations take another snapshot at admission. Separate CLI processes do not
share reservations. Batch processing currently runs one asset worker at a time.

The media backend explicitly sets FFmpeg decoder, encoder, and filter thread counts to
one, and FFprobe decoder threads to one. One CPU token represents that codec allocation;
tools may still create helper threads. Streamed decoded-media hashing holds the same
reservation as commands that capture their output.

Structured `resource-budget` records show chosen workers, child thread allocation,
CPU tokens, estimated memory tokens, and whether memory came from the host, the process
control group, or the unknown fallback. They contain no source paths or machine IDs.

## Output and pending work

Captured stdout defaults to 16 MiB and stderr to 1 MiB, independently. Rust callers can
set `RunOptions.max_stdout_bytes` and `max_stderr_bytes`; each has a hard maximum of
64 MiB. The actual caps are included in the memory charge. Log character limits are
separate: disabling output logging does not disable capture limits.

Streaming video checksums also bound each frame record to 64 KiB, including comment
lines and a final line without a newline. An oversized record fails verification; it
is never truncated and accepted as evidence. Audio digests consume fixed-size chunks.

Overflow fails with `output-limit`; no partial output is returned as successful data.
The media adapter reports `media-output-limit`, and source probing reports
`media-probe-output-limit`. Captured pipe readers are joined before a reservation is
returned. A command deadline includes resource waiting. Cooperative cancellation stops
a waiting reservation; work already running keeps its normal drain behavior.

Pipes left open after the direct child exits get a two-second close deadline, including
calls without an explicit command timeout. Windows jobs retain ordinary descendants;
Unix uses an owned process group, or the inherited supervisor group inside an asset
worker. A Unix child that deliberately starts a separate session can escape group
termination; nonblocking pipe readers still stop and join at the capture deadline.

Ordered task producers submit at most 128 unconsumed tasks per coordinator. Source
downloads and probes share a 128-task window, including probes waiting for initialization
data. Track coordinator threads also follow the worker maximum. Ordered results and
source metadata are retained, so total memory still grows with tracks and segments.

## Repeating measurements

Build and measure fresh capture-test processes, separately from compilation:

```sh
python3 tools/measure_capture.py --output capture-measurements.json
```

Use the project's Rust toolchain on `PATH`. The default runs five processes; each child
attempts 32 MiB on stdout and 32 MiB on stderr. The report samples the Rust parent
process's resident memory every 10 ms on macOS and Linux. Unsupported or missed samples
are explicitly `unmeasured`. This is sampled resident memory, not an exact peak or the
combined child process memory. The expected bounded result is `output-limit`.

On one macOS ARM64 host, the original read-to-end capture completed that workload with
a sampled parent median of 240.35 MB over five fresh processes. Bounded capture and
resource admission rejected it with a sampled median of 27.74 MB. The outcomes differ:
this demonstrates rejection of oversized output, not faster processing of equal work.

For real media, follow the [Big Buck Bunny regression guide](media-processing.md).
Keep tool versions, build profile, worker maximum, fixture, and host load the same when
comparing runs. Check complete decoded media and packet timing before comparing time.
First launches and filesystem caches can materially change these short measurements.
The ten-second fixture is a correctness and startup check, not a long-media throughput
benchmark or playback/device certification.

The same host, Rust 1.99 debug build, FFmpeg 9.0.2, Shaka Packager 3.8.0, and two-worker
maximum produced these warm-run ranges (two warm runs after one first launch):

| Ten-second BBB operation | Before resource controls | With resource controls |
| --- | ---: | ---: |
| Analyze HLS | 0.076–0.083 s | 0.087–0.090 s |
| Package HLS | 1.110–1.137 s | 1.175–1.192 s |

Every run preserved the full decoded video, audio, and packet timeline. These short runs
show some admission/capture overhead; they do not demonstrate a throughput improvement.
First analysis launches took about 0.57 s in both measurements.
