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

Each child is estimated to need 64 MiB, 24 bytes per video pixel, twice its
estimated input working set, and four times its configured output capture caps. Unknown video
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
waiting reservations and active captured tools. The supervisor terminates its owned
process group or Windows job, reaps the direct child, and joins the pipe readers before
returning the reservation.

Generic `convert` and `verify` register Unix SIGINT and SIGTERM before starting tools.
The same cancellation flag reaches track scheduling, resource admission, and active
media commands; cancellation observed before final publication prevents that publication.
Synchronous file operations finish their current operation before the next check.
The CLI returns `130` after cooperative cancellation and confirmed process cleanup.
On Windows, Ctrl+C and Ctrl+Break use the same cooperative path. Native tests deliver
both console events to `convert` and `verify`, then check descendant termination,
released file locks, removed workspaces, and that output was not published. A detached
media descendant cannot receive the console event, so these checks also exercise the
CLI's own process-tree cleanup.

The conversion measurement tool first requests cooperative termination and allows five
seconds for the CLI to return. Its `cooperative-exit` label records the CLI's cancellation
exit code. A forced fallback is marked `cleanup: unconfirmed`: killing only the CLI's
process group cannot cover media children that own separate groups. The label is not
independent operating-system proof of every descendant's exit.

If pipes remain open after the direct child exits, cleanup starts after two seconds,
including calls without an explicit command timeout. Windows jobs retain ordinary descendants;
Unix uses an owned process group, or the inherited supervisor group inside an asset
worker. A Unix child that deliberately starts a separate session can escape group
termination; nonblocking pipe readers still stop and join at the capture deadline.
On Windows, the 30-second job-notification wait does not bound reader joins when operating-system
I/O remains blocked. Unconfirmed cleanup is an error, never a completed cancellation;
see [Windows cleanup limits](windows.md#unconfirmed-process-cleanup).

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

## Generic conversion

`convert` and `verify` accept `--max-workers 1` through `64` as an upper bound.
With no option, independent tracks share the CPU and memory admission described above.
Snapshots and stream metadata are checked before parallel normalization. Results retain
presentation order, and a failed track prevents publication. All content checks still run.

Whole-track inputs are streamed: the estimate includes up to a 32 MiB input window,
not the entire file size. Segment-based pipelines include their largest retained input
resource. Video dimensions, capture caps, and track metadata are charged separately.
Unknown media uses the conservative fallback until observed stream metadata is available.
This estimate does not enforce an absolute resident-memory limit on third-party tools.

FFmpeg and ffprobe use one decoder thread; FFmpeg output encoders and filters also use
one thread. Shaka Packager uses `--single_threaded` and `--io_cache_size=0`, disabling its
otherwise separate per-file threaded I/O caches. Parallel work is coordinated outside
these tools. The tradeoff is measured with the same complete verification in both modes.

For a paired worker comparison, build the CLI first, then run:

```sh
cargo build --release -p drmr
python3 tools/measure_conversion.py --drmr target/release/drmr \
  --input input/manifest.mpd --keys-file keys.json --to cbcs --format dash \
  --report worker-comparison.json
```

Use `drmr.exe` on Windows. The tool alternates automatic and one-worker runs, creates
fresh outputs, and independently verifies every result. Reports omit operator paths,
arguments, and raw diagnostics. The input-entry hash covers that file only, including
just the manifest when one is supplied. Referenced media is not fingerprinted by this
measurement tool. Windows process-memory sampling is reported as unmeasured.

One macOS ARM64 release build with FFmpeg 9.0.2 and Shaka Packager 3.8.0 measured three
paired runs of ten-second Big Buck Bunny clear DASH to CBCS DASH, with five two-second video segments:

| Worker setting | Median conversion | Median separate verification | Largest observed descendant count |
| --- | ---: | ---: | ---: |
| Automatic | 0.747 s | 0.385 s | 2 |
| Maximum 1 | 1.271 s | 0.637 s | 1 |

All six conversions and independent checks passed stream metadata, encoded payload,
complete decoded content, and every packet timestamp. During separate verification,
median sampled CLI resident memory was 13.0 MB with automatic workers and 12.5 MB with
one worker; descendant totals were 20.6 MB and 18.3 MB respectively. Memory samples were
100 ms apart, and parent/descendant maxima are separate. Short-lived processes can be
missed. These observations apply to this small asset and host; they do not establish
long-media throughput, an exact memory peak, or a result on another operating system.
## A larger synthetic input

Generate a nine-track input from the attributed, ten-second Big Buck Bunny clip:

```sh
python3 tools/prepare_bbb_benchmark.py --output /path/to/new-benchmark
```

Then use `new-benchmark/source/manifest.mpd` with the measurement command above.
Python 3.9+, FFmpeg and Shaka Packager are required. The destination must be new
and its parent must exist. Paths containing commas, dollar signs or line breaks are
refused because Packager parses commas and expands dollar expressions in paths.
A failed generation leaves
its diagnostic log and partial directory; choose a new directory after correcting it.

This creates eight identical 1280×720 video renditions and one audio track, with five
two-second video segments. It measures independent track work; it is not a realistic
adaptive-bitrate ladder or a video quality comparison. `ATTRIBUTION.md` accompanies
the generated media. `input-report.json` records the pinned excerpt hash and every
generated resource's size and SHA-256. Generated metadata and tool versions can
change these hashes, so retain the report with a measurement.

On one ten-logical-CPU macOS ARM64 host, Rust 1.99.0 release, FFmpeg 9.0.2 and
Shaka Packager 3.8.0, three alternating pairs with this input produced:

| Worker setting | Median conversion | Median separate verification | Conversion CLI sampled RSS | Conversion descendants sampled RSS |
| --- | ---: | ---: | ---: | ---: |
| Automatic | 10.671 s | 4.839 s | 13.2 MB | 445.3 MB |
| Maximum 1 | 25.477 s | 13.295 s | 11.7 MB | 66.0 MB |

All six clear DASH to CBCS DASH conversions and separate verifications passed all
four content and timing checks. Every input resource retained its recorded hash.
The measurements include complete verification and startup. Other development work
continued on the host; these are observations under load, not isolated throughput
results. The automatic setting used more descendant memory while completing this
work sooner. RSS was sampled every 100 ms; the parent and descendant maxima are
separate, can miss spikes, and are not a hard memory limit. These results do not
establish long-media, disk usage, device playback, or other operating-system behavior.
