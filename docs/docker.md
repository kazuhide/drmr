# Run with Docker

Build a Linux container containing `drmr`, FFmpeg and ffprobe, Bento4's `mp4dump`, and Shaka Packager. Rust and the media tools are built inside Docker. The image runs the CLI and its experimental web console.

Use a running Docker installation with BuildKit and Git. Builds are native for Linux `amd64` (64-bit Intel/AMD) or `arm64`; select Linux containers when using Docker Desktop. The examples use a macOS or Linux shell. The native [Windows guide](windows.md) covers installation without Docker.

On Windows, use Docker Desktop's [WSL 2 integration](https://docs.docker.com/desktop/features/wsl/) and run these commands in its Linux shell. Keep protected configuration in the distribution's Linux filesystem: Windows-drive mounts can expose [different file permissions](https://learn.microsoft.com/en-us/windows/wsl/file-permissions). This is a Linux-container workflow, separate from the native Windows CLI.

## Build

From a committed checkout of this repository:

```sh
revision=$(git rev-parse HEAD)
tree=$(git rev-parse "${revision}^{tree}")
git archive --format=tar "$revision" | docker build \
  --build-arg DRMR_REVISION="$revision" \
  --build-arg DRMR_TREE="$tree" \
  --tag drmr:local -

docker run --rm --init drmr:local doctor --probe --route all
```

The build uses that captured commit, including its dependency lockfile. Uncommitted files are excluded. Keep keys, downloaded media, and conversion results outside the repository; mount them only when running a container. The first build compiles the included tools and retains build caches in Docker.

The recipe pins its compiler, base images, and media-tool sources. Those pins describe this image; the source build's minimum Rust requirement remains [Rust 1.95 or newer](requirements.md). `doctor --probe` checks installed versions and advertised options; conversion still validates the actual media.

## Convert local media

Prepare three host directories: `media` for your source presentation, `config` for its [key configuration](usage.md#separate-input-and-output-keys), and `output` for results. On macOS and Linux, create `config` and `output` for your own user with directory mode `700`, and give `config/keys.json` file mode `600`. These permissions allow only the owner to access the protected configuration.

```sh
docker run --rm --init --user "$(id -u):$(id -g)" \
  --mount "type=bind,src=$PWD/media,dst=/input,readonly" \
  --mount "type=bind,src=$PWD/config,dst=/config,readonly" \
  --mount "type=bind,src=$PWD/output,dst=/output" \
  drmr:local convert /input/manifest.mpd \
  --to cbcs --format dash --output /output/protected-dash \
  --keys-file /config/keys.json
```

Paths passed to `drmr` are paths **inside the container**. `/input` and `/config` are read-only mounts. Matching the container's user and group IDs to your host user lets it read owner-only keys and create results you own. The image defaults to a non-root user; it does not change host file ownership or repair permissions.

The output destination must be new. Use a qualified input from the [conversion support table](conversion-support.md); adding Docker does not broaden the supported media profiles. To compare the result independently:

```sh
docker run --rm --init --user "$(id -u):$(id -g)" \
  --mount "type=bind,src=$PWD/media,dst=/input,readonly" \
  --mount "type=bind,src=$PWD/config,dst=/config,readonly" \
  --mount "type=bind,src=$PWD/output,dst=/output,readonly" \
  drmr:local verify /output/protected-dash/manifest.mpd \
  --source /input/manifest.mpd --keys-file /config/keys.json --json
```

drmr chooses workers automatically, using the CPU and memory available to its process. Readable Linux container limits also contribute to that estimate. See [resource limits](resources.md) for the distinction between these estimates and operating-system enforcement.

`--rm` removes the container's temporary filesystem when it exits. Mounted results remain on the host. `--init` forwards stop signals and reaps exited child processes; drmr retains its own cancellation and cleanup checks. Press Ctrl+C for an attached command. For a named container, `docker stop --timeout 60 NAME` allows time for cooperative shutdown before Docker forces termination. drmr's final status establishes confirmed cleanup.

## Experimental web console

On a Linux Docker host, use host networking so the console can keep its default `127.0.0.1:8080` listener:

```sh
docker run --rm --init --name drmr-console --network host \
  --user "$(id -u):$(id -g)" \
  --mount "type=bind,src=$PWD/catalog,dst=/catalog,readonly" \
  --mount "type=bind,src=$PWD/output,dst=/output" \
  --env DRMR_CATALOG_ROOT=/catalog --env DRMR_OUTPUT_ROOT=/output \
  drmr:local serve
```

Open `http://127.0.0.1:8080` on that host. `catalog` must use the [web console's supported source layout](usage.md#experimental-web-console). Its workflows remain experimental.

Docker Desktop requires its optional [host networking feature](https://docs.docker.com/engine/network/drivers/host/) for this mode. Ordinary port publishing does not reach a listener bound only to the container's loopback address. Keep the console's existing loopback and request-origin protections. The Desktop application itself runs on the host; this image supplies CLI and web execution.

## Included versions and notices

The image records its source revision, source tree, and dependency pins in `/usr/share/drmr/build-manifest.json`. Component notices are in `/usr/share/drmr/licenses`. `/usr/share/drmr/sources` retains the media-tool source archives, drmr, and the selected Rust dependencies and standard-library source. The repository's MIT license covers drmr; bundled components retain their own licenses.

Debian package versions and their packaged notices are recorded separately. The image does not include a full Debian source mirror. Packages installed from Debian's repositories can change between builds; the manifest records the versions actually installed.

## Clean up

After removing containers that use this image, remove your local tag with:

```sh
docker image rm drmr:local
```

Docker retains build caches for reuse. If you build with a dedicated builder, remove that builder when you no longer need its cache. Keep source media, keys, and results until you have finished verifying and using them.
