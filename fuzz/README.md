# Native codec fuzzing

This development workspace tests the production HEVC and VP9 parsers with
coverage-guided mutations. LLVM's libFuzzer keeps inputs that reach new branches
and uses them for subsequent mutations. The two targets call drmr's existing
configuration, sample-header and persistent native sample checks. They neither
decode pictures nor start FFmpeg, Shaka Packager, a network client or a key service.

The normal workspace and release binaries do not depend on libFuzzer or require
nightly Rust. The empty `drmr-core/fuzzing` feature exposes only development
adapters; the same adapters run under `cfg(test)` for the short unit regressions.
This separate workspace locks its development dependencies in `Cargo.lock` and
keeps the root workspace's `process-wrap` and `private-fs` security patches.

## Fixed public seeds

The seeds are adapted from the eight attributed, ten-second **Big Buck Bunny**
fixtures in [HEVC.md](../crates/drmr/tests/fixtures/bbb/HEVC.md) and
[VP9.md](../crates/drmr/tests/fixtures/bbb/VP9.md): HEVC Main/Main 10 and VP9
profile/depth pairs 0/8, 1/8, 2/10, 2/12, 3/10 and 3/12. The media is
(c) copyright 2008, Blender Foundation / www.bigbuckbunny.org, licensed under
[Creative Commons Attribution 3.0](https://creativecommons.org/licenses/by/3.0/).
Keep this attribution when redistributing these adapted inputs.

`seed_corpus.py` reads the committed MP4 sample tables directly. It checks the
original fixture hashes before extracting configuration records and short
sample sequences. It needs Python 3, without media tools or downloads:

```sh
python3 fuzz/seed_corpus.py --check
cargo test -p drmr-core --locked --lib conversion::fuzzing::tests
```

There are 26 fixed inputs: four accepted HEVC seeds, eighteen accepted VP9
seeds, and two refused framing controls per codec. HEVC sequences include
samples from two random access points. VP9 sequences retain the first sixteen
samples, including reference state; each profile also has an explicit one-frame
superframe index. `seed-manifest.json` binds every seed to its length, SHA-256,
expected acceptance and sample count, and records the original fixture hashes.

Both fuzz targets replay these exact embedded inputs before mutations. They
assert the expected outcomes through the real parsers and print the following
machine-readable admission evidence only after successful replay:

```text
DRMR_FUZZ_CORPUS target=hevc accepted=4 refused=2
DRMR_FUZZ_CORPUS target=vp9 accepted=18 refused=2
```

## Build and run a bounded search

The pinned development tools are nightly Rust `nightly-2026-11-01`,
`cargo-fuzz` 0.13.2 and `libfuzzer-sys` 0.4.13. The Linux automation uses one
process per target with AddressSanitizer, debug assertions and overflow checks.
The [cargo-fuzz guide](https://rust-fuzz.github.io/book/cargo-fuzz.html) describes
platform requirements; LLVM documents the
[libFuzzer limits](https://llvm.org/docs/LibFuzzer.html#options).

```sh
rustup toolchain install nightly-2026-11-01 --profile minimal --component rust-src
cargo install cargo-fuzz --version 0.13.2 --locked
cargo fetch --manifest-path fuzz/Cargo.toml --locked
cargo metadata --manifest-path fuzz/Cargo.toml --locked --offline --format-version 1

# Keep instrumented build files and mutation outputs outside the repository.
export CARGO_PROFILE_RELEASE_DEBUG_ASSERTIONS=true
export CARGO_PROFILE_RELEASE_OVERFLOW_CHECKS=true
fuzz_work=$(mktemp -d)
mkdir "$fuzz_work/hevc" "$fuzz_work/vp9" "$fuzz_work/artifacts"
CARGO_NET_OFFLINE=true cargo +nightly-2026-11-01 fuzz build \
  --fuzz-dir fuzz --target-dir "$fuzz_work/target" \
  --sanitizer address --debug-assertions

for codec in hevc vp9; do
  CARGO_NET_OFFLINE=true cargo +nightly-2026-11-01 fuzz run "$codec" \
    "$fuzz_work/$codec" "fuzz/corpus/$codec" \
    --fuzz-dir fuzz --target-dir "$fuzz_work/target" \
    --sanitizer address --debug-assertions -- \
    -max_total_time=60 -timeout=2 -rss_limit_mb=512 -malloc_limit_mb=128 \
    -max_len=65536 -seed=1 -print_final_stats=1 \
    -artifact_prefix="$fuzz_work/artifacts/"
done
```

The first corpus directory receives new mutations; the committed second corpus
provides the fixed initial inputs. Do not use confidential media or keys as
fuzzer inputs. Crash and timeout artifacts contain the complete offending input.
Keep a failure artifact until it has been minimized and covered by a normal
regression test; remove disposable build and mutation files afterward.

Each input is limited to 65,536 bytes, a 16,384-byte configuration and sixteen
samples. Sample payloads stay borrowed, with each at most 65,535 bytes. The
sample evidence adapter can split a sample into two chunks and checks that an
earlier failure remains sticky and prevents successful completion. Synthetic,
continuous clocks isolate codec state; these adapters do not qualify original
container timestamps or decoded picture equivalence.

The campaign flags bound each target to sixty seconds, two seconds per input,
512 MiB resident memory and 128 MiB per allocation. libFuzzer checks some resource
limits periodically. Automation must also enforce an outer deadline, collect
the final exit/statistics and confirm owned process cleanup. A missing seed
admission, crash, timeout, memory limit, missing final statistics or unfinished
cleanup is a failure, never a passing result.

A passing finite campaign means no failure was found in that particular search.
It does not establish exhaustive codec correctness, decoded-picture equality,
DRM license acquisition or device playback support. Native archive conversion
and independent verification remain separate release requirements.
