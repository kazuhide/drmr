# Vendored: Shaka Player

The browser console plays old/new content with Shaka Player. It is vendored here (instead of loaded
from a CDN at page load) so the tool stays fully local once the image is built: no third-party
network fetch, no unpinned remote script.

| | |
|---|---|
| Package | `shaka-player` (npm) |
| Version | **4.10.6** |
| File | `shaka-player.compiled.js` — the package's `dist/shaka-player.compiled.js`, **unmodified** |
| License | Apache-2.0 — `LICENSE` in this directory (copied from the same package) |
| Source URL | https://cdn.jsdelivr.net/npm/shaka-player@4.10.6/dist/shaka-player.compiled.js |
| Size | 642,828 bytes |
| sha256 | `8f7e52f5ee6144cb6b67a2ecb0612b36a31a9eb97c88f56466eed293aa0e7223` |

Referenced from `static/index.html` as
`/static/vendor/shaka-player/shaka-player.compiled.js`.

## Provenance

The bytes were taken from the npm registry tarball
`https://registry.npmjs.org/shaka-player/-/shaka-player-4.10.6.tgz` (its sha512 integrity and sha1
shasum were checked against the registry's published `dist` values) and cross-checked against the
copy served by unpkg. jsDelivr's `/npm/` endpoint serves the same package file verbatim.

The `//# sourceMappingURL=shaka-player.compiled.map` trailer is left intact; the 1.8 MB map is
deliberately **not** vendored (browser DevTools may log a 404 for it — harmless).

## Updating

1. Pick the new version and fetch `dist/shaka-player.compiled.js` and `LICENSE` from the npm
   tarball (verify the tarball against `npm view shaka-player@<ver> dist.integrity`).
2. Replace both files here and update the version, size and sha256 in the table above.
3. Re-confirm in Chrome: old and new DASH manifests load, ClearKey playback starts, and the
   rendition dropdown can switch video (both codec families) and audio renditions.
