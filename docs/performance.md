# Analysis performance

These measurements show how complete analysis changed, then compare native
optimizations at the same verification depth. Whole-presentation measurements use the same retained clear
HLS input: about ten minutes, six video renditions, one audio track and
708 referenced files totaling 857,417,309 bytes. Network acquisition is outside
the timed command. Every recorded analysis passed source admission.

The [numeric results](benchmarks/native-analysis-2026-10-25.json) include
source revisions, individual observations and measurement limits.

## When native validation became the default

These are historical observations, one run per row. The linked revisions identify
the exact published source trees matched by the measured builds.

| Source tree | Validation | Command time | CPU time | FFmpeg / ffprobe / Packager calls |
| --- | --- | ---: | ---: | ---: |
| [794fea033](https://github.com/kazuhide/drmr/commit/794fea033dbcc3d6408f9ed519a3d49803879203) | Complete decoding by default | 229.79 s | 799.88 s | 13 / 2,075 / 6 |
| [c1f16506](https://github.com/kazuhide/drmr/commit/c1f16506a2936c0d4ee3b7cf7fd14dbcd2a72fde) | Native source validation | 16.43 s | 11.67 s | 0 / 0 / 0 |

Native validation checks every admitted sample, codec configuration, clock and
supported playback window in Rust. Complete decoding adds decoder execution and
decoded-content evidence. A separate control with complete decoding still
requested took 223.30 seconds once; that small difference from 229.79 seconds
does not establish a meaningful speedup at the same depth. See
[verification depth](verification.md) for the evidence each mode provides.

Both historical runs used automatic worker selection. Track checks were already
parallel; private source copying and fingerprint checks were sequential. Calling
the initial implementation entirely single-threaded would be inaccurate.

The recorded environment was macOS 27.0.1, ARM64, ten logical CPUs and Rust 1.95.0
release builds, with FFmpeg/ffprobe 9.0.2 and Shaka Packager 3.8.0.
Darwin `time -p -l` supplied wall and CPU time. Its reported maximum resident-set
values were 136,921,088 and 13,697,024 bytes, respectively. Those values are not
a simultaneous sum of memory across the process tree.

## Native implementation changes

This separate experiment uses Rust 1.95.0 release builds on an Apple M5,
ten logical CPUs, 32 GiB of physical memory and macOS 27.0.1.
All four builds use native validation and automatic workers.

- **a** selects the CPU SHA-256 backend and makes frequent cancellation polls
  use an atomic flag. The build also selects AES acceleration at runtime;
  this clear-input workload exercises hashing, parsing and source preparation.
- **b** prepares each private parent directory once, checks its identity on reuse
  and retains a complete final private-file census.
- **c** copies and fingerprints independent source files in bounded pools shared
  with the command's CPU, memory and worker limits.

Each build ran three times, with the starting build rotated between rounds.
The table gives median wall and CPU times, the complete wall-time range,
and the largest observed CLI resident memory. CPU time includes user and kernel
execution and can exceed wall time when threads run concurrently.
A MiB is 1,048,576 bytes.

| Build | Median wall, s | Wall range, s | Median CPU, s | Peak CLI memory, MiB |
| --- | ---: | ---: | ---: | ---: |
| [Native baseline](https://github.com/kazuhide/drmr/commit/b03514df0bcd69cbf1a33a47200ef7d2b1555429) | 21.13 | 14.41–21.54 | 14.72 | 14.1 |
| [a: CPU hashing and cancellation](https://github.com/kazuhide/drmr/commit/22917a7710a1aa041641b9fb9ae18ff5dd0d9bbf) | 10.34 | 9.41–12.73 | 6.93 | 15.5 |
| [a + b: prepared directories](https://github.com/kazuhide/drmr/commit/2c7e9f7bf0644fc4b78c0a663f2bb5751ab93254) | 11.28 | 8.80–11.93 | 6.58 | 16.0 |
| [a + b + c: bounded source pools](https://github.com/kazuhide/drmr/commit/9bd7e6c1e89335018f954e09637ac8e64b1ea047) | 10.42 | 9.66–10.66 | 7.41 | 15.2 |

All twelve analyses passed, started no media tools, and produced identical
reports after removing the measurement fields. Original files retained their
bytes. Timing surrounded each whole CLI invocation, including its cleanup;
Darwin `wait4` supplied CPU and peak resident-memory counters.

Other work continued on the host, and filesystem caches were uncontrolled.
The ranges overlap among optimized builds: this batch does not establish
a separate wall-time gain for each small change. Directory preparation used
less median CPU time than **a** in this batch; the pool build used
more median CPU time than the directory preparation build. Its wall time
remained lower than the native baseline. Use the observations
and ranges together rather than treating the rows as a guaranteed smooth curve.

## Worker bounds

A separate, earlier alternating-pair batch used
[build f5f3c571](https://github.com/kazuhide/drmr/commit/f5f3c571a4cc10284edf59ecfca5f2cc14a98edf)
with the same input, compiler, native depth and host configuration.
Each setting has three successful observations.

| Worker upper bound | Median wall, s | Median CPU, s | Peak CLI memory, MiB |
| --- | ---: | ---: | ---: |
| 1 | 8.56 | 5.32 | 14.7 |
| 2 | 7.86 | 5.46 | 15.2 |
| 4 | 7.48 | 5.76 | 16.4 |
| Automatic | 6.98 | 6.41 | 15.0 |

Every report matched apart from measurements. In that batch, the automatic
native baseline took 13.82 seconds and the bounded-pool build took 6.98 seconds.
It is a separate experiment from the rotating-build table above; host load and
cache state can change elapsed time. Numeric worker options are upper bounds.
Automatic selection uses available CPU and memory budgets at each stage.

## Further native changes

The next changes reduce repeated parsing and temporary media copies, then share
bounded workers across independent HLS renditions and output pipelines. The
source fingerprints, complete encoded-sample checks and timestamp comparisons
remain required. The [numeric results](https://github.com/kazuhide/drmr/blob/main/docs/benchmarks/native-processing-2026-10-26.json)
keep these experiments separate from the earlier measurements above.

H.264 (AVC) framing is parsed once for syntax checks and encoded-sample hashing.
Start codes mark the beginning of each H.264 data unit. Searching for them uses
the vectorized byte-search implementation already provided by the
`memchr` dependency. The parser borrows an unescaped payload when it can; escaped
payloads use bounded owned storage. Dense escape sequences use a scalar fallback.
The complete payload is still checked, including malformed trailing bytes.

A parser-only build and its native baseline each ran three times, alternating
which build ran first. Both used the original seven-track retained input,
Rust 1.95.0 release builds, automatic workers and the same native verification.

| Build | Median wall, s | Wall range, s | Median CPU, s | Peak CLI memory, MiB |
| --- | ---: | ---: | ---: | ---: |
| Baseline | 6.93 | 6.80–7.37 | 6.28 | 14.4 |
| Parser changes | 5.67 | 5.63–5.74 | 4.97 | 14.9 |

All six reports matched after removing measurements; every original file
retained its bytes, and neither build started a media tool. These observations
cover the parser change alone. Filesystem caches and other host processes were
uncontrolled, so they do not establish the gain on another input or machine.

Native HLS checks now read ordered source segments directly, avoiding a joined
copy solely for validation. This preserves the source's segment membership while
retaining private source copies and complete content and identity checks. Routes
that require container conversion or complete decoding can still join tracks.
Independent renditions and output pipelines share the command's worker ceiling;
adding a pool does not multiply `--max-workers`. The resource estimates and
embedding limits are described under [resource limits](resources.md).

### Integrated analysis and worker limits

The integrated build was measured separately against the same native baseline
with one, two and seven selected tracks. The one-track view selects the lowest
bitrate video; the two-track view adds the original audio. Their media bytes are
unchanged, and only the master playlist is adjusted to select tracks. The
seven-track view uses the full original presentation. These views differ in both
media size and track count, so each baseline is compared with its matching view.

The baseline source was checked retrospectively against all 933 tracked files.
Their contents match the recorded Git tree, and their modification and metadata
change times precede the original build. The retained original build output
matches the frozen binary's SHA-256 and recorded modification time. This binding
relies on retained filesystem metadata and artifacts; it does not provide a
build-time clean-tree log or a hermetic rebuild. The numeric report records the
method and its limits.

All builds used Rust 1.95.0 release mode on a macOS ARM64 host with ten logical
CPUs. Three rounds rotated the binary, view and worker-setting order, producing
72 complete analyses. Every report matched within its view after removing
measurements. Every input file retained its bytes, and all media-tool counts were
zero. The whole CLI invocation, including cleanup, was timed; `wait4` supplied
CPU and peak resident-memory counters. Filesystem caches and other host processes
were uncontrolled.

With automatic worker selection, the results were:

| Selected tracks | Baseline wall, median (range), s | Updated wall, median (range), s | Median CPU, baseline / updated, s | Peak CLI memory, baseline / updated, MiB |
| --- | ---: | ---: | ---: | ---: |
| 1 video | 0.505 (0.502–0.514) | 0.491 (0.464–0.503) | 0.241 / 0.231 | 11.6 / 11.8 |
| 1 video + 1 audio | 0.845 (0.814–0.900) | 0.874 (0.871–0.890) | 0.441 / 0.469 | 12.1 / 12.5 |
| 6 video + 1 audio | 7.430 (6.944–7.848) | 4.182 (3.896–4.449) | 6.526 / 5.017 | 14.6 / 19.0 |

The seven-track median fell from 7.43 to 4.18 seconds in this batch. The smaller
views do not establish a consistent gain: the two-track automatic median rose
from 0.845 to 0.874 seconds, with overlapping ranges. Parallel preparation can
also use more resident memory; the seven-track automatic maximum observed CLI
memory rose from 14.6 to 19.0 MiB.

For the seven-track input, the requested worker upper bound gave:

| Worker upper bound | Baseline wall, median (range), s | Updated wall, median (range), s | Updated median CPU, s | Updated peak CLI memory, MiB |
| --- | ---: | ---: | ---: | ---: |
| 1 | 9.123 (8.417–10.197) | 7.396 (6.713–7.403) | 3.924 | 15.0 |
| 2 | 8.785 (8.353–8.824) | 5.368 (5.069–5.937) | 3.976 | 16.1 |
| 4 | 8.087 (7.811–8.261) | 4.277 (4.172–4.693) | 4.541 | 17.8 |
| Automatic | 7.430 (6.944–7.848) | 4.182 (3.896–4.449) | 5.017 | 19.0 |

These are upper bounds, not observed concurrent worker counts. Increasing the
bound reduced elapsed time for this input while increasing CPU work and memory
within the updated build. The automatic and four-worker ranges overlap, so this
batch does not establish a separate improvement from four workers to automatic.

The selected quiescent workspace checkpoints for the seven-track input contained
1,714,824,807 regular-file bytes before and 857,432,007 after the change. Those
logical lengths are about half at the observed checkpoint. They are not an exact
command-wide temporary-disk peak, allocated filesystem blocks or total bytes
read and written. Peak CLI resident memory is also separate from a simultaneous
process-tree peak.

## Cipher and MP4 reader measurements

Component measurements help explain individual changes. They do not measure
whole conversions or predict the same percentage improvement for the CLI.
The cipher loops process at least 256 MiB per case, include construction, copying
and a SHA-256 check of every output byte, and use the same locked dependencies.
Three alternating pairs produced these median times:

| Synthetic workload | Baseline, s | Updated, s |
| --- | ---: | ---: |
| `cenc`, 16-byte initialization vector | 0.625 | 0.510 |
| HLS AES-128, approximately 1 MiB resources | 0.623 | 0.585 |

The `cenc` scheme uses AES counter-mode encryption; an initialization vector
selects the starting counter. The updated implementation processes independent
counter blocks in batches. CBC (cipher-block chaining) decryption can also batch
contiguous encrypted blocks,
while sparse patterns retain direct block handling. HLS AES-128 resources known to be at or
below 65,552 ciphertext bytes use the previous small-resource path. The larger
streaming path retains fixed buffers and checks final padding at actual end of
input. Small-resource differences do not establish an improvement, and sparse
SAMPLE-AES and CBC pattern cases have overlapping time ranges.

A separate parser experiment ran five alternating observations, each with
256 calls over a fixed 1 MiB synthetic input. Start-code scanning took 0.128879
seconds before and 0.005492 seconds after vectorized searching. An AVC payload
without escape bytes took 0.187187 and 0.004743 seconds; a densely escaped payload
took 0.202877 and 0.202638 seconds. The dense case shows no meaningful improvement.
These timed parser checksums aggregate output lengths; separate differential
regressions check complete output and refusal behavior. Input construction,
CLI I/O and complete source admission are outside these parser loops.

The MP4 reader uses a bounded reusable buffer, caches its current file position
and reads small encrypted subsample windows together. A separate diagnostic over
three independent oracle tests recorded 17 reader instances, 6,533 Rust
`File::read` calls and 900 `File::seek` calls.

These counts describe that revision and test corpus; they do not quantify an I/O
reduction. They are not operating-system syscall counts or whole-command I/O
measurements. Buffer-reuse regressions check the owned scratch storage; no global
allocation-count reduction is claimed. The numeric report identifies the measured
revision and confirms that the relevant production source bytes are unchanged in
the integrated candidate.

## Compare your own retained input

Use the same local original to separate processing from network transfer:

```sh
drmr analyze input.m3u8 --json --measure --progress off
drmr analyze input.m3u8 --max-workers 1 --json --measure --progress off
```

Add `--decode` to both commands when comparing complete-decoding workloads.
Keep the source, compiler, tools and requested verification depth fixed.
Record whole-command time separately from the overlapping phase sums emitted
by `--measure`. See [resource limits and measurements](resources.md) for budgets,
cancellation and measurement coverage.

These observations describe this input and host. They do not qualify other
media profiles, other operating systems, DRM license services or device playback.
