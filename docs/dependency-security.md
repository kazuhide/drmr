# Dependency advisory checks

The advisory workflow runs for pull requests, main-branch changes and weekly schedules. It retains the advisory database revision, the raw `cargo-audit` output and separate results for the default CLI and optional Desktop graph on Linux x64, macOS ARM64 and Windows x64. Development-only dependencies and unrelated optional GUI packages are excluded from each distribution graph. This is advisory discovery, not a proof that an application path is exploitable or safe.

## Linux Desktop dispositions

The current Linux Desktop graph has two explicitly bounded dispositions. Neither finding is deleted from the reports. Default CLI graphs do not contain these packages.

| Finding | Treatment | Remaining boundary |
| --- | --- | --- |
| [RUSTSEC-2024-0429](https://rustsec.org/advisories/RUSTSEC-2024-0429.html), `glib 0.18.5` | The exact publisher source is vendored with the two-line output-pointer fix from [upstream commit 05dff0e](https://github.com/gtk-rs/gtk-rs-core/commit/05dff0ee696f9bcd8617cd48c4b812d046d440cb). All 121 publisher files and the change are checked against `tools/backports/glib-0.18.5.json`. A native optimized iterator regression must also pass before the advisory gate calls it `source-mitigated`. | The version remains `0.18.5`, so the registry version advisory still applies. This is a reviewed source backport, not an upstream release or GUI runtime qualification. |
| [RUSTSEC-2024-0370](https://rustsec.org/advisories/RUSTSEC-2024-0370.html), `proc-macro-error 1.0.4` | `acknowledged-maintenance`, limited to the exact Linux Desktop dependency ancestry through the `glib-macros 0.18.5` and `gtk3-macros 0.18.2` procedural macros. | This unmaintained build-time dependency can still contain undiscovered defects. The disposition is not a vulnerability fix or a claim that builds are safe. |

The complete dependency paths, resolved features and target kinds are retained in `summary.json`. The policy binds the complete semantic finding record (including affected functions, platforms, version ranges and future fields), direct parents, package version, advisory ID, classification, target and resolved feature graph. Dependency ancestry also binds each registry/git source identity or exact workspace-relative manifest path; paths outside the workspace are refused. No absolute checkout paths enter these fingerprints or summaries. A new finding or a change to this semantic record, ancestry, source identity, features or classification requires review and fails the gate. There is no general warning ignore option.

A direct GLib 0.20 upgrade would change the GTK3 type family used by the current Linux WebView dependencies. A switch to `proc-macro-error2` does not resolve maintenance exposure: [RUSTSEC-2026-0173](https://rustsec.org/advisories/RUSTSEC-2026-0173) also identifies it as unmaintained. Reevaluate when the WebView/GTK dependency family offers a compatible maintained path, when either advisory changes, or whenever the locked graph changes. Do not replace either package with an unreviewed fork solely to silence a warning.

## Windows source-layout preparation

When preparing outputs that retain source directories, Windows builds use a pinned source extension to `private-fs 0.2.1`. Directory creation, media relocation and removal of staging entries use already-opened object handles. Replacing a source pathname therefore cannot select a different object for those operations. drmr checks ownership, access controls and recorded file identities before requesting the change. The final publication step uses its separate staging checks. Key and authentication files retain the strict private-directory policy.

All 31 publisher files, the three modified source files and the original archive checksum are checked against `tools/backports/private-fs-0.2.1.json`. The extension reuses the package's native rename and deletion operations. The Windows suite checks replacement after identity validation, occupied destinations, invalid names and nonempty directories as an ordinary user. The package's MIT and Apache license texts remain in distribution notices.

## Reproduce the checks

Use the supported Rust toolchain and cached locked dependency sources. Verify both complete source patches and exercise the GLib string iterator under optimization:

```sh
python3 tools/verify_backports.py
cargo test --manifest-path tools/regressions/glib-variant-iter/Cargo.toml --release --locked --offline
```

The regression requires native GLib development libraries. The advisory workflow runs it on Linux; it does not initialize a WebView.

With `cargo-audit 0.22.2` installed and a clean checkout of the official [RustSec advisory database](https://github.com/RustSec/advisory-db):

```sh
python3 tools/audit_dependencies.py --database /path/to/advisory-db \
  --cargo-audit /path/to/cargo-audit --output /path/to/new-report-directory \
  --native-regression
```

The native mitigation gate requires Linux x64. Omitting `--native-regression` permits inspection elsewhere but leaves GLib's mitigation pending and returns failure for the Linux Desktop scope.

`cargo-audit` skips path-vendored packages. For discovery, the script additionally audits a temporary lock file that restores the exact publisher registry identities and archive checksums of the reviewed `glib 0.18.5` and `private-fs 0.2.1` sources. It retains **both** raw audit results, both input hashes and the separate normalized input. The normalized result is not presented as an audit of the actual lock file. Source verification and native runtime regression remain independent requirements.

CI summaries label the known maintenance exposure and source mitigation explicitly. The policy accepts only these documented dispositions; new advisories and unresolved findings remain failures. A passing advisory gate does not establish distribution signing, GUI behavior, media verification or license compliance.
