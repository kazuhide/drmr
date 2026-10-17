# Vendored fonts

IBM Plex Sans (latin, weights 400/500/600) and IBM Plex Mono (latin, 400), taken from the
`@fontsource/ibm-plex-sans` and `@fontsource/ibm-plex-mono` npm packages, version 5.3.0
(registry tarball `dist.integrity` sha512 verified at vendoring time, 2026-10-16). Licensed under
the SIL Open Font License 1.1 (`LICENSE-ibm-plex.txt`). Only the Latin subset is shipped; Japanese
text falls back to the operating system's fonts (Hiragino Sans / Yu Gothic UI / Noto Sans JP) by
design, so the tool stays fully offline with no CDN request at page load.

| File | Bytes | SHA-256 |
|---|---|---|
| `ibm-plex-mono-latin-400-normal.woff2` | 14,708 | `08949f728dc52d528e69b1667d15c89a5686a4ee9a296ff90983985f99c380f7` |
| `ibm-plex-sans-latin-400-normal.woff2` | 22,588 | `3b646991d30055a93a4ecc499713d4347953a74a947ecab435ab72070cbdab0e` |
| `ibm-plex-sans-latin-500-normal.woff2` | 24,184 | `0717336fb31fcdcde4b8deb3675bb4a0f7f6d484864afcd6751ac29975962203` |
| `ibm-plex-sans-latin-600-normal.woff2` | 24,252 | `8960851d691c054ed38e259bdcf1a6190d157b4203ed5bb32c632a863fb8ec2f` |

## Updating

1. Fetch the package metadata and tarball from the npm registry
   (`https://registry.npmjs.org/@fontsource/<name>/latest`), and verify the tarball against the
   `dist.integrity` value before extracting.
2. Copy `files/<name>-latin-<weight>-normal.woff2` for the weights listed above and the package
   `LICENSE` (as `LICENSE-ibm-plex.txt`) into this directory; keep the same file names so
   `app.css`'s `@font-face` rules keep resolving.
3. Refresh the table above (size and SHA-256) and the version in the first paragraph.
