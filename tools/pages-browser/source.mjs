import { createHash } from 'node:crypto';
import { readFileSync } from 'node:fs';
import { execFileSync } from 'node:child_process';

export const assets = [
  'index.html', 'assets/site.css', 'assets/drmr.svg',
  'assets/fonts/LICENSE-ibm-plex.txt', 'assets/fonts/README.txt',
  'assets/fonts/ibm-plex-sans-latin-400-normal.woff2',
  'assets/fonts/ibm-plex-sans-latin-600-normal.woff2',
  'assets/fonts/ibm-plex-mono-latin-400-normal.woff2',
];
export const publicOrigin = 'https://kazuhide.github.io';
export const publicBase = publicOrigin + '/drmr/';
export const localOrigin = 'http://127.0.0.1:4173';
export const digest = bytes => createHash('sha256').update(bytes).digest('hex');
export function source() {
  return {
    sourceHead: execFileSync('git', ['rev-parse', 'HEAD'], { encoding: 'utf8' }).trim(),
    sourceTree: execFileSync('git', ['rev-parse', 'HEAD^{tree}'], { encoding: 'utf8' }).trim(),
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
  const relative = url.pathname === '/drmr/' ? 'index.html' : url.pathname.slice('/drmr/'.length);
  return url.pathname.startsWith('/drmr/') && assets.includes(relative) ? relative : null;
}

export function requireCi(live) {
  if (process.env.CI !== 'true' || process.env.GITHUB_ACTIONS !== 'true' || !process.env.PW_REPORT_ROOT) {
    throw new Error('Pages browser checks must run in GitHub Actions with an explicit evidence directory');
  }
  if (live && (process.env.GITHUB_REF !== 'refs/heads/main' || process.env.GITHUB_EVENT_NAME === 'pull_request')) {
    throw new Error('Published Pages checks are main-only');
  }
}
