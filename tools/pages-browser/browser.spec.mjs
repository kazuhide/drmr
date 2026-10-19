import { test, expect } from '@playwright/test';
import AxeBuilder from '@axe-core/playwright';
import { writeFile } from 'node:fs/promises';
import { assets, digest, liveMode, publicOrigin, localOrigin, allowedRequest, assetPath } from './source.mjs';
import { requireFocus, requireImages, requireLayout, layoutObservation, focusObservation, pollFocus, requireClippedFocusTimeout, FocusConditionTimeout } from './checks.mjs';

const live = liveMode(process.env.PAGES_LIVE);
const origin = live ? publicOrigin : localOrigin;
const fontFiles = ['ibm-plex-sans-latin-400-normal.woff2', 'ibm-plex-sans-latin-600-normal.woff2',
  'ibm-plex-mono-latin-400-normal.woff2'];
const monitors = new WeakMap();

async function artifact(info, name, value, type = 'application/json') {
  const path = info.outputPath(name);
  await writeFile(path, type === 'application/json' ? JSON.stringify(value, null, 2) + '\n' : value);
  await info.attach(name, { path, contentType: type });
}

async function ready(page) {
  const response = await page.goto('/drmr/');
  expect(response.status()).toBe(200);
  await page.evaluate(async () => {
    await document.fonts.ready;
    await Promise.all([...document.fonts].map(font => font.load()));
  });
  await expect(page.getByRole('heading', { level: 1 })).toHaveCount(1);
}

function monitor(page) {
  const evidence = { console: [], pageErrors: [], failedRequests: [], responses: [], focusChecks: [] };
  const pending = [];
  page.on('console', value => {
    if (['error', 'warning'].includes(value.type())) evidence.console.push({ type: value.type(), text: value.text() });
  });
  page.on('pageerror', error => evidence.pageErrors.push(error.message));
  page.on('requestfailed', request => evidence.failedRequests.push(request.url()));
  page.on('response', response => {
    const row = { url: response.url(), status: response.status(), content_type: response.headers()['content-type'],
      cache_control: response.headers()['cache-control'], asset: assetPath(response.url()) };
    evidence.responses.push(row);
    if (row.asset) pending.push(response.body().then(bytes => { row.sha256 = digest(bytes); })
      .catch(() => { row.body_error = 'response-body-unavailable'; }));
  });
  monitors.set(page, { evidence, pending });
  return evidence;
}

// Focus identity alone does not prove the native viewport movement has settled.
// Observe the same complete condition for at most the existing five-second
// element-state budget. Do not scroll, refocus, or restart a failed test.
async function requireTargetFocus(page, target, phase, ordinal) {
  const check = { phase, ordinal, samples: [] };
  monitors.get(page).evidence.focusChecks.push(check);
  try {
    await pollFocus(() => target.evaluate(focusObservation), check.samples);
    requireFocus(check.samples.at(-1));
    check.passed = true;
    return check.samples.at(-1);
  } catch (error) {
    check.failureKind = error instanceof FocusConditionTimeout ? 'condition-timeout' : 'observation-error';
    if (Number.isFinite(error.elapsedMs)) check.failureElapsedMs = error.elapsedMs;
    throw error;
  } finally {
    check.passed ??= false;
    const info = test.info();
    // Persist before another Tab or afterEach network processing, also on failure.
    await writeFile(info.outputPath('keyboard-focus.json'), JSON.stringify({ schema_version: 1,
      source_head: info.config.metadata.sourceHead, source_tree: info.config.metadata.sourceTree,
      mode: info.config.metadata.mode, asset_hashes: info.config.metadata.assetHashes,
      checks: monitors.get(page).evidence.focusChecks }, null, 2) + '\n');
  }
}

test.afterEach(async ({ page }, info) => {
  if (monitors.has(page)) {
    const { evidence, pending } = monitors.get(page);
    await Promise.all(pending);
    await artifact(info, 'browser-observations.json', { schema_version: 1,
      source_head: info.config.metadata.sourceHead, source_tree: info.config.metadata.sourceTree,
      asset_hashes: info.config.metadata.assetHashes, mode: info.config.metadata.mode,
      test_status: info.status, observations: evidence });
  }
});

async function projectOnly(context) {
  // A page requiring a CDN, tracker or backend must fail, not reach the network.
  await context.route('**/*', route => allowedRequest(route.request().url(), live)
    ? route.continue() : route.abort('blockedbyclient'));
}

for (const width of [320, 768, 1024, 1440]) {
  test(`${width}px: layout, assets, keyboard and rendered accessibility`, async ({ page, context, browser }, info) => {
    await projectOnly(context);
    const network = monitor(page);
    await page.setViewportSize({ width, height: 900 });
    await ready(page);
    // Check production HTML before axe injects its test-only engine into the DOM.
    expect(await page.locator('script').count()).toBe(0);
    const layout = await page.evaluate(layoutObservation);
    const screenshot = info.outputPath(`page-${width}.png`);
    await page.screenshot({ path: screenshot, fullPage: true, animations: 'disabled' });
    await info.attach('rendered-page', { path: screenshot, contentType: 'image/png' });
    requireLayout(layout);

    const fonts = await page.evaluate(() => [...document.fonts].map(font => ({
      family: font.family, weight: font.weight, status: font.status,
    })));
    expect(fonts).toHaveLength(3);
    expect(fonts.every(font => font.status === 'loaded')).toBe(true);
    for (const file of fontFiles) expect(network.responses.some(row => row.url.endsWith('/' + file) && row.status === 200)).toBe(true);
    const images = await page.locator('img').evaluateAll(elements => elements.map(image => ({
      src: image.getAttribute('src'), complete: image.complete, width: image.naturalWidth,
    })));
    expect(images).toHaveLength(2);
    requireImages(images);
    expect(network.responses.some(row => row.url.endsWith('/drmr/assets/site.css') && row.status === 200)).toBe(true);

    await expect(page.getByRole('main')).toHaveCount(1);
    for (const name of ['Main navigation', 'Documentation and source']) await expect(page.getByRole('navigation', { name, exact: true })).toHaveCount(1);
    for (const name of ['From declarations to checked media', 'The container matters', 'Four ways back to the original', 'Start with the CLI']) {
      await expect(page.getByRole('heading', { level: 2, name, exact: true })).toBeVisible();
    }
    const headings = await page.locator('h1,h2,h3').evaluateAll(elements => elements.map(element => ({
      level: Number(element.tagName.slice(1)), name: element.textContent.trim().replace(/\s+/g, ' '),
    })));
    expect(headings[0].level).toBe(1);
    for (let i = 1; i < headings.length; i++) {
      expect(headings[i].level).toBeLessThanOrEqual(headings[i - 1].level + 1);
      expect(headings[i].name.length).toBeGreaterThan(0);
    }
    const tree = await page.locator('body').ariaSnapshot();
    expect(tree).toContain('navigation "Main navigation"');
    expect(tree).toContain('navigation "Documentation and source"');
    expect(tree).toContain('main:');
    expect(tree).toContain('link "Read the full support matrix and restrictions"');
    await artifact(info, 'rendered-aria.yml', tree, 'text/yaml');

    // Native Tab/Enter, not script-triggered click, proves skip-link movement.
    await page.keyboard.press('Tab');
    await expect(page.getByRole('link', { name: 'Skip to content', exact: true })).toBeFocused();
    await requireTargetFocus(page, page.getByRole('link', { name: 'Skip to content', exact: true }), 'skip-link', 1);
    await page.keyboard.press('Enter');
    await expect(page.getByRole('main')).toBeFocused();
    await expect(page).toHaveURL(origin + '/drmr/#main');

    await page.goto('about:blank');
    await ready(page);
    const focusable = page.locator('a[href], pre[tabindex="0"]');
    const focusRecords = [];
    for (let i = 0; i < await focusable.count(); i++) {
      await page.keyboard.press('Tab');
      await expect(focusable.nth(i)).toBeFocused();
      const observed = await requireTargetFocus(page, focusable.nth(i), 'tab-order', i + 1);
      focusRecords.push({ ordinal: i + 1, ...observed });
    }
    for (const [name, id] of [['Workflow', 'workflow'], ['Support', 'support'], ['Get started', 'get-started']]) {
      const link = page.getByRole('navigation', { name: 'Main navigation', exact: true }).getByRole('link', { name, exact: true });
      await link.focus();
      await page.keyboard.press('Enter');
      await expect(page).toHaveURL(origin + '/drmr/#' + id);
      const box = await page.locator('#' + id).boundingBox();
      expect(box.y).toBeGreaterThanOrEqual(0);
      expect(box.y).toBeLessThan(900);
    }
    // Every served asset must match this checkout byte for byte, including notices.
    const servedAssets = [];
    for (const path of assets) {
      const response = await page.request.get(origin + '/drmr/' + (path === 'index.html' ? '' : path),
        { maxRedirects: 0, headers: { 'Cache-Control': 'no-cache' } });
      expect(response.status()).toBe(200);
      const sha256 = digest(await response.body());
      expect(sha256).toBe(info.config.metadata.assetHashes[path]);
      servedAssets.push({ path, sha256, headers: response.headers() });
    }
    const license = page.getByRole('link', { name: 'Font license', exact: true });
    expect(new URL(await license.getAttribute('href'), page.url()).href)
      .toBe(origin + '/drmr/assets/fonts/LICENSE-ibm-plex.txt');
    const accessibility = await new AxeBuilder({ page }).analyze();
    await artifact(info, 'axe.json', accessibility);
    expect(accessibility.violations).toEqual([]);
    expect(network.console).toEqual([]);
    expect(network.pageErrors).toEqual([]);
    expect(network.failedRequests).toEqual([]);
    await Promise.all(monitors.get(page).pending);
    expect(network.responses.every(row => allowedRequest(row.url, live) && row.status === 200)).toBe(true);
    for (const row of network.responses.filter(row => row.asset)) {
      expect(row.body_error).toBeUndefined();
      expect(row.sha256).toBe(info.config.metadata.assetHashes[row.asset]);
    }
    await artifact(info, 'evidence.json', { schema_version: 1, source_head: info.config.metadata.sourceHead,
      source_tree: info.config.metadata.sourceTree, mode: info.config.metadata.mode,
      asset_hashes: info.config.metadata.assetHashes, servedAssets, chromium: browser.version(), viewport: { width, height: 900 }, production_scripts: 0,
      layout, fonts, images, headings, keyboard: focusRecords, network,
      axe: { version: accessibility.testEngine.version, violations: 0, incomplete: accessibility.incomplete.length },
      limitations: ['automated Chromium only; no manual screen-reader or other-browser qualification',
        'external repository links checked by source validator; no external browser navigation'] });
  });
}

test('core content and anchor navigation work without production JavaScript', async ({ browser }, info) => {
  const context = await browser.newContext({ javaScriptEnabled: false, viewport: { width: 320, height: 900 } });
  try {
    await projectOnly(context);
    const page = await context.newPage();
    const mounts = live ? ['/drmr/'] : ['/', '/drmr/'];
    for (const mount of mounts) {
      const response = await page.goto(origin + mount);
      expect(response.status()).toBe(200);
      await expect(page.getByRole('main')).toBeVisible();
      await expect(page.getByRole('heading', { level: 1 })).toHaveCount(1);
      const link = page.getByRole('navigation', { name: 'Main navigation' }).getByRole('link', { name: 'Workflow', exact: true });
      for (let i = 0; i < 3; i++) await page.keyboard.press('Tab');
      await expect(link).toBeFocused();
      await page.keyboard.press('Enter');
      await expect(page).toHaveURL(origin + mount + '#workflow');
      await expect(page.locator('#workflow')).toBeInViewport();
    }
    await artifact(info, 'evidence.json', { schema_version: 1, source_head: info.config.metadata.sourceHead,
      java_script_enabled: false, mode: info.config.metadata.mode, mounts, browser: browser.version() });
  } finally {
    await context.close();
  }
});

test('browser guard controls detect real overflow, invisible focus and a missing image', async ({ page, context }, info) => {
  await projectOnly(context);
  await ready(page);
  const overflow = await page.addStyleTag({ content: 'body { min-width: 2000px !important; }' });
  const badLayout = await page.evaluate(layoutObservation);
  expect(() => requireLayout(badLayout)).toThrow();
  await overflow.evaluate(element => element.remove());
  requireLayout(await page.evaluate(layoutObservation));
  await page.keyboard.press('Tab');
  const hiddenFocus = await page.addStyleTag({ content: ':focus-visible { outline: none !important; }' });
  const badFocus = await page.evaluate(focusObservation);
  expect(() => requireFocus(badFocus)).toThrow();
  await hiddenFocus.evaluate(element => element.remove());
  requireFocus(await page.evaluate(focusObservation));
  const missing = page.waitForResponse(response => response.url().endsWith('/drmr/missing-image.svg'));
  await page.evaluate(() => {
    const image = document.createElement('img');
    image.id = 'missing-image-control';
    image.src = './missing-image.svg';
    document.body.append(image);
  });
  expect((await missing).status()).toBe(404);
  await expect(page.locator('#missing-image-control')).toHaveJSProperty('naturalWidth', 0);
  const badImages = await page.locator('img').evaluateAll(elements => elements.map(image => ({
    complete: image.complete, width: image.naturalWidth,
  })));
  expect(() => requireImages(badImages)).toThrow();
  await artifact(info, 'negative-controls.json', { schema_version: 1, badLayout, badFocus,
    missing_image_status: 404, badImages });
});

test('native Tab keeps edge targets fully visible only with the production scroll margin', async ({ page, context }, info) => {
  await projectOnly(context);
  monitor(page);
  const comparisons = [];
  try {
    for (const [width, ordinal, expected] of [
      [320, 17, page.locator('pre[aria-label="Build and check the CLI"]')],
      [1440, 18, page.getByRole('link', { name: 'Source installation instructions', exact: true })],
    ]) {
      // Replay the same native Tab sequence in a fresh document for each state.
      for (const state of ['production', 'disabled-control', 'restored']) {
        await page.setViewportSize({ width, height: 900 });
        await page.goto('about:blank');
        await ready(page);
        if (state === 'disabled-control') {
          await page.addStyleTag({ content: 'a[href], pre[tabindex="0"] { scroll-margin-block: 0 !important; }' });
        }
        expect(await expected.evaluate(element => getComputedStyle(element).scrollMarginBlockStart))
          .toBe(state === 'disabled-control' ? '0px' : '16px');
        const focusable = page.locator('a[href], pre[tabindex="0"]');
        for (let i = 0; i < ordinal; i++) {
          await page.keyboard.press('Tab');
          await expect(focusable.nth(i)).toBeFocused();
        }
        await expect(expected).toBeFocused();
        const phase = `scroll-margin-${width}-${state}`;
        let failure;
        try {
          await requireTargetFocus(page, expected, phase, ordinal);
        } catch (error) {
          failure = error;
        }
        const check = monitors.get(page).evidence.focusChecks.at(-1);
        comparisons.push({ width, state, ...check });
        if (state === 'disabled-control') {
          // The negative must be actual partial clipping of the correct target,
          // not a missing element, broken outline or unrelated failure.
          requireClippedFocusTimeout(failure, check);
        } else {
          if (failure) throw failure;
          requireFocus(check.samples.at(-1));
        }
      }
    }
  } finally {
    await artifact(info, 'scroll-margin-comparison.json', { schema_version: 1,
      source_head: info.config.metadata.sourceHead, source_tree: info.config.metadata.sourceTree,
      mode: info.config.metadata.mode, comparisons });
  }
});
