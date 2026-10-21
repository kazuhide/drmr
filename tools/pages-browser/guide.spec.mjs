import { test, expect } from '@playwright/test';
import AxeBuilder from '@axe-core/playwright';
import { writeFile } from 'node:fs/promises';
import { assets, assetPath, assetRoute, allowedRequest, digest, liveMode, publicOrigin, localOrigin } from './source.mjs';
import { focusObservation, pollFocus, requireFocus, requireImages } from './checks.mjs';
import { completeMatrixKey } from './matrix.mjs';
import { guideChapters, guideLocales, guideObservation, requireGuideLayout,
  guideScrollObservation, requireGuideMovement, codeAriaTemplate, requireGuideCheckpoint } from './guide.mjs';
import { matrixRecipes } from './recipes.mjs';

const live = liveMode(process.env.PAGES_LIVE);
const origin = live ? publicOrigin : localOrigin;
const records = new WeakMap();
const focusSelector = 'a[href],pre[tabindex="0"],.guide-table-scroll[tabindex="0"]';
const compact = text => text.trim().replace(/\s+/g, ' ');
function identity(info) {
  return { schema_version: 1, suite: 'guide', source_head: info.config.metadata.sourceHead,
    source_tree: info.config.metadata.sourceTree, mode: info.config.metadata.mode,
    asset_hashes: info.config.metadata.assetHashes, builder_inputs: info.config.metadata.builderInputHashes };
}
async function attach(info, name, value, type = 'application/json') {
  const path = info.outputPath(name);
  await writeFile(path, type === 'application/json' ? JSON.stringify(value, null, 2) + '\n' : value);
  await info.attach(name, { path, contentType: type });
}
function watch(page) {
  const value = { console: [], pageErrors: [], failedRequests: [], responses: [], focus: [], scrolls: [], checkpoints: [], phases: [] };
  const pending = [];
  page.on('console', message => {
    if (['error', 'warning'].includes(message.type())) value.console.push(message.text());
  });
  page.on('pageerror', error => value.pageErrors.push(error.message));
  page.on('requestfailed', request => value.failedRequests.push(request.url()));
  page.on('response', response => {
    const row = { url: response.url(), status: response.status(), asset: assetPath(response.url()),
      content_type: response.headers()['content-type'], cache_control: response.headers()['cache-control'] };
    value.responses.push(row);
    if (row.asset) pending.push(response.body().then(bytes => { row.sha256 = digest(bytes); })
      .catch(() => { row.body_error = 'response-body-unavailable'; }));
  });
  records.set(page, { value, pending });
  return value;
}
async function confined(context) {
  await context.route('**/*', route => allowedRequest(route.request().url(), live)
    ? route.continue() : route.abort('blockedbyclient'));
}
async function timed(record, name, action) {
  const phase = { name, started_ms: performance.now() };
  record.phases.push(phase);
  try {
    const value = await action();
    phase.passed = true;
    return value;
  } finally {
    phase.elapsed_ms = performance.now() - phase.started_ms;
    phase.passed ??= false;
  }
}
async function ready(page, locale, mount = '/drmr/', fragment = '') {
  await page.goto('about:blank');
  const response = await page.goto(origin + mount + locale.route + fragment);
  expect(response.status()).toBe(200);
  expect(digest(await response.body())).toBe(test.info().config.metadata.assetHashes[locale.asset]);
  await expect(page.locator('html')).toHaveAttribute('lang', locale.language);
  await expect(page.getByRole('heading', { level: 1, name: locale.title, exact: true })).toHaveCount(1);
  await page.evaluate(async () => { await document.fonts.ready;
    await Promise.all([...document.fonts].map(font => font.load())); });
}
async function focus(page, target, phase) {
  const record = { phase, samples: [] };
  records.get(page).value.focus.push(record);
  try {
    const sample = await pollFocus(() => target.evaluate(focusObservation), record.samples);
    requireFocus(sample);
    record.passed = true;
  } finally { record.passed ??= false; }
}
async function tabTo(page, target, phase) {
  const limit = await page.locator(focusSelector).count();
  expect(limit).toBeGreaterThan(0);
  for (let ordinal = 1; ordinal <= limit; ordinal++) {
    await page.keyboard.press('Tab');
    if (await target.evaluate(element => element === document.activeElement)) {
      await focus(page, target, phase);
      return ordinal;
    }
  }
  throw new Error('Native Tab did not reach the requested guide target');
}
async function jumpToChapter(page, locale, chapter) {
  const link = page.getByRole('navigation', { name: locale.toc, exact: true }).locator(`a[href="#${chapter}"]`);
  await tabTo(page, link, 'chapter-contents');
  await page.keyboard.press('Enter');
  await expect(page).toHaveURL(new RegExp('/' + locale.route + '#' + chapter + '$'));
  const rect = await page.locator('#' + chapter).boundingBox();
  expect(rect.y).toBeGreaterThanOrEqual(0);
  expect(rect.y).toBeLessThan(900);
}
async function scrollTarget(page, locator, checkpoint) {
  const target = await locator.elementHandle();
  expect(target).not.toBeNull();
  const record = { samples: [] };
  records.get(page).value.scrolls.push(record);
  try {
    const initial = await target.evaluate(guideScrollObservation);
    expect(initial.focused).toBe(true);
    expect(initial.scrollLeft).toBe(0);
    record.samples.push(initial);
    record.scrollNeeded = initial.scrollWidth > initial.clientWidth;
    if (!record.scrollNeeded) { record.passed = true; await checkpoint(target); return record; }
    const deadline = performance.now() + 5000;
    const move = async key => {
      const after = await completeMatrixKey({ before: record.samples.at(-1),
        direction: key === 'ArrowRight' ? 'right' : 'left', deadline, validateMovement: requireGuideMovement,
        arm: async () => {
          const handle = await target.evaluateHandle(element => {
            const signal = { element };
            signal.completion = new Promise(resolve => { signal.finish = resolve; });
            signal.listener = event => { if (event.target === element) signal.finish(true); };
            element.addEventListener('scrollend', signal.listener);
            return signal;
          });
          return { handle, dispose: async () => {
            try { await handle.evaluate(signal => {
              signal.element.removeEventListener('scrollend', signal.listener); signal.finish(false);
            }); } finally { await handle.dispose(); }
          } };
        },
        press: () => page.keyboard.press(key),
        wait: async signal => expect(await signal.handle.evaluate(value => value.completion)).toBe(true),
        observe: () => target.evaluate(guideScrollObservation),
      });
      record.samples.push({ ...after, completedKey: key, scrollEndObserved: true });
    };
    for (let count = 0; record.samples.at(-1).scrollLeft < initial.scrollWidth - initial.clientWidth && count < 64; count++) await move('ArrowRight');
    expect(record.samples.at(-1).scrollLeft).toBe(initial.scrollWidth - initial.clientWidth);
    for (let count = 0; record.samples.at(-1).scrollLeft > 0 && count < 64; count++) await move('ArrowLeft');
    expect(record.samples.at(-1).scrollLeft).toBe(0);
    record.passed = true;
    await checkpoint(target);
    return record;
  } finally { record.passed ??= false; await target.dispose(); }
}
async function content(page, locale) {
  const observed = await page.evaluate(guideObservation);
  requireGuideLayout(observed);
  const toc = page.getByRole('navigation', { name: locale.toc, exact: true });
  await expect(toc.locator('a')).toHaveCount(20);
  expect(await toc.locator('a').evaluateAll(nodes => nodes.map(node => node.getAttribute('href'))))
    .toEqual(guideChapters.map(id => '#' + id));
  await expect(page.locator('script')).toHaveCount(0);
  expect(await page.locator('#url-input h3[id^="recipe-"]:not(#recipe-setup)')
    .evaluateAll(nodes => nodes.map(node => ({ id: node.id, tabindex: node.getAttribute('tabindex') }))))
    .toEqual(matrixRecipes.map(recipe => ({ id: recipe.id, tabindex: '-1' })));
  const groups = [];
  const groupLocators = await page.locator('figure.guide-example').all();
  // The page is static. Read each figure's DOM facts in one protocol call and
  // batch independent figures; retain every rendered/ARIA assertion and order.
  for (let start = 0; start < groupLocators.length; start += 16) {
    groups.push(...await Promise.all(groupLocators.slice(start, start + 16).map(async group => {
      const pre = group.locator('pre');
      const [facts, aria] = await Promise.all([
        group.evaluate(node => ({ role: node.getAttribute('role'),
          captionText: node.querySelector('figcaption').innerText,
          code: node.querySelector('pre code').textContent,
          rendered: node.querySelector('pre code').innerText,
          language: node.getAttribute('data-code-language') })),
        group.ariaSnapshot(),
      ]);
      const { role, captionText, code, rendered, language } = facts;
      expect(role).toBeNull();
      const caption = compact(captionText);
      expect(code.trim()).not.toBe('');
      expect(compact(rendered)).toBe(compact(code));
      await Promise.all([
        expect(group).toHaveRole('figure'),
        expect(group).toHaveAttribute('data-code-language', /^(sh|bash|powershell|json|text)$/),
        expect(group).toHaveAccessibleName(caption),
        expect(pre).toHaveAttribute('tabindex', '0'),
        expect(pre.locator('code')).toMatchAriaSnapshot(codeAriaTemplate(code)),
      ]);
      return { label: caption, language, role: 'figure', code, rendered, aria };
    })));
  }
  expect(groups.length).toBeGreaterThan(0);
  const tables = [];
  const regions = await page.locator('.guide-table-scroll').all();
  for (let start = 0; start < regions.length; start += 16) {
    tables.push(...await Promise.all(regions.slice(start, start + 16).map(async region => {
      const [captionText, aria, scopes] = await Promise.all([
        region.locator('caption').innerText(), region.ariaSnapshot(),
        region.locator('th').evaluateAll(headers => headers.map(header => header.getAttribute('scope'))),
      ]);
      const caption = compact(captionText);
      for (const scope of scopes) expect(['row', 'col']).toContain(scope);
      await Promise.all([
        expect(region).toHaveAttribute('tabindex', '0'),
        expect(region).toHaveAttribute('role', 'region'),
        expect(region).toHaveAccessibleName(caption),
      ]);
      return { caption, aria };
    })));
  }
  expect(tables.length).toBeGreaterThan(0);
  const chapterLinks = page.locator('.guide-chapter-switch');
  await expect(chapterLinks).toHaveCount(20);
  const links = await chapterLinks.all();
  for (let start = 0; start < links.length; start += 16) {
    await Promise.all(links.slice(start, start + 16).map(async (link, offset) => {
      const href = await link.getAttribute('href');
      expect(new URL(href, page.url()).href)
        .toBe(origin + '/drmr/' + locale.switchRoute + '#' + guideChapters[start + offset]);
      await Promise.all([
        expect(link).toHaveAttribute('lang', locale.other),
        expect(link).toHaveAttribute('hreflang', locale.other),
      ]);
    }));
  }
  const missingAnchors = await page.locator('a[href^="#"]').evaluateAll(nodes => nodes
    .filter(node => !document.getElementById(node.getAttribute('href').slice(1))).map(node => node.getAttribute('href')));
  expect(missingAnchors).toEqual([]);
  return { layout: observed, code_groups: groups, tables };
}
test.afterEach(async ({ page }, info) => {
  if (records.has(page)) {
    const { value, pending } = records.get(page);
    await Promise.all(pending);
    await attach(info, 'guide-observations.json', { ...identity(info), status: info.status, observations: value });
  }
});

for (const locale of guideLocales) for (const width of [320, 768, 1024, 1440]) {
  test(`${locale.language} guide / ${width}px: complete chapters, keyboard, assets and accessibility`, async ({ page, context, browser }, info) => {
    await confined(context); const network = watch(page);
    await page.setViewportSize({ width, height: 900 });
    await timed(network, 'initial-navigation', () => ready(page, locale));
    const initial = await page.evaluate(() => ({ scrollX, scrollY, focusedTag: document.activeElement.tagName }));
    expect(initial).toEqual({ scrollX: 0, scrollY: 0, focusedTag: 'BODY' });
    const initialPath = info.outputPath('initial-viewport-before-interaction.png');
    await page.screenshot({ path: initialPath, fullPage: false, animations: 'disabled', timeout: 5000 });
    await info.attach('initial-viewport-before-interaction', { path: initialPath, contentType: 'image/png' });
    if (locale.language === 'en' && width === 320) {
      const heading = page.locator('#introduction-title');
      const previous = await heading.evaluate(node => node.style.visibility);
      let hidden;
      try {
        await heading.evaluate(node => { node.style.visibility = 'hidden'; });
        hidden = await page.evaluate(guideObservation);
        expect(() => requireGuideLayout(hidden)).toThrow('every chapter');
      } finally { await heading.evaluate((node, value) => { node.style.visibility = value; }, previous); }
      const restored = await page.evaluate(guideObservation); requireGuideLayout(restored);
      await attach(info, 'guide-negative-controls.json', { ...identity(info), hidden, restored });
    }
    const contents = await timed(network, 'complete-content-and-figure-aria', () => content(page, locale));
    const aria = await timed(network, 'complete-body-aria', () => page.locator('body').ariaSnapshot());
    await attach(info, 'rendered-aria.yml', aria, 'text/yaml');
    expect(aria).toContain('main:');
    const fonts = await page.evaluate(() => [...document.fonts].map(font => ({ family: font.family, weight: font.weight, status: font.status })));
    expect(fonts).toHaveLength(3); expect(fonts.every(font => font.status === 'loaded')).toBe(true);
    const images = await page.locator('img').evaluateAll(nodes => nodes.map(node => ({ complete: node.complete, width: node.naturalWidth, src: node.currentSrc })));
    requireImages(images);
    expect(images.every(image => image.src.endsWith('/assets/drmr.svg'))).toBe(true);
    await page.keyboard.press('Tab'); await expect(page.getByRole('link', { name: locale.skip, exact: true })).toBeFocused();
    await focus(page, page.getByRole('link', { name: locale.skip, exact: true }), 'skip-link');
    await page.keyboard.press('Enter'); await expect(page.getByRole('main')).toBeFocused();
    const native = [];
    for (const selector of ['pre[tabindex="0"]', '.guide-table-scroll']) {
      const started = performance.now();
      await ready(page, locale);
      const candidates = await page.locator(selector).evaluateAll(nodes => nodes.map((node, index) => ({ index,
        extent: node.scrollWidth - node.clientWidth, chapter: node.closest('[data-guide-chapter]').id,
        tag: node.tagName, identity: node.getAttribute('aria-labelledby') || node.parentElement.getAttribute('aria-labelledby') })));
      const selected = candidates.reduce((best, item) => item.extent > best.extent ? item : best);
      await jumpToChapter(page, locale, selected.chapter);
      const target = page.locator(selector).nth(selected.index);
      const ordinal = await tabTo(page, target, 'native-scroll-owner');
      const phase = selector.startsWith('pre') ? 'native-code-viewport' : 'native-table-viewport';
      const expected = { source_head: info.config.metadata.sourceHead, source_tree: info.config.metadata.sourceTree,
        language: locale.language, page_asset: locale.asset, page_sha256: info.config.metadata.assetHashes[locale.asset],
        viewport: { width, height: 900 } };
      const checkpoint = async owner => {
        await focus(page, owner, phase);
        const observe = async () => ({ scroll: await owner.evaluate(guideScrollObservation), focus: await owner.evaluate(focusObservation) });
        const record = { ...expected, phase, selected, before: await observe() };
        network.checkpoints.push(record);
        try {
          requireGuideCheckpoint({ ...record, after: record.before }, expected);
          const path = info.outputPath(phase + '.png');
          await page.screenshot({ path, fullPage: false, animations: 'disabled', timeout: 5000 });
          record.after = await observe();
          requireGuideCheckpoint(record, expected);
          await info.attach(phase, { path, contentType: 'image/png' });
          record.passed = true;
        } finally { record.passed ??= false; }
      };
      native.push({ selector, selected, ordinal, traversal: await scrollTarget(page, target, checkpoint) });
      network.phases.push({ name: phase, started_ms: started, elapsed_ms: performance.now() - started, passed: true });
    }
    const served = [];
    for (const path of assets) {
      const response = await page.request.get(origin + '/drmr/' + assetRoute(path), { maxRedirects: 0 });
      expect(response.status()).toBe(200); const sha256 = digest(await response.body());
      expect(sha256).toBe(info.config.metadata.assetHashes[path]); served.push({ path, sha256, headers: response.headers() });
    }
    const axe = await timed(network, 'complete-axe', () => new AxeBuilder({ page }).analyze());
    await attach(info, 'axe.json', axe);
    expect(axe.violations).toEqual([]); expect(axe.incomplete).toEqual([]);
    expect(network.console).toEqual([]); expect(network.pageErrors).toEqual([]); expect(network.failedRequests).toEqual([]);
    await Promise.all(records.get(page).pending);
    expect(network.responses.every(row => allowedRequest(row.url, live) && row.status === 200 && !row.body_error &&
      (!row.asset || row.sha256 === info.config.metadata.assetHashes[row.asset]))).toBe(true);
    await attach(info, 'evidence.json', { ...identity(info), language: locale.language, page_asset: locale.asset,
      page_sha256: info.config.metadata.assetHashes[locale.asset], viewport: { width, height: 900 }, initial_viewport: initial,
      chapters: contents, native_keyboard: native, fonts, images, served_assets: served, network,
      visual_checkpoints: { contract: 'viewport-native-v1', records: network.checkpoints },
      chromium: browser.version(), production_scripts: 0, axe: { violations: 0, incomplete: 0 } });
  });
}

for (const locale of guideLocales) test(`${locale.language} guide: chapters and native contents work without JavaScript`, async ({ browser }, info) => {
  const context = await browser.newContext({ javaScriptEnabled: false, viewport: { width: 320, height: 900 } });
  try {
    await confined(context); const page = await context.newPage(); const network = watch(page); const pages = [];
    for (const mount of live ? ['/drmr/'] : ['/', '/drmr/']) {
      await ready(page, locale, mount); await expect(page.locator('[data-guide-chapter]')).toHaveCount(20);
      await expect(page.locator('script')).toHaveCount(0);
      const first = page.getByRole('navigation', { name: locale.toc, exact: true }).locator('a[href="#introduction"]');
      await tabTo(page, first, 'no-js-contents'); await page.keyboard.press('Enter');
      await expect(page).toHaveURL(origin + mount + locale.route + '#introduction');
      await expect(page.locator('#introduction')).toBeInViewport(); pages.push({ mount, asset: locale.asset, sha256: info.config.metadata.assetHashes[locale.asset] });
    }
    await Promise.all(records.get(page).pending);
    expect(network.console).toEqual([]); expect(network.pageErrors).toEqual([]); expect(network.failedRequests).toEqual([]);
    expect(network.responses.every(row => allowedRequest(row.url, live) && row.status === 200 && !row.body_error &&
      (!row.asset || row.sha256 === info.config.metadata.assetHashes[row.asset]))).toBe(true);
    await attach(info, 'evidence.json', { ...identity(info), language: locale.language, java_script_enabled: false, pages, network });
  } finally { await context.close(); }
});

test('guide native language links retain the selected chapter in both directions', async ({ page, context }, info) => {
  await confined(context); watch(page); const hops = [];
  for (const mount of live ? ['/drmr/'] : ['/', '/drmr/']) {
    await ready(page, guideLocales[0], mount);
    await jumpToChapter(page, guideLocales[0], 'verification');
    for (const [from, to] of [[guideLocales[0], guideLocales[1]], [guideLocales[1], guideLocales[0]]]) {
      const link = page.locator('#verification .guide-chapter-switch');
      await tabTo(page, link, 'chapter-language'); await page.keyboard.press('Enter');
      await expect(page).toHaveURL(origin + mount + to.route + '#verification');
      await expect(page.locator('html')).toHaveAttribute('lang', to.language);
      const response = await page.request.get(page.url(), { maxRedirects: 0 });
      const sha256 = digest(await response.body()); expect(sha256).toBe(info.config.metadata.assetHashes[to.asset]);
      hops.push({ mount, from: from.language, to: to.language, chapter: 'verification', asset: to.asset, sha256 });
    }
  }
  await attach(info, 'language-roundtrip.json', { ...identity(info), hops });
});

test('both guides retain all chapters, code and tables in print media', async ({ page, context }, info) => {
  await confined(context); watch(page); const pages = [];
  for (const locale of guideLocales) {
    await ready(page, locale); await page.emulateMedia({ media: 'print' });
    const layout = await page.evaluate(guideObservation); requireGuideLayout(layout);
    const styles = await page.locator('.guide-toc,.guide-chapter-switch,.language-switch,.guide-header-nav,.guide-toplink')
      .evaluateAll(nodes => nodes.map(node => ({ class: node.className, display: getComputedStyle(node).display })));
    expect(styles.length).toBeGreaterThan(20); expect(styles.every(row => row.display === 'none')).toBe(true);
    const contentStyles = await page.locator('pre,.guide-table-scroll').evaluateAll(nodes => nodes.map(node => ({
      tag: node.tagName, overflow: getComputedStyle(node).overflowX, whiteSpace: getComputedStyle(node).whiteSpace,
      text: node.textContent.trim(), shown: node.getClientRects().length > 0 })));
    expect(contentStyles.length).toBeGreaterThan(0);
    expect(contentStyles.every(row => row.shown && row.text && row.overflow === 'visible' && (row.tag !== 'PRE' || row.whiteSpace === 'pre-wrap'))).toBe(true);
    pages.push({ language: locale.language, page_asset: locale.asset, page_sha256: info.config.metadata.assetHashes[locale.asset], layout, hidden_navigation: styles, content: contentStyles });
    await page.emulateMedia({ media: 'screen' });
  }
  await attach(info, 'print-media.json', { ...identity(info), pages,
    limit: 'Computed Chromium print-media state; not a physical printer or paginated PDF visual acceptance.' });
});
