# Resource limits and measurements

See [analysis performance](performance.md) for the measured implementation history,
source revisions and worker comparisons.

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

When external decoding is requested, the media backend explicitly sets FFmpeg decoder, encoder, and filter thread counts to
one, and FFprobe decoder threads to one. One CPU token represents that codec allocation;
tools may still create helper threads. Streamed decoded-media hashing holds the same
reservation as commands that capture their output.

The worker maximum applies across nested pools belonging to one command. Preparing
several HLS renditions does not multiply that maximum by the number of renditions.
Independent commands in the same process retain their own worker maximums and share
the process allocation. Temporary occupancy makes a fitting request wait; a request
larger than its configured allocation fails without starting work.

Multi-output plans can process independent outputs concurrently. Their source and
output file metadata and prepared track state consume a separate retained memory
reservation, without holding CPU tokens while child work waits. Once output processing
is admitted, that reservation remains held through final source checks, publication,
cancellation and workspace cleanup.

Source reopening and discovery run before this coordinator reservation, under their
own bounded capture and discovery limits. Source file metadata kept while a call
waits is not charged to this additional reservation. Applications embedding drmr
should limit the number of queued conversion and verification calls.

Generated manifests have no final byte count before packaging. A multi-output
conversion therefore preserves the remaining memory allocation for its own child
work. Another such conversion in the same process may wait until it finishes;
the admitted command's outputs, renditions and segments can still run concurrently.
Children that cannot fit alongside their own retained state fail immediately.

Fresh verification of an existing multi-output result first inventories only the
HLS playlists or DASH manifest and derives the native linter's minimum working set
from their actual sizes and reference counts. Media and key addresses are not fetched in this
preflight. The temporary discovery state is released before waiting for the retained
reservation. Verification then captures the outputs and checks that every
manifest's relative path and complete bytes still match the preflight. A changed
graph is refused before the larger lint allocation. Known small graphs can therefore
be verified by independent commands concurrently. Parsing still respects the worker
maximum and the command's reserved memory headroom.

Structured `resource-budget` records show chosen workers, child thread allocation,
CPU tokens, estimated memory tokens, and whether memory came from the host, the process
control group, or the unknown fallback. They contain no source paths or machine IDs.

## Full analysis time

Large media checksums use SHA-256, a fixed-size fingerprint of the bytes.
Hashing and AES encryption select processor acceleration at runtime where it is
supported, with a software fallback. Source builds need no processor-specific
compiler flags. The same complete byte, sample and timing checks remain required.

`analyze` without `--metadata-only` checks complete source media. A URL operation
first retains the original resources, then checks a private copy against their bytes,
prepares tracks and checks all selected encoded samples, codec configuration, clocks and playback windows in Rust. `--decode` additionally checks complete decoded media. Finishing
the download does not finish the analysis. The progress history distinguishes
acquisition from the later inspection, preparation and verification work.

Worker selection applies to independent jobs within a stage. It does not make
every stage parallel or split every long-running media-tool call. Network and
disk throughput, subprocess startup, track count and checks that must retain
presentation order can limit the benefit of more workers. Use
`--max-workers 1` and the automatic setting on the same retained local input to
compare processing, rather than comparing two unrelated downloads. With
`--json --measure`, phase observations help locate the work; overlapping phase
times must not be added together as command wall time.

TS preparation collects metadata and encoded-frame evidence from independent
segments in parallel within the worker limit. It retains only a bounded window
of compact results, then checks duration and clock boundaries in the original
playlist order. Independent media playlists can be prepared concurrently under the
same command-wide worker ceiling. This pool does not
parallelize every stage or split a complete-track decode, and all source media
checks remain required.

Source preparation also copies and fingerprints independent media files in
parallel within the shared CPU, memory and worker limits. Manifest discovery
follows the declarations in dependency order. Each copy uses a 64 KiB streaming
buffer; bounded file and path metadata also consume memory. Original-file
checks use the same admission limits, and all active workers finish before
failure cleanup can remove the private copies.

For a quick look at MP4 metadata or HLS/DASH declarations, use
`analyze --metadata-only`. It does not download complete media or run complete
source checks, so its success does not establish conversion eligibility. See
[analysis and progress](usage.md) for the two workflows.

The measurements below retain their recorded source/tool scope. Historical complete-decoding measurements describe the additional `--decode` path, not the new native default. Compare runs only at the same verification depth.

## Native manifest work

HLS and DASH manifest linting uses the same process capacity as media work.
The adapter inventories manifest files before reserving memory for their retained
bytes and the lint call. It checks the loaded bytes against that inventory and
checks file identities and contents again before accepting the result.

The library runs small inputs inline. Larger calls use one bounded pool: multiple
documents can be parsed independently, while a single large DASH document shares
one parsed tree across its independent checks. These pools do not nest. HLS graph
checks reuse parsed child summaries; DASH checks reuse inherited templates and
timelines. DASH timeline repetition is checked with arithmetic, rather than
expanded into individual segments.

`--max-workers` remains an upper bound for native work, including
`verify --manifest-only`. The selected count also depends on input work, available
CPU capacity and the conservative memory estimate. Byte, node, depth, reference
and diagnostic limits bound work on untrusted manifests. An exhausted limit is
reported as incomplete work, rather than a passing check or an assertion that the
document violates the format specification. The allocation estimate is not a
measurement of process resident memory or an operating-system memory cap.

### Measured native manifest calls

A synthetic library measurement on 2026-10-21 used source
[`c1458e7d`](https://github.com/kazuhide/drmr/commit/c1458e7deea5b31118073c682d799f60c8f4e8b5), a macOS ARM64 host with 10 logical CPUs and 32 GiB of memory,
and a Rust 1.99 release build. One master and 64 media playlists, each containing
1,000 segments, supplied 1,924,687 manifest bytes. Whole lint calls took
14.50 ms with one worker and 3.37 ms with automatic selection of 10 workers.
These are medians of three rotating-order process medians, with input construction
outside the timed call. Small inputs stayed inline. A 30,000-entry DASH timeline
took 5.22 ms serially and 5.27 ms automatically: parallelism does not
accelerate every input.

All 144 timing runs across 12 workloads compared the complete report with serial
execution, allowing only `workers_used` to differ. Another 24 runs measured
allocations separately. For the 64-playlist automatic case, the peak additional
live requested allocation was 885,302 bytes; a separate operating-system process
peak was 8,142,848 bytes. The input buffers, allocator overhead and process state
make those different measurements. A conservative admission estimate is neither
number. Our media tests were paused for this measurement; other host processes
were not isolated. These observations describe library checks, not conversion
throughput or a performance guarantee on another machine.

The DASH profile-URI scanner uses a fixed immutable state graph and fixed-size
working sets, rather than allocating a graph or backtracking over the input.
The graph occupies 57,360 bytes and the two explicit state-set arrays total
128 bytes; these are not a total stack or resident-memory claim. Direct allocation
controls, including first use and long valid and invalid values, observed no
runtime scanner heap allocations. Parser, report and graph allocations remain
part of the library's resource accounting.

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

Sending a termination signal does not wait for descendants to finish closing files.
For an owned Unix process group, drmr therefore waits until the kernel reports that
the group no longer exists before confirming cleanup. An existing group or a permission
response remains unconfirmed, including while exited processes await collection by
their parent. After the direct child's exit status is collected, this group-completion
wait has a 30-second deadline; it does not bound unrelated blocking I/O or pipe-reader
joins. If absence cannot be confirmed, cleanup fails rather than reporting success or
a completed cancellation. Supervised Web and Desktop jobs retain their owner until managed tools have exited.

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

Captured output and streamed digest readers use nonblocking reads on Unix and Windows.
Unix readers wait for data readiness between bursts; Windows readers check available bytes on their exclusively owned overlapped
child-output handles before each read. An empty Windows pipe is treated as temporarily
unavailable, not as end of file. Each wait is bounded so cancellation can stop and join
the reader even while another process or the caller retains a writer.

If pipes remain open after the direct child exits, cleanup starts after two seconds,
including calls without an explicit command timeout. Windows jobs retain ordinary descendants;
Unix gives each managed child its own process group. A Unix child that deliberately
starts a separate session can escape group
termination; nonblocking pipe readers still stop and join at the capture deadline.
On Windows, readers no longer need every writer to close before they can stop and join.
The existing 30-second job-notification deadline remains separate from the command deadline;
nonblocking reads do not make thread scheduling, synchronous filesystem operations, or
caller-supplied stream callbacks hard real-time operations. Unconfirmed process cleanup
still takes precedence over timeout or cancellation; it is never reported as completed
cancellation. See [Windows cleanup limits](windows.md#unconfirmed-process-cleanup).

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

On one macOS ARM64 host with a Rust 1.95 debug build, the existing 40,000,000-byte
streaming regression timed out at its twenty-second deadline with the old reader.
With readiness waiting, the same complete byte count passed in 0.40 seconds while
retaining no stdout payload. The deadline and 8 KiB chunk bound were unchanged.
This synthetic pipe observation does not establish media conversion speed.

## Generic conversion

Use `convert ... --measure --json` or `verify ... --measure --json` to add an
optional `measurements` object to the normal result. The media checks and exit
codes are unchanged. Without `--measure`, the JSON result has no measurement
fields. A failed command retains its normal error rather than manufacturing a
successful measurement report.

Phase values are sums of explicitly timed scopes, including combined operations
such as decryption and container changes in one Packager call. Parallel scopes
can overlap: their sum is neither command wall time nor CPU time. Tool durations
also sum overlapping calls and include admission waiting, captured or streamed
output handling, and child cleanup. Do not add tool and phase sums together.
Zero-count phases are unmeasured; coverage excludes uninstrumented Rust work.

Workspace measurements inspect only owned private staging directories at explicit
checkpoints after their tools have finished. They report regular-file logical
lengths, with bounded traversal and no symlink or reparse-point following. These
checkpoints can miss files created and deleted between observations. They are not
an exact temporary-disk peak, allocated blocks, or total I/O bytes. Unavailable
or incomplete observations are labelled; total process I/O remains unmeasured.
Each checkpoint covers its selected staging directories, which can omit other
live command workspaces. The scan assumes the private stage owners remain held
and all writers have finished; metadata checks and directory traversal do not
atomically prevent path replacement or impose a hard filesystem I/O deadline.

Full `analyze`, `plan --save`, `convert` and `verify` accept `--max-workers 1`
through `64` as an upper bound. Metadata-only analysis refuses this option.
With no option, independent tracks share the CPU and memory admission described above.
Snapshots and stream metadata are checked before parallel normalization. Results retain
presentation order, and a failed track prevents publication. All content checks still run.

Whole-track inputs are streamed: the estimate includes up to a 32 MiB input window,
not the entire file size. Segment-based pipelines include their largest retained input
resource. Video dimensions, capture caps, and track metadata are charged separately.
Unknown media uses the conservative fallback until observed stream metadata is available.
This estimate does not enforce an absolute resident-memory limit on third-party tools.

FFmpeg and ffprobe use one decoder thread; FFmpeg output encoders and filters also use
one thread. Final joint packaging can run one Shaka Packager media job per distinct
input file. All jobs must fit the command's CPU ceiling, `--max-workers` and memory
allowance: the call reserves one CPU token and one conservative child-memory estimate
per job, once, through the shared limiter. Otherwise it uses `--single_threaded`.
Other Packager calls remain single-threaded. `--io_cache_size=0` always disables
separate per-file threaded I/O caches. Multiple outputs share these limits; they do
not each receive an independent allocation. These are admission estimates, not a
hard limit on an external binary's operating-system threads or resident memory.

The optional benchmark scripts are in the source checkout; run them from the repository
root with Python 3.9 or later. Python is not required for the CLI itself.

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
measurement tool. Helper report schema 2 keeps optional work measurements under
each conversion and verification, alongside command elapsed time and memory samples.
The media-proof fields remain separate.

Linux and macOS use process-table resident set size (RSS) samples. Windows uses read-only process
handles and working-set samples, checking creation identities and parent lifetimes
before attributing descendants. A pass queries processes sequentially; its sum is
not an atomic simultaneous snapshot. Short-lived processes, query failures, and
sampling limits can leave incomplete observations. These are labelled partial or
unmeasured, with null values when no usable sample exists. They are not complete
zero-byte observations or proof that every child was seen.

A short command can finish between samples. Parent and descendant peaks are
positive observations or `null`, never an assumed zero when memory was not observed.
Fixed counters distinguish a missing root, a failed query, no observed child rows,
and child rows whose resident bytes were zero. A positive observed child count with
a null memory peak means child rows were seen, without a positive memory observation.
It does not establish that the child's lifetime peak was zero. Status and reason
labels describe these gaps separately from media verification.

Summary medians use a separate observed-run count for parent and descendant memory.
Missing values are excluded with their own counts; an entirely missing metric stays
null. Command elapsed-time medians still include all successfully verified measured
runs. The helper's success field means media conversion and independent verification
passed, not that every memory metric was measured. Each operating system also has a
mandatory held-child test through the real measurement helper, requiring positive
memory observations and confirmed owned cleanup. This checks the observation path
without extending a real media command artificially or replacing its media checks.

The Windows observer is a separate owned helper, with no child processes or threads.
It emits one bounded numeric summary through a private pipe. The controller reads
only after confirming observer exit, with finite termination on timeout. Observer
cleanup is reported separately from media cleanup. The summary byte cap does not
guarantee pipe capacity: a blocked writer produces an unmeasured timeout, never a
successful observation. No observer thread is added to the CLI.

The default cache condition is `uncontrolled`. To perform an equivalent conversion
and independent verification before each measured case, add:

```sh
--condition warmed-by-equivalent-verified-run
```

Warmups use fresh outputs and must pass the same media checks. Their results are
recorded separately and excluded from measured medians. This condition means an
equivalent verified run happened immediately beforehand; it does not prove full
cache residency. Neither condition establishes a physically cold cache. Hashing
input entries and querying the binary can themselves warm caches. The helper does
not flush operating-system caches or alter host settings. Total I/O, exact disk
peaks, and physical-cold measurements remain unmeasured.

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
Python 3.9+ and the selected conversion tools are required. Add `--decode` to the measurement helper to require and record complete FFmpeg decoding on conversion and fresh verification; native-only and decoded runs are different workloads. The destination must be new
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
