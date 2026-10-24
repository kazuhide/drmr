# Desktop console

The experimental Desktop host opens the existing local Web console in a native window. It uses the same saved plans, jobs and media tools as `drmr serve`. The common workflow runs analyze, saved-plan creation, conversion and independent verification. Other CLI commands remain available through the CLI.

## Choose folders

1. Open **File → Choose input folder…** and select the allowed media/saved-plan folder.
2. Open **File → Choose output location…**. drmr creates or opens a private `drmr-output` child folder there.
3. Optionally choose **File → Choose private configuration folder…** for key and fetch-policy files. Keep it separate from input and output folders. **Clear private configuration selection** removes this selection before starting the console. Clear-media flows without fetch settings need no configuration folder.
4. Select **File → Open console**.

The macOS bundle permits plain HTTP only for the loopback IP `127.0.0.1`; window navigation still requires the exact owned port. The native menus handle folder selection. The console receives no native shell or filesystem permissions. Links cannot navigate the window to another origin or open another window. An owned report link opens in the existing window; **File → Open console** returns to the console.

To change folders after opening the console, quit and reopen the application. Source URLs and downloaded media can remain in private analysis folders; closing the window does not delete them. See [usage](usage.md) for retention of downloaded inputs, saved plans and results.

**Quit drmr** stops admission and waits for the local server to confirm cleanup. If cleanup cannot be confirmed, the application stays open, retains its running work and shows an error. Correct the reported cause, then select **Quit drmr** again to retry orderly cleanup. An operating-system force quit is separate from this orderly shutdown.

## Saved-plan conversion and later verification

Choose **New conversion** in the console. Select local media or **Source URL**, open the input, and analyze it. URL input accepts the [supported MP4, static DASH and completed HLS profiles](conversion-support.md#url-input). Add 1–8 named outputs, choose a new output folder, then **Save plan**. **Download original plan** preserves the original bytes and absolute paths; keep that file private because it identifies local files and output locations.

In Desktop, **Download original plan** opens a native save dialog. Choose a new JSON file in a private folder outside the input folder and `drmr-output`. The private folder must not contain either media folder. The default is the selected private configuration folder, or a private `drmr-plans` sibling of `drmr-output`. drmr creates that folder when needed and refuses an existing folder with unsafe permissions; it does not change its permissions. Existing files are never overwritten. Other attachment downloads are unavailable in the Desktop window.

Canceling the dialog creates no file. If you choose **Quit drmr** with the dialog open, cancel the dialog to finish closing. Closing discards a late selection and waits for the export worker and any owned partial-file cleanup. A changed file or parent folder is preserved when cleanup cannot establish ownership. If cleanup remains unconfirmed, inspect the selected private folder and any hidden `.drmr-plan-cleanup-*` folders without deleting unfamiliar files. Restore the original private folder, then choose **Quit drmr** again. See [cleanup and recovery](security.md#temporary-files-and-cleanup) for the file-preservation policy.

After submission, URL input is cleared and shown only as a generic **Source URL** label. The full URL is not returned to the console because credentials can also occur in path segments.

Choose **Convert** to create the output, then **Verify again** for an independent comparison. To reopen a private downloaded copy later, select its folder as **Private configuration folder** before opening the console, then choose **Saved CLI plan**. For a copy saved in `drmr-plans`, select that folder in the later session. The folder must remain separate from the input and output media folders. You can also use `drmr verify --plan /path/to/private/drmr-plan.json` with the CLI, supplying runtime keys if needed. Existing plans in the allowed input/output folders remain supported. New console plans use schema 6 even with one output and bind processing mode, boundary guarantee and minimum verification depth; legacy plans 1/2 retain a complete-decoding minimum. The additional-decoding checkbox matches CLI `--decode` and can strengthen the saved minimum. Results distinguish native evidence from actual decoding and unrequested decoding. Key files keep their existing versions: named-output v2/v4/v6 plans use v2 keys, while single-output v1/v3/v5 plans use v1 keys. The page selects private configuration references and never uploads or retrieves key-file contents.

Saved-plan operations share one execution slot. A lost response remains unknown until status lookup resolves it; the page does not repeat a launch. Stop acknowledgment does not establish process cleanup. A pending, running or cleanup-unconfirmed session remains owned. See [the Web workflow](usage.md#saved-plan-workflow) for session limits and recovery.

These controls do not extend the native runtime results in the table below. Each native WebView still needs its own workflow, attachment-download and keyboard qualification.

## Tested results

Development checks on macOS used the public Big Buck Bunny 10-second fixture with five 2-second segments. These observations do not qualify every source or platform.

| Check | Observed result |
| --- | --- |
| Native folder menus, keyboard input and report navigation | Tested; owned reports open in the same window. |
| Native Stop and orderly close during active processing | Owned CLI and media processes exited; closing the app also released its listener. |
| Windows and Linux native GUI | Runtime qualification remains open. The [Linux dependency warnings](#linux-dependency-qualification) also remain open. |

## External tools and launch environment

The host and its jobs inherit `PATH` from the application launcher. Finder may provide a different environment from your terminal. The host does not search extra installation folders or run shell startup files.

Before launching on macOS, use the bundled CLI in a terminal where your media tools are available:

```sh
"/path/to/drmr.app/Contents/MacOS/drmr" doctor
"/path/to/drmr.app/Contents/MacOS/drmr-desktop"
```

Before opening the console, choose **File → Check external tools…**. This runs the same bounded version and advertised-option checks as `drmr doctor --probe`, using the native application's actual launch environment. The dialog shows resolved paths and fixed results. It does not run shell startup files or add guessed installation directories. If tools are missing, install them and reopen the application from an environment whose `PATH` contains their executable directories. A close request stops subsequent probes and waits for the active diagnostic call and its readers. If process cleanup remains unconfirmed, the application stays open and retains that process owner; select Quit again to retry. It cannot open the console or exit normally until cleanup is confirmed.

A successful diagnostic does not establish media compatibility. A window opening successfully does not establish tool availability. Windows and Linux portable builds likewise inherit the environment used to launch their native executable.

## Build from source

Install the [Tauri platform prerequisites](https://v2.tauri.app/start/prerequisites/) and the external media tools listed in [requirements](requirements.md). Windows uses WebView2, macOS uses WKWebView, and Linux uses WebKitGTK. A successful cross-compile does not verify native dialogs or WebView behavior.

Select a supported Rust toolchain explicitly, with its `cargo` and `rustc` on `PATH`. The examples use `python3`; use `python` on Windows:

```sh
RUSTUP_TOOLCHAIN=1.95.0 python3 tools/build_desktop.py --bundle
```

Add `--release` for optimized binaries. `CARGO_TARGET_DIR` chooses a dedicated build directory; the default is `target/desktop`.

The script builds the CLI from the same checkout, stages dependency license texts and publisher notices, then builds the optional `desktop` feature. It creates an unsigned macOS `.app`, or a portable Windows/Linux directory, under `desktop-bundles` in the target directory, together with a `.tar.gz` or `.zip` archive that retains executable permissions. Move a previous bundle before rebuilding; it is never overwritten.

Published packages that omit a license text require a version-pinned publisher fallback with a verified checksum. Unresolved or changed fallbacks stop packaging. CLI archives use the same notice collector and retain embedded Web asset licenses.

The host checks the bundled CLI's SHA-256 before starting the server. Keep the two binaries together. Native analysis and verification need no external media tools. Install Shaka Packager for packaging routes and FFmpeg/ffprobe for additional complete decoding. Progressive MP4 copying and decryption run in Rust. These tools are installed separately. Keys, downloaded media and operator configuration are not included in the application bundle.

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

The Linux GUI graph uses a source-verified backport for the `glib 0.18.5` iterator issue and retains a narrowly acknowledged, unmaintained build dependency. See [dependency advisory checks](dependency-security.md) for the exact source, optimized regression, affected graph and residual maintenance exposure. The advisory findings remain in CI reports. Linux GUI runtime qualification remains open; optional GUI dependencies are not enabled by default CLI builds.

## Development checks

The Desktop dependencies are optional. Existing workspace checks without the `desktop` feature keep building the CLI and server without a WebView development environment. To check the native host, first stage its CLI and notices:

```sh
RUSTUP_TOOLCHAIN=1.95.0 python3 tools/build_desktop.py --prepare-only
RUSTUP_TOOLCHAIN=1.95.0 cargo clippy --locked -p drmr-desktop --features desktop --all-targets -- -D warnings
```

Native qualification must additionally cover folder-picker cancellation, keyboard input, real jobs, shutdown during active work, and immediate child-process and listener cleanup. Headless policy and packaging tests do not establish those results.
