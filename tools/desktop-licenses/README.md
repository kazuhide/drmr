# Publisher license fallbacks

These unchanged notice texts supplement published crates that omit a license document. `manifest.json` binds each fallback to the exact package version, publisher revision, selected license, source URL, and SHA-256 checksum. The build fails if a needed fallback is absent, mismatched, or changed.

Where a publisher declares a standard license without supplying its full text, the retained declaration is accompanied by the corresponding pinned SPDX license text. Publisher notices remain intact; copyright holders and years are not invented.

The WebView2 bindings also retain the exact Microsoft SDK loader LICENSE and NOTICE, independently of the Rust wrapper's MIT notice. Their manifest records the SDK archive, checksum, and archive member names. The SDK binaries are not copied here.

Packages licensed under the Mozilla Public License receive a human-readable `SOURCE.txt` in the generated notice directory with the complete unmodified source archive URL. The inventory records publisher notices and source provenance for the selected target's dependency graph. Embedded Web asset and vendored process-wrapper notices are retained separately.
