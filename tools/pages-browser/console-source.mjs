import { createHash } from 'node:crypto';
import { readFileSync, statSync } from 'node:fs';
import { execFileSync } from 'node:child_process';
import { resolve } from 'node:path';

export const consoleOrigin = 'http://127.0.0.1:4174';
export const fixtureOrigin = 'http://127.0.0.1:4175';
export const consoleAssets = [
  'index.html', 'app.css', 'js/api.js', 'js/ui.js', 'js/i18n.js',
  'js/common-api.js', 'js/common-state.js', 'js/common-report.js',
  'js/common-files.js', 'js/common-view.js', 'js/common-console.js',
];
export const digest = bytes => createHash('sha256').update(bytes).digest('hex');
export function requireConsoleCi(env = process.env) {
  if (env.CI !== 'true' || env.GITHUB_ACTIONS !== 'true' || env.RUNNER_OS !== 'Linux' ||
      !env.PW_REPORT_ROOT || !env.DRMR_CONSOLE_BINARY || !env.DRMR_CONSOLE_BUILD ||
      !/^[0-9a-f]{40}$/.test(env.GITHUB_SHA || '')) {
    throw new Error('Console browser qualification requires its Linux CI source and owned evidence directory');
  }
}
export function allowedConsoleRequest(raw) {
  const url = new URL(raw);
  return !url.username && !url.password &&
    [consoleOrigin, fixtureOrigin].includes(url.origin);
}
export function consoleIdentity() {
  requireConsoleCi();
  const git = (...args) => execFileSync('git', args, { encoding: 'utf8' }).trim();
  const sourceHead = git('rev-parse', 'HEAD');
  if (sourceHead !== process.env.GITHUB_SHA || git('status', '--porcelain', '--untracked-files=all')) {
    throw new Error('Console qualification requires the unchanged workflow checkout');
  }
  const binary = resolve(process.env.DRMR_CONSOLE_BINARY);
  if (!statSync(binary).isFile()) throw new Error('The actual CLI binary is missing');
  const buildBytes = readFileSync(process.env.DRMR_CONSOLE_BUILD);
  const artifacts = buildBytes.toString('utf8').trim().split('\n').map(line => JSON.parse(line))
    .filter(item => item.reason === 'compiler-artifact' && item.target.name === 'drmr' &&
      item.target.kind.includes('bin') && item.executable && !item.profile.test);
  if (artifacts.length !== 1 || resolve(artifacts[0].executable) !== binary) {
    throw new Error('CLI binary does not match the observed Cargo build artifact');
  }
  const assetHashes = Object.fromEntries(consoleAssets.map(path => [path,
    digest(readFileSync(new URL('../../crates/drmr-server/static/' + path, import.meta.url)))]));
  return { sourceHead, sourceTree: git('rev-parse', 'HEAD^{tree}'), binarySha256: digest(readFileSync(binary)),
    buildSha256: digest(buildBytes), buildProfile: artifacts[0].profile, assetHashes,
    fixtureSha256: digest(readFileSync(new URL('../../crates/drmr/tests/fixtures/bbb/clip.mp4', import.meta.url))) };
}
