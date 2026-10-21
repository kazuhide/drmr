import { defineConfig } from '@playwright/test';
import { readFileSync } from 'node:fs';
import { createRequire } from 'node:module';
import { dirname, join, resolve } from 'node:path';
import { source, liveMode, requireCi, publicOrigin, localOrigin } from './source.mjs';

// Browser execution is authorized in GitHub Actions only, not on a local host.
const live = liveMode(process.env.PAGES_LIVE);
requireCi(live);
const identity = source();
const { sourceHead } = identity;
if (sourceHead !== process.env.GITHUB_SHA) throw new Error('Checkout does not match the workflow source');
const require = createRequire(import.meta.url);
const lock = JSON.parse(readFileSync(new URL('./package-lock.json', import.meta.url), 'utf8'));
const pins = JSON.parse(readFileSync(join(dirname(require.resolve('playwright-core/package.json')), 'browsers.json'), 'utf8'));
const report = resolve(process.env.PW_REPORT_ROOT);

export default defineConfig({
  testDir: '.',
  testMatch: 'browser.spec.mjs',
  fullyParallel: false,
  workers: 1,
  retries: 0,
  forbidOnly: true,
  timeout: 30_000,
  globalTimeout: 180_000,
  expect: { timeout: 5000 },
  outputDir: join(report, 'results'),
  reporter: [['list'], ['json', { outputFile: join(report, 'results.json') }],
    ['html', { outputFolder: join(report, 'html'), open: 'never' }]],
  metadata: {
    ...identity,
    mode: live ? 'published' : 'loopback',
    languages: ['en', 'ja'],
    suite: 'landing',
    browserCases: 19,
    node: process.version,
    packages: Object.fromEntries(Object.entries(lock.packages).filter(([name]) => name.startsWith('node_modules/'))
      .map(([name, value]) => [name.slice('node_modules/'.length), value.version])),
    browserPins: pins.browsers.filter(({ name }) => name.startsWith('chromium')),
    limits: 'CI Chromium and automated axe checks; no manual screen-reader, other-browser or device qualification',
  },
  use: {
    browserName: 'chromium',
    baseURL: live ? publicOrigin : localOrigin,
    viewport: { width: 1280, height: 900 },
    locale: 'en-US',
    reducedMotion: 'reduce',
    serviceWorkers: 'block',
    screenshot: 'only-on-failure',
    trace: 'retain-on-failure',
    actionTimeout: 5000,
    navigationTimeout: 10_000,
  },
  webServer: live ? undefined : {
    command: 'node server.mjs',
    url: 'http://127.0.0.1:4173/drmr/',
    reuseExistingServer: false,
    timeout: 10_000,
    gracefulShutdown: { signal: 'SIGTERM', timeout: 5000 },
  },
});
