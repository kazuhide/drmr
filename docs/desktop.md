# Desktop console

The experimental Desktop host opens the existing local Web console in a native window. It uses the same catalog, jobs and media tools as `drmr serve`. It does not expose every generic CLI command as a desktop form.

## Choose folders

1. Open **File → Choose catalog folder** and select your media catalog.
2. Open **File → Choose output location**. drmr creates or opens a private `drmr-output` child folder there.
3. Select **File → Open console**.

The macOS bundle permits plain HTTP only for the loopback IP `127.0.0.1`; window navigation still requires the exact owned port. The native menus handle folder selection. The console receives no native shell or filesystem permissions. Links cannot navigate the window to another origin or open another window. An owned report link opens in the existing window; **File → Open console** returns to the console.

To change folders after opening the console, quit and reopen the application. Source URLs and downloaded media can remain in private analysis folders; closing the window does not delete them. See [usage](usage.md) for saved-source and history retention.

**Quit drmr** stops admission and waits for the local server to confirm cleanup. If cleanup cannot be confirmed, the application stays open, retains its running work and shows an error. Correct the reported cause, then select **Quit drmr** again to retry orderly cleanup. An operating-system force quit is separate from this orderly shutdown.

## Local playback

The before/after player is a local ClearKey playback test, independent of the conversion's media checks. A fetched or configured test key is not reported as usable until the video emits a playing event. Successful playback in Chrome does not qualify WKWebView, WebView2 or WebKitGTK; each native WebView needs its own runtime check. No DRM service or device playback guarantee follows from this test.

Playback setup has a 20-second budget covering previous-player cleanup, the local test-key response, Shaka media loading and the video actually beginning to play. This bounds a stalled setup in the running page without changing media-processing timeouts. On expiry the controls return with recovery guidance; conversion results remain unchanged. “Loading media” means Shaka has not completed its load; “Media loaded; waiting for playback to start” means the media load returned but the video has not emitted a playing event. Neither phase establishes that a key is usable. Retry after a setup timeout. If previous-player cleanup cannot be confirmed, reload the console page before retrying: drmr retains that owner and does not create another player on the same video. JavaScript timers depend on the WebView event loop running and are not an operating-system termination deadline.

## Tested results

Development checks on macOS used the public Big Buck Bunny 10-second fixture with five 2-second segments. These observations do not qualify every source or platform.

| Check | Observed result |
| --- | --- |
| Native folder menus, keyboard input and report navigation | Tested; owned reports open in the same window. |
| Native catalog conversion | Completed with all media checks passing. |
| Native Stop and orderly close during active processing | Owned CLI and media processes exited; closing the app also released its listener. |
| Chrome local ClearKey playback | Before and after clips both reached the end. |
| WKWebView local ClearKey playback | English/Japanese setup failure and retry recovery tested; completed playback remains unqualified. |
| Windows and Linux native GUI | Runtime qualification remains open. The [Linux dependency warnings](#linux-dependency-qualification) also remain open. |

## External tools and launch environment

The host and its jobs inherit `PATH` from the application launcher. Finder may provide a different environment from your terminal. The host does not search extra installation folders or run shell startup files.

Before launching on macOS, use the bundled CLI in a terminal where your media tools are available:

```sh
"/path/to/drmr.app/Contents/MacOS/drmr" doctor
"/path/to/drmr.app/Contents/MacOS/drmr-desktop"
```

`doctor` checks tool discovery, not tool versions or media compatibility. A missing media tool can make jobs fail even when the window opens successfully; check the job log. Windows and Linux portable builds likewise inherit the environment used to launch their native executable.

## Build from source

Install the [Tauri platform prerequisites](https://v2.tauri.app/start/prerequisites/) and the external media tools listed in [requirements](requirements.md). Windows uses WebView2, macOS uses WKWebView, and Linux uses WebKitGTK. A successful cross-compile does not verify native dialogs or WebView behavior.

Select a supported Rust toolchain explicitly, with its `cargo` and `rustc` on `PATH`. The examples use `python3`; use `python` on Windows:

```sh
RUSTUP_TOOLCHAIN=1.95.0 python3 tools/build_desktop.py --bundle
```

Add `--release` for optimized binaries. `CARGO_TARGET_DIR` chooses a dedicated build directory; the default is `target/desktop`.

The script builds the CLI from the same checkout, stages dependency license texts and publisher notices, then builds the optional `desktop` feature. It creates an unsigned macOS `.app`, or a portable Windows/Linux directory, under `desktop-bundles` in the target directory, together with a `.tar.gz` or `.zip` archive that retains executable permissions. Move a previous bundle before rebuilding; it is never overwritten.

Published packages that omit a license text require a version-pinned publisher fallback with a verified checksum. Unresolved or changed fallbacks stop packaging. CLI archives use the same notice collector and retain embedded Web asset licenses.

The host checks the bundled CLI's SHA-256 before starting the server. Keep the two binaries together. FFmpeg, Shaka Packager and Bento4 are not bundled. Keys, downloaded media and operator configuration are not included in the application bundle.

The Tauri configuration also describes native installer packaging. The build script does not sign, notarize or create an installer, and its output does not establish qualification on another operating system.

## Verify a distribution archive

CI checks the completed archive before upload. The verifier compares its exact file set, target-specific dependency notices, original publisher bytes, binary headers and executable modes against the checkout. Each archive includes `build-manifest.json` with the source commit, tree, target, compiler and binary hashes. This manifest describes a build; it is not a signed attestation.

To repeat the check, use the matching clean checkout and a Rust toolchain with its locked dependency sources already cached. For example, on Apple Silicon macOS:

```sh
python3 tools/verify_archive.py verify --kind desktop \
  --target aarch64-apple-darwin --archive /path/to/drmr.app.tar.gz \
  --report /path/to/new-verification.json --run-pair-check
```

`--run-pair-check` executes only the extracted native host's fixed `--verify-bundle` mode, then requires that the same host reject a deliberately changed CLI copy. It starts no window or server. Use this option only for a trusted build on its matching native operating system and architecture. Without it, Desktop verification reports that pair verification remains incomplete. For CLI archives, use `--kind cli` and omit this option.

The checks establish archive contents and the compiled host/CLI pairing. They do not establish GUI behavior, media compatibility, signing, provenance of the build environment or legal certification.

## Linux dependency qualification

The current Linux GUI dependency graph includes `glib 0.18.5`, affected by the `VariantStrIter` issue in [RUSTSEC-2024-0429](https://rustsec.org/advisories/RUSTSEC-2024-0429.html), and the unmaintained `proc-macro-error 1.0.4` build dependency ([RUSTSEC-2024-0370](https://rustsec.org/advisories/RUSTSEC-2024-0370.html)). No advisory is suppressed. Linux GUI runtime qualification remains open; a successful build does not resolve these warnings. These optional Linux GUI dependencies are not enabled by default CLI builds.

## Development checks

The Desktop dependencies are optional. Existing workspace checks without the `desktop` feature keep building the CLI and server without a WebView development environment. To check the native host, first stage its CLI and notices:

```sh
RUSTUP_TOOLCHAIN=1.95.0 python3 tools/build_desktop.py --prepare-only
RUSTUP_TOOLCHAIN=1.95.0 cargo clippy --locked -p drmr-desktop --features desktop --all-targets -- -D warnings
```

Native qualification must additionally cover folder-picker cancellation, keyboard input, real jobs, shutdown during active work, and immediate child-process and listener cleanup. Headless policy and packaging tests do not establish those results.
