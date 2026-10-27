# Processing performance

These measurements cover analysis, multi-output conversion and independent
verification. The analysis sections separate historical verification-depth
changes from comparisons at the same depth. The conversion sections compare
two builds at each requested verification depth.

The earlier whole-presentation analysis runs use the same retained clear HLS input: about ten
minutes, six video renditions, one audio track and 708 referenced files
totaling 857,417,309 bytes. An 18-second excerpt is identified separately.
Network acquisition is outside the timed commands. Every recorded source
admission passed.

The [historical analysis data](benchmarks/native-analysis-2026-10-25.json)
include source revisions, individual observations and measurement limits.

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
build-time clean-tree log or a fully isolated rebuild. The numeric report records the
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
measurements. Buffer-reuse regressions check the owned scratch storage; no
whole-command allocation-count reduction is claimed. The numeric report identifies
the measured revision and confirms that the relevant production source bytes are
unchanged in the integrated candidate.

### Selected allocation and input/output diagnostics

Separate single-threaded diagnostics counted operations in selected parser
and reader helpers over synthetic bytes. They used frozen public source,
with ordinary and counted wrappers, after the timed experiments.
All six diagnostic builds succeeded; 42 observations include empty and
allocator controls. Source inventories remained unchanged. The
[numeric report](https://github.com/kazuhide/drmr/blob/main/docs/benchmarks/bundle-processing-2026-10-26.json)
records source, wrapper and binary hashes, all counters and their scope.

For a 65,535-byte parser payload without escape bytes, the updated helper
borrowed the payload instead of allocating a copy:

| Parser build | Successful allocations | Logical copied bytes | Explicit wipe target bytes |
| --- | ---: | ---: | ---: |
| Baseline | 1 | 65,535 | 65,535 |
| Updated | 0 | 0 | 0 |

Sparse and dense escape-byte inputs still allocated once and wiped
65,535 capacity bytes each. Wiping uses `zeroize`, which explicitly clears
owned memory. Copy and wipe counts are logical operation targets, including
repeated wipes and vector capacity on drop; they are not memory-bus traffic.

A reader diagnostic consumed eight 512-byte samples using the updated
helpers. Fresh creates a buffer per sample; cold reuses a buffer allocated
inside the interval; warm preallocates before the interval. The window
variant reads each small sample together before serving smaller reads.

| Reader configuration | Successful allocations | Rust file reads / seeks | Unix call total |
| --- | ---: | ---: | ---: |
| Fresh, direct | 8 | 16 / 7 | 23 |
| Cold reuse, direct | 1 | 16 / 7 | 23 |
| Warm reuse, direct | 0 | 16 / 7 | 23 |
| Warm reuse, sample window | 0 | 8 / 7 | 15 |

The window adds 4,096 logical copied bytes and targets 12,288 bytes for
wiping in this case. Warm-buffer allocation and destruction fall outside
the interval; zero interval allocations do not mean zero retained storage.
The 64 KiB boundary, larger direct-read path and short-read refusal were
also checked.

Unix and Mach totals are operating-system calls made by the diagnostic
process, including observation overhead. The empty control recorded zero
Unix calls and one Mach call; no fixed value was subtracted. These totals
do not classify kernel reads and seeks separately. Allocator counters
describe requested bytes and entries, excluding allocator rounding and
internal reallocation work.

The controls compare status, output length and a noncryptographic 64-bit
checksum that traverses the complete output. This is separate from the
actual-media correspondence checks below. These diagnostics cover selected
helpers, without measuring a whole CLI allocation count, complete encrypted
MP4 pipeline or maximum number of open files.

## Multi-output conversion and verification

The clear HLS MPEG-TS input was converted into HLS MPEG-TS encrypted with
SAMPLE-AES and FairPlay signaling, and DASH fragmented MP4 encrypted with
the `cbcs` scheme and Widevine/PlayReady signaling. Synthetic keys were
used. Each saved plan was executed and its outputs were verified by a fresh CLI
process against the retained original. Network download time is excluded.

The [baseline](https://github.com/kazuhide/drmr/commit/3fa08d5f9bf5a507c240b2543b7eacfef1e9379b)
and [updated build](https://github.com/kazuhide/drmr/commit/147a9e192e51b8999711ed8ac1a43519ba6d6e34)
use Rust 1.95.0 release mode on the same macOS ARM64 host with ten logical
CPUs and automatic worker selection. The [numeric results](https://github.com/kazuhide/drmr/blob/main/docs/benchmarks/bundle-processing-2026-10-26.json)
include every observation, binary/tool hashes and measurement limits.

Planning checks and binds the selected source. Conversion includes its
required output verification; fresh verification repeats the comparison
in a separate CLI process.

| Recipe | HLS processing | DASH processing |
| --- | --- | --- |
| Mixed | Segments; preserve source boundaries | Stitch; segment again |
| Stitch both | Stitch; segment again | Stitch; segment again |

The recipes perform different boundary work. Each build comparison below
keeps the same source, recipe and verification depth.

### Full presentation with native verification

This batch uses the full approximately ten-minute presentation, with
six video renditions, one audio track and 717 retained files.

| Recipe | Build | Plan, median (range), s | Convert, median (range), s | Fresh verify, median (range), s |
| --- | --- | ---: | ---: | ---: |
| Mixed | Baseline | 27.687 (22.970–33.278) | 146.888 (145.809–153.720) | 64.934 (63.637–66.084) |
| Mixed | Updated | 19.468 (18.460–25.381) | 99.201 (98.627–117.564) | 52.931 (51.441–54.322) |
| Stitch both | Baseline | 22.141 (20.406–22.429) | 174.342 (150.650–181.110) | 72.723 (66.026–83.683) |
| Stitch both | Updated | 13.052 (12.805–13.789) | 80.598 (78.618–83.312) | 45.384 (39.142–48.737) |

### 18-second excerpt with native verification

The excerpt contains the same six video renditions and one audio track over
approximately 18 seconds, in 29 retained files. The complete chosen asset was
checked.

| Recipe | Build | Plan, median (range), s | Convert, median (range), s | Fresh verify, median (range), s |
| --- | --- | ---: | ---: | ---: |
| Mixed | Baseline | 1.062 (1.059–1.064) | 6.217 (6.033–6.736) | 2.663 (2.376–2.687) |
| Mixed | Updated | 0.825 (0.785–0.830) | 4.008 (3.696–4.021) | 2.112 (2.021–2.175) |
| Stitch both | Baseline | 0.955 (0.916–0.981) | 6.380 (6.357–6.467) | 2.623 (2.596–2.684) |
| Stitch both | Updated | 0.573 (0.549–0.576) | 3.166 (3.129–3.233) | 1.587 (1.472–1.642) |

### The same excerpt with complete decoded-content verification

This batch uses exactly the same 29-file excerpt as the native table.
Complete decoded-content comparison covers the whole chosen excerpt.
The native and decoded modes perform different verification work.

| Recipe | Build | Plan, median (range), s | Convert, median (range), s | Fresh verify, median (range), s |
| --- | --- | ---: | ---: | ---: |
| Mixed | Baseline | 27.383 (26.630–29.286) | 215.623 (190.486–235.318) | 90.880 (60.587–101.819) |
| Mixed | Updated | 20.535 (20.482–22.360) | 131.147 (118.422–136.242) | 41.133 (38.393–43.119) |
| Stitch both | Baseline | 35.044 (30.031–35.619) | 227.158 (190.969–374.424) | 61.673 (59.468–68.953) |
| Stitch both | Updated | 22.359 (20.097–33.014) | 144.352 (134.832–158.148) | 38.538 (37.970–41.420) |

### Recorded host load for the excerpt

A snapshot was taken before and after every measured command. Load
averages describe operating-system load over one, five and fifteen
minutes. Process observations can be partial when process-argument rows
are unavailable; missing observations stay missing.

| Verification depth | Snapshot status: complete / partial / unavailable | One-minute load, observed range | Process snapshots available |
| --- | ---: | ---: | ---: |
| Native | 0 / 72 / 0 | 3.66–7.20 | 72 / 72 |
| Decoded | 0 / 72 / 0 | 4.06–48.78 | 72 / 72 |

Each timing value summarizes three runs. The three experiments each have
twelve complete plan/convert/fresh-verify cases. Compare builds within the
same table; the full native input and the short decoded excerpt differ in
duration and media size. Native checks compare complete encoded samples,
stream configuration and timestamps. Decoded mode additionally compares
all decoded content for the selected asset.

All conversions and fresh verifications passed their requested checks.
The native-depth cases launched neither FFmpeg nor ffprobe; conversion
used Shaka Packager. Tool counts remain separate from verification depth.
The native manifest linter reported no errors or incomplete work within
its declared manifest profiles; delivery and media-evidence rules outside
that linter scope remain separate from media correspondence checks.

The full-input native batch stopped under an idle guard between cases.
Nine complete cases were retained unchanged and the remaining three ran
after a pause. The original interrupted summary remains preserved.
The excerpt batches proceed while background work may be active. Host load is
observed before and after each command, outside its measured elapsed time.
Incomplete process observations remain labelled as partial or unavailable.
Load averages include earlier benchmark work as well as other processing.
They cover one, five and fifteen minutes; process-CPU percentage
sums are observations from the process table, without a precise CPU peak
or isolation claim. Filesystem caches and host load are uncontrolled.
The timings describe these runs under recorded load; they do not isolate
the effect of code changes from changes in other host work.

Times surround each whole CLI command, including its cleanup. CPU and
maximum resident-memory counters come from `wait4` for the command and
its children. This resident-memory value is not a simultaneous sum across
the process tree. The numeric report includes every observation, recomputed
CPU/memory summaries and media-tool launch counts.
Phase scope sums emitted by the CLI can overlap and are not command wall
time. Random initialization vectors can change encrypted output bytes;
each output was verified independently, with no byte-for-byte ciphertext
equality claim. License-server and device playback were not qualified.

## Shared source preparation in direct conversion

A later comparison uses one `convert --outputs` command to analyze, plan,
convert and check two outputs. The retained clear HLS TS input has six video
renditions, one audio rendition and 708 referenced files totaling 857,447,176
bytes. It is a separate retained copy from the earlier experiments above.
The outputs are CBCS-encrypted fMP4: DASH with Widevine/PlayReady information
and HLS with FairPlay information.

The [baseline](https://github.com/kazuhide/drmr/commit/af861071e0736d8fdd1d9ef6fdfbfeef94f4693c)
and [shared-preparation build](https://github.com/kazuhide/drmr/commit/1d6321d7f5b43d87785d09364a2ed42f374192c8)
used Rust 1.99.0 release builds, Shaka Packager 3.8.0, automatic workers and
the same input, recipe and synthetic keys. The host was an Apple M5 with ten
logical CPUs and 32 GiB of memory, running macOS on ARM64. Downloading,
decoding and external manifest validators are outside this comparison.

| Build | Command wall, s | CPU, s | Packager calls |
| --- | ---: | ---: | ---: |
| Baseline | 85.88 | 168.12 | 28 |
| Shared preparation | 62.51 | 121.65 | 15 |

This is one sequential pair, with the baseline first. Background work continued;
one-minute load ranged from 7.59 to 8.76. Filesystem caches were uncontrolled.
CPU time is user plus system time for the command and its children.
The [numeric report](benchmarks/shared-bundle-source-2026-10-27.json) includes
binary hashes, individual observations and limits. This pair does not measure
repeat-run variance or establish a guaranteed speedup. Neither conversion
started FFmpeg or ffprobe.

The updated command keeps source qualification and planning within the same
operation and shares joining and clear-track preparation between these outputs.
Each output still has its own encryption working files, complete media
comparisons, DRM/manifest checks and final publication checks. Separately
executed plans and independent verification check current files again.
Short-fixture regressions count one common source qualification instead of
eight, and six Packager calls instead of ten, while requiring the same media
proofs. The full-input run above reduced normalization scopes from 14 to seven;
scope elapsed-time sums overlap and are not command wall time.

Both conversions passed content, timing, metadata and DRM checks. Four fresh
verification commands checked both outputs from both builds against the
unchanged original; native manifests reported no errors or warnings within
their declared profiles. All seven HLS playlists retained target duration 6
and 100 segments. DASH used duration-based `SegmentTemplate` references,
seven representations, a nominal six-second duration and 100 media resources
per representation. Its explicit period duration was 596.526077098 seconds.
These checks do not establish DRM license acquisition or device playback.

Peak sampled process-family RSS rose from 167.1 to 224.3 MiB. This sampled sum
can include shared pages more than once and can miss short-lived processes.
Darwin `wait4` reported a different maximum-resident metric of 113.8 and
112.0 MiB. The updated workspace-size observations were partial: eight
successful checkpoints and two incomplete attempts. Those file-size checkpoints are separate from
media verification and do not establish peak disk use. Memory and worker
admission limits remain in effect.

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

These observations describe the selected inputs and recorded host conditions.
They do not qualify other media profiles, other operating systems, DRM license
services or device playback.
