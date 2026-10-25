# Analysis performance

These measurements show how complete analysis changed, then compare native
optimizations at the same verification depth. All use the same retained clear
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
