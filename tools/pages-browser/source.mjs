import { createHash } from 'node:crypto';
import { readFileSync } from 'node:fs';
import { execFileSync } from 'node:child_process';

export const assets = [
  'index.html', 'ja/index.html', 'guide/index.html', 'ja/guide/index.html',
  'assets/site.css', 'assets/guide.css', 'assets/drmr.svg',
  'assets/fonts/LICENSE-ibm-plex.txt', 'assets/fonts/README.txt',
  'assets/fonts/ibm-plex-sans-latin-400-normal.woff2',
  'assets/fonts/ibm-plex-sans-latin-600-normal.woff2',
  'assets/fonts/ibm-plex-mono-latin-400-normal.woff2',
];
export const builderInputs = ['tools/build_guide_site.py', 'tools/templates/guide.html',
  'site-content/guide/en.md', 'site-content/guide/ja.md'];
export const publicOrigin = 'https://kazuhide.github.io';
export const publicBase = publicOrigin + '/drmr/';
export const localOrigin = 'http://127.0.0.1:4173';
export function assetRoute(path) {
  if (!assets.includes(path)) throw new Error('Unknown Pages asset');
  return path.endsWith('index.html') ? path.slice(0, -'index.html'.length) : path;
}
export const digest = bytes => createHash('sha256').update(bytes).digest('hex');
export function builderHashes(read = path => readFileSync(new URL('../../' + path, import.meta.url))) {
  return Object.fromEntries(builderInputs.map(path => [path, digest(read(path))]));
}
export function source() {
  return {
    sourceHead: execFileSync('git', ['rev-parse', 'HEAD'], { encoding: 'utf8' }).trim(),
    sourceTree: execFileSync('git', ['rev-parse', 'HEAD^{tree}'], { encoding: 'utf8' }).trim(),
    builderInputHashes: builderHashes(),
    assetHashes: Object.fromEntries(assets.map(path => [path,
      digest(readFileSync(new URL('../../site/' + path, import.meta.url)))])),
  };
}
export function liveMode(value) {
  if (value !== undefined && value !== 'true') throw new Error('Unsupported Pages test mode');
  return value === 'true';
}
export function allowedRequest(raw, live) {
  const url = new URL(raw);
  return url.origin === (live ? publicOrigin : localOrigin) &&
    (!live || url.pathname.startsWith('/drmr/')) && !url.search && !url.username && !url.password;
}
export function assetPath(raw) {
  const url = new URL(raw);
  let relative = url.pathname.startsWith('/drmr/') ? url.pathname.slice('/drmr/'.length) : url.pathname.slice(1);
  if (!relative || relative.endsWith('/')) relative += 'index.html';
  return assets.includes(relative) ? relative : null;
}

export function requireCi(live) {
  if (process.env.CI !== 'true' || process.env.GITHUB_ACTIONS !== 'true' || !process.env.PW_REPORT_ROOT) {
    throw new Error('Pages browser checks must run in GitHub Actions with an explicit evidence directory');
  }
  if (live && (process.env.GITHUB_REF !== 'refs/heads/main' || process.env.GITHUB_EVENT_NAME === 'pull_request')) {
    throw new Error('Published Pages checks are main-only');
  }
}
