import { defineConfig } from '@playwright/test';
import { readFileSync } from 'node:fs';
import { createRequire } from 'node:module';
import { dirname, join, resolve } from 'node:path';
import { consoleIdentity, consoleOrigin, requireConsoleCi } from './console-source.mjs';

requireConsoleCi();
const identity = consoleIdentity();
const require = createRequire(import.meta.url);
const pins = JSON.parse(readFileSync(join(dirname(require.resolve('playwright-core/package.json')), 'browsers.json'), 'utf8'));
const lock = JSON.parse(readFileSync(new URL('./package-lock.json', import.meta.url), 'utf8'));
const report = resolve(process.env.PW_REPORT_ROOT);

// This independent suite does not alter the landing or guide suite deadlines.
export default defineConfig({
  testDir: '.', testMatch: 'console.spec.mjs', fullyParallel: false, workers: 1,
  retries: 0, forbidOnly: true, timeout: 90_000, globalTimeout: 600_000,
  expect: { timeout: 10_000 }, outputDir: join(report, 'results'),
  reporter: [['list'], ['json', { outputFile: join(report, 'results.json') }],
    ['html', { outputFolder: join(report, 'html'), open: 'never' }]],
  metadata: { ...identity, suite: 'console', languages: ['en', 'ja'], node: process.version,
    packages: Object.fromEntries(Object.entries(lock.packages).filter(([name]) => name.startsWith('node_modules/'))
      .map(([name, value]) => [name.slice('node_modules/'.length), value.version])),
    browserPins: pins.browsers.filter(({ name }) => name.startsWith('chromium')),
    limits: 'Actual Linux Chromium and media tools; no native WebView, other-browser, device, license or playback qualification' },
  use: { browserName: 'chromium', baseURL: consoleOrigin, viewport: { width: 1440, height: 1000 },
    locale: 'en-US', reducedMotion: 'reduce', serviceWorkers: 'block',
    screenshot: 'only-on-failure', trace: 'retain-on-failure', actionTimeout: 10_000, navigationTimeout: 10_000 },
  webServer: { command: 'node console-server.mjs', url: consoleOrigin + '/api/common/capabilities',
    reuseExistingServer: false, timeout: 30_000,
    gracefulShutdown: { signal: 'SIGTERM', timeout: 30_000 } },
});
