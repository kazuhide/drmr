# syntax=docker/dockerfile:1.19.0@sha256:b6afd42430b15f2d2a4c5a02b919e98a525b785b1aaff16747d2f623364e39b6

FROM rust:1.95.0-slim-bookworm@sha256:d7482085ff5b415f84dba5647ae71606650bdef00db7aeb69f4b3d170c3e4082 AS native
ARG TARGETARCH
ARG BUILD_JOBS=2
ENV PYTHONDONTWRITEBYTECODE=1
RUN case "$TARGETARCH" in amd64|arm64) ;; *) echo "Unsupported image architecture" >&2; exit 1 ;; esac \
    && apt-get update \
    && apt-get install -y --no-install-recommends build-essential ca-certificates cmake curl git gnupg ninja-build nasm pkg-config python3 xz-utils \
    && rm -rf /var/lib/apt/lists/*
COPY docker/native.py docker/ffmpeg.py docker/sources.json docker/ffmpeg-release.asc /recipe/
RUN python3 /recipe/ffmpeg.py --jobs "$BUILD_JOBS"

# Serial stages keep simultaneous C++ and Rust compilation within a small daemon budget.
FROM native AS cargo-cache-check
COPY docker/cargo_cache.py docker/tests/cargo_cache_control.py /cache-recipe/
RUN python3 /cache-recipe/cargo_cache_control.py --report /cargo-cache-control.json

FROM native AS shaka-build
COPY docker/shaka.py /recipe/shaka.py
RUN python3 /recipe/shaka.py --jobs "$BUILD_JOBS"

FROM shaka-build AS drmr-build
WORKDIR /source
RUN rustup component add rust-src
COPY Cargo.toml Cargo.lock ./
COPY crates/ crates/
COPY vendor/ vendor/
COPY docker/cargo_cache.py /recipe/cargo_cache.py
COPY --from=cargo-cache-check /cargo-cache-control.json /opt/drmr/metadata/cargo-cache-control.json
RUN --mount=type=cache,id=drmr-registry-${TARGETARCH},target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,id=drmr-target-${TARGETARCH},target=/source/target,sharing=locked \
    python3 /recipe/cargo_cache.py --source /source --target /source/target --report /opt/drmr/metadata/cargo-cache.json \
    && CARGO_BUILD_JOBS="$BUILD_JOBS" cargo build --release --locked -p drmr \
    && install -m 0755 target/release/drmr /opt/drmr/bin/drmr
# Documentation and provenance do not invalidate the preceding compilation layer.
COPY . .
COPY docker/assemble.py /recipe/assemble.py
RUN --mount=type=cache,id=drmr-registry-${TARGETARCH},target=/usr/local/cargo/registry,sharing=locked python3 /recipe/assemble.py

FROM drmr-build AS qualification-build
RUN --mount=type=cache,id=drmr-registry-${TARGETARCH},target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,id=drmr-target-${TARGETARCH},target=/source/target,sharing=locked \
    python3 /recipe/cargo_cache.py --source /source --target /source/target \
    && CARGO_BUILD_JOBS="$BUILD_JOBS" cargo build --release --locked -p drmr-test-support --bin container-qualification \
    && CARGO_BUILD_JOBS="$BUILD_JOBS" cargo build --release --locked -p drmr-core --example container-resources \
    && mkdir /out \
    && install -m 0755 target/release/container-qualification /out/container-qualification \
    && /out/container-qualification export-fixture /out/drmr-native-fixture \
    && install -m 0755 target/release/examples/container-resources /out/container-resources
COPY docker/qualification_manifest.py /recipe/qualification_manifest.py
ARG DRMR_REVISION
ARG DRMR_TREE
RUN python3 /recipe/qualification_manifest.py "$DRMR_REVISION" "$DRMR_TREE" "$TARGETARCH"
FROM scratch AS qualification-artifacts
COPY --from=qualification-build /out/ /out/

FROM debian:bookworm-slim@sha256:7c7b2c966bc9ee8cedfeef67e0e279108992c77681fa595db4a9d65c06ccc587 AS runtime-base
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates libatomic1 libstdc++6 \
    && rm -rf /var/lib/apt/lists/* \
    && groupadd --gid 10001 drmr \
    && useradd --uid 10001 --gid 10001 --no-create-home --home-dir /tmp --shell /usr/sbin/nologin drmr \
    && install -d -m 0755 -o 10001 -g 10001 /work /usr/share/drmr \
    && install -d /usr/share/drmr/licenses/os \
    && cp -aL /usr/share/doc/. /usr/share/drmr/licenses/os/ \
    && cp -aL /usr/share/common-licenses /usr/share/drmr/licenses/os/common-licenses \
    && dpkg-query -W -f='${binary:Package}\t${Version}\t${Architecture}\t${source:Package}\t${source:Version}\n' > /usr/share/drmr/runtime-os.tsv
COPY --from=drmr-build /opt/drmr/bin/ /usr/local/bin/
COPY --from=drmr-build /opt/drmr/licenses/ /usr/share/drmr/licenses/
COPY --from=drmr-build /opt/drmr/sources/ /usr/share/drmr/sources/
COPY --from=drmr-build /opt/drmr/metadata/ /usr/share/drmr/metadata/

# Python is used only in this discarded metadata stage, never in the runtime image.
FROM runtime-base AS metadata
RUN apt-get update && apt-get install -y --no-install-recommends python3 && rm -rf /var/lib/apt/lists/*
COPY docker/finalize.py /recipe/finalize.py
ARG DRMR_REVISION
ARG DRMR_TREE
ARG TARGETARCH
RUN python3 /recipe/finalize.py --revision "$DRMR_REVISION" --tree "$DRMR_TREE" --architecture "$TARGETARCH"

FROM runtime-base AS runtime
ARG DRMR_REVISION
ARG DRMR_TREE
LABEL org.opencontainers.image.title="drmr" \
      org.opencontainers.image.source="https://github.com/kazuhide/drmr" \
      org.opencontainers.image.revision="$DRMR_REVISION" \
      io.drmr.source.tree="$DRMR_TREE" \
      io.drmr.purpose="docker-runtime"
COPY --from=metadata /usr/share/drmr/build-manifest.json /usr/share/drmr/build-manifest.json
COPY --from=metadata /usr/share/drmr/licenses/inventory.json /usr/share/drmr/licenses/inventory.json
COPY --from=metadata /usr/share/drmr/licenses/os/packages.json /usr/share/drmr/licenses/os/packages.json
COPY --from=metadata /usr/share/drmr/sources/inventory.json /usr/share/drmr/sources/inventory.json
ENV HOME=/tmp TMPDIR=/tmp
USER 10001:10001
WORKDIR /work
ENTRYPOINT ["drmr"]
CMD ["--help"]
