import { test, expect } from '@playwright/test';
import AxeBuilder from '@axe-core/playwright';
import { writeFile } from 'node:fs/promises';
import { assets, digest, liveMode, publicOrigin, localOrigin, allowedRequest, assetPath, assetRoute } from './source.mjs';
import { requireFocus, requireImages, requireLayout, layoutObservation, focusObservation, pollFocus, requireClippedFocusTimeout, partialFocusCandidate, FocusConditionTimeout } from './checks.mjs';
import { locales, english } from './locales.mjs';
import { matrixColumns, matrixRows, matrixObservation, requireMatrixLayout, requireMatrixMovement, completeMatrixKey } from './matrix.mjs';
import { matrixRecipes, recipeObservation, requireRecipe, requireRecipeIdentity, requireRecipePosition,
  matrixLinkExposure, requireMatrixLinkExposure } from './recipes.mjs';

const live = liveMode(process.env.PAGES_LIVE);
const origin = live ? publicOrigin : localOrigin;
const fontFiles = ['ibm-plex-sans-latin-400-normal.woff2', 'ibm-plex-sans-latin-600-normal.woff2',
  'ibm-plex-mono-latin-400-normal.woff2'];
const codeExamples = [
  ['Analyze command', 'drmr analyze \\ "https://example.com/vod/master.m3u8" \\ --download-dir source --json'],
  ['Plan command', 'drmr plan source/input.m3u8 \\ --to cbcs --format dash \\ --output converted \\ --save conversion.plan.json'],
  ['Convert command', 'drmr convert \\ --plan conversion.plan.json \\ --keys-file keys.json'],
  ['Verify command', 'drmr verify \\ --plan conversion.plan.json \\ --keys-file keys.json --json'],
  ['Build and check the CLI', 'cargo install --path crates/drmr --locked drmr --version drmr doctor --probe --route progressive-mp4'],
];
const monitors = new WeakMap();

async function artifact(info, name, value, type = 'application/json') {
  const path = info.outputPath(name);
  await writeFile(path, type === 'application/json' ? JSON.stringify(value, null, 2) + '\n' : value);
  await info.attach(name, { path, contentType: type });
}

async function settled(page, locale) {
  await expect(page.locator('html')).toHaveAttribute('lang', locale.language);
  await page.evaluate(async () => {
    await document.fonts.ready;
    await Promise.all([...document.fonts].map(font => font.load()));
  });
  await expect(page.getByRole('heading', { level: 1 })).toHaveCount(1);
}

async function ready(page, locale = english, mount = '/drmr/') {
  const response = await page.goto(origin + mount + locale.route);
  expect(response.status()).toBe(200);
  await settled(page, locale);
  const sha256 = digest(await response.body());
  expect(sha256).toBe(test.info().config.metadata.assetHashes[locale.asset]);
  monitors.get(page)?.evidence.pages.push({ language: locale.language, asset: locale.asset, sha256, mount });
}

async function renderedFonts(page, context) {
  // Chromium reports the actual glyph-producing platform fonts, including
  // system fallback for Japanese. This is not a manual readability assessment.
  const session = await context.newCDPSession(page);
  try {
    await session.send('DOM.enable');
    await session.send('CSS.enable');
    const { root } = await session.send('DOM.getDocument', { depth: 0 });
    const result = [];
    for (const selector of ['h1', '.lead', 'pre code']) {
      const { nodeId } = await session.send('DOM.querySelector', { nodeId: root.nodeId, selector });
      expect(nodeId).toBeGreaterThan(0);
      const { fonts } = await session.send('CSS.getPlatformFontsForNode', { nodeId });
      expect(fonts.some(font => font.glyphCount > 0)).toBe(true);
      result.push({ selector, fonts });
    }
    return result;
  } finally {
    await session.detach();
  }
}

function monitor(page, coldFonts) {
  const evidence = { console: [], pageErrors: [], failedRequests: [], responses: [], focusChecks: [], pages: [] };
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
    coldFonts?.bindResponse(response, row);
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
  const pageAsset = assetPath(page.url());
  const check = { phase, ordinal, language: locales.find(locale => locale.asset === pageAsset)?.language,
    page_asset: pageAsset, page_sha256: test.info().config.metadata.assetHashes[pageAsset], samples: [] };
  monitors.get(page).evidence.focusChecks.push(check);
  try {
    await pollFocus(() => target.evaluate(focusObservation), check.samples);
    requireFocus(check.samples.at(-1));
    check.matrixLinkExposure = await target.evaluate(matrixLinkExposure);
    requireMatrixLinkExposure(check.matrixLinkExposure);
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
      mode: info.config.metadata.mode, asset_hashes: info.config.metadata.assetHashes, builder_inputs: info.config.metadata.builderInputHashes,
      checks: monitors.get(page).evidence.focusChecks }, null, 2) + '\n');
  }
}

test.afterEach(async ({ page }, info) => {
  if (monitors.has(page)) {
    const { evidence, pending } = monitors.get(page);
    await Promise.all(pending);
    await artifact(info, 'browser-observations.json', { schema_version: 1,
      source_head: info.config.metadata.sourceHead, source_tree: info.config.metadata.sourceTree,
      asset_hashes: info.config.metadata.assetHashes, builder_inputs: info.config.metadata.builderInputHashes, mode: info.config.metadata.mode,
      test_status: info.status, observations: evidence });
  }
});

const focusSelector = 'a[href], pre[tabindex="0"], .matrix-scroll[tabindex="0"]';
// Fixed inventories include the intentional documentation links in each locale.
const focusTargetCounts = { en: 81, ja: 80 };

function textWithoutNotes(node) {
  const text = value => value.nodeType === Node.TEXT_NODE ? value.textContent :
    value.nodeName === 'SUP' ? '' : [...value.childNodes].map(text).join('');
  return text(node).trim().replace(/\s+/g, ' ');
}

async function renderedMatrix(page, locale) {
  const wrapper = page.getByRole('region', { name: locale.matrixRegion, exact: true });
  await expect(wrapper).toHaveCount(1);
  await expect(wrapper).toHaveAttribute('tabindex', '0');
  await expect(wrapper).toHaveAttribute('aria-describedby', 'matrix-guide matrix-scroll-hint');
  for (const id of ['matrix-guide', 'matrix-scroll-hint', 'matrix-limits', 'matrix-definitions']) {
    await expect(page.locator('#' + id)).toBeVisible();
    expect((await page.locator('#' + id).innerText()).trim()).not.toBe('');
  }
  const table = wrapper.getByRole('table', { name: locale.matrixCaption, exact: true });
  await expect(table).toHaveCount(1);
  await expect(table.locator('caption')).toHaveText(locale.matrixCaption);
  await expect(table.getByRole('columnheader')).toHaveCount(5);
  await expect(table.getByRole('columnheader').nth(0)).toHaveAccessibleName(locale.matrixCorner);
  await expect(table.locator('thead th .axis-input')).toHaveText(locale.matrixCorner.split(' / ')[0]);
  await expect(table.locator('thead th .axis-output')).toHaveText(locale.matrixCorner.split(' / ')[1]);
  for (const [index, name] of matrixColumns.entries()) await expect(table.getByRole('columnheader').nth(index + 1)).toHaveText(name);
  await expect(table.getByRole('rowheader')).toHaveCount(5);
  await expect(table.getByRole('cell')).toHaveCount(20);
  const facts = [];
  for (const [index, expected] of matrixRows.entries()) {
    const row = table.locator('tbody > tr').nth(index);
    await expect(row).toHaveAttribute('id', expected.id);
    const header = row.getByRole('rowheader');
    await expect(header).toHaveAttribute('scope', 'row');
    await expect(header.locator('.input-format')).toHaveText(expected.format);
    expect(await header.locator('.input-profile').evaluate(textWithoutNotes)).toBe(locale.profiles[index]);
    await expect(header.locator('.input-protections code')).toHaveText(expected.input.slice(1));
    const inputs = await header.locator('.input-protections').innerText();
    expect(inputs.replace(/[\s/]/g, '')).toBe(expected.input.join(''));
    const cells = row.getByRole('cell');
    const values = [];
    for (const [column, protections] of expected.output.entries()) {
      const cell = cells.nth(column);
      const text = await cell.evaluate(textWithoutNotes);
      await expect(cell.locator('code')).toHaveText(protections.slice(1));
      const recipes = matrixRecipes.filter(recipe => recipe.row === expected.id && recipe.column === column);
      await expect(cell.locator('a')).toHaveCount(recipes.length);
      for (const [offset, recipe] of recipes.entries()) {
        const link = cell.locator('a').nth(offset);
        await expect(link).toHaveText(recipe.label);
        expect(new URL(await link.getAttribute('href'), page.url()).href)
          .toBe(origin + '/drmr/' + locale.route + 'guide/#' + recipe.id);
      }
      expect(protections.length ? text.replace(/[\s/]/g, '') : text)
        .toBe(protections.length ? protections.join('') : locale.outside);
      values.push(text);
    }
    facts.push({ input: expected.id, protections: inputs, outputs: values });
  }
  for (const header of await table.getByRole('columnheader').all()) await expect(header).toHaveAttribute('scope', 'col');
  const order = await page.locator('main > section').evaluateAll(elements => elements.map(element => ({
    id: element.id, className: element.className, top: element.getBoundingClientRect().top,
    bottom: element.getBoundingClientRect().bottom,
  })));
  expect(order[0].className).toBe('hero');
  expect(order.slice(1, 3).map(section => section.id)).toEqual(['support', 'workflow']);
  expect(order[0].bottom).toBeLessThanOrEqual(order[1].top);
  expect(order[1].bottom).toBeLessThanOrEqual(order[2].top);
  const aria = await table.ariaSnapshot();
  expect(aria).toContain(`table "${locale.matrixCaption}"`);
  for (const name of matrixColumns) expect(aria).toContain(`columnheader "${name}"`);
  const layout = await wrapper.evaluate(matrixObservation);
  requireMatrixLayout(layout);
  return { wrapper, evidence: { facts, aria, order, layout } };
}

async function settledMatrixKey(page, wrapper, key, before, deadline) {
  const direction = key === 'ArrowRight' ? 'right' : 'left';
  const after = await completeMatrixKey({ before, direction, deadline,
    arm: async () => {
      // Arm before keydown; even an instant scrollend must belong to this key.
      const handle = await wrapper.evaluateHandle(target => {
        const signal = { target, ended: false };
        signal.completion = new Promise(resolve => { signal.finish = resolve; });
        signal.listener = event => {
          if (event.target === target) { signal.ended = true; signal.finish(true); }
        };
        target.addEventListener('scrollend', signal.listener);
        return signal;
      });
      return { handle, dispose: async () => {
        try { await handle.evaluate(signal => {
          signal.target.removeEventListener('scrollend', signal.listener);
          signal.finish(false);
        }); }
        finally { await handle.dispose(); }
      } };
    },
    press: () => page.keyboard.press(key),
    wait: async signal => expect(await signal.handle.evaluate(value => value.completion)).toBe(true),
    observe: () => wrapper.evaluate(matrixObservation),
  });
  return { ...after, completedKey: key, scrollEndObserved: true };
}

async function nativeMatrixScroll(page, wrapper, width) {
  // Keep the same static DOM target throughout the timed native traversal.
  const target = await wrapper.elementHandle();
  expect(target).not.toBeNull();
  try {
    const initial = await target.evaluate(matrixObservation);
    requireMatrixLayout(initial);
    expect(initial.focused).toBe(true);
    expect(initial.scrollLeft).toBe(0);
    if (width === 320) expect(initial.scrollWidth).toBeGreaterThan(initial.clientWidth);
    if (initial.scrollWidth <= initial.clientWidth) return { initial, scrollNeeded: false };
    const deadline = performance.now() + 5000;
    const samples = [initial];
    const move = async (key) => {
      samples.push(await settledMatrixKey(page, target, key, samples.at(-1), deadline));
    };
    const first = initial.firstCells;
    const headerRight = Math.max(...initial.rowHeaders.map(header => header.right));
    for (const cell of first) {
      expect(cell.left).toBeGreaterThanOrEqual(headerRight);
      expect(cell.right).toBeLessThanOrEqual(initial.contentRight);
    }
    for (let count = 0; samples.at(-1).scrollLeft < initial.scrollWidth - initial.clientWidth && count < 64; count++) {
      await move('ArrowRight');
    }
    const right = samples.at(-1);
    expect(right.scrollLeft).toBe(initial.scrollWidth - initial.clientWidth);
    for (const cell of right.lastCells) {
      expect(cell.left).toBeGreaterThanOrEqual(Math.max(...right.rowHeaders.map(header => header.right)));
      expect(cell.right).toBeLessThanOrEqual(right.contentRight);
    }
    for (let count = 0; samples.at(-1).scrollLeft > 0 && count < 64; count++) await move('ArrowLeft');
    expect(samples.at(-1).scrollLeft).toBe(0);
    return { scrollNeeded: true, samples };
  } finally {
    await target.dispose();
  }
}

async function nativeMatrixOrigin(page, target) {
  const initial = await target.evaluate(matrixObservation);
  requireMatrixLayout(initial);
  expect(initial.focused).toBe(true);
  const samples = [initial];
  const deadline = performance.now() + 5000;
  for (let count = 0; samples.at(-1).scrollLeft > 0 && count < 64; count++) {
    samples.push(await settledMatrixKey(page, target, 'ArrowLeft', samples.at(-1), deadline));
  }
  expect(samples.at(-1).scrollLeft).toBe(0);
  return { scrollNeeded: initial.scrollLeft > 0, samples };
}

async function nativeMatrixReturn(page, focusable, wrapper) {
  const target = await wrapper.elementHandle();
  expect(target).not.toBeNull();
  const record = { phase: 'complete-keyboard-workflow', reverse: [] };
  monitors.get(page).evidence.matrixReturn = record;
  const info = test.info();
  try {
    record.postForward = await target.evaluate(matrixObservation);
    // The original complete forward traversal ends on the final inventory item.
    // Reverse real Tab events to this same matrix, never refocusing by script.
    const matrixIndex = 10;
    await expect(focusable.nth(matrixIndex)).toHaveAttribute('class', 'matrix-scroll');
    const count = await focusable.count();
    await expect(focusable.nth(count - 1)).toBeFocused();
    if (record.postForward.scrollLeft === 0) {
      record.origin = { scrollNeeded: false, samples: [record.postForward] };
      record.passed = true;
      return record;
    }
    for (let index = count - 2; index >= matrixIndex; index--) {
      await page.keyboard.press('Shift+Tab');
      const item = focusable.nth(index);
      await expect(item).toBeFocused();
      record.reverse.push({ ordinal: index + 1, observed: await item.evaluate(focusObservation) });
    }
    await requireTargetFocus(page, target, 'return-to-matrix', matrixIndex + 1);
    record.origin = await nativeMatrixOrigin(page, target);
    record.passed = true;
    return record;
  } finally {
    record.passed ??= false;
    try {
      await artifact(info, 'matrix-keyboard-return.json', { schema_version: 1,
        source_head: info.config.metadata.sourceHead, source_tree: info.config.metadata.sourceTree,
        mode: info.config.metadata.mode, asset_hashes: info.config.metadata.assetHashes,
        builder_inputs: info.config.metadata.builderInputHashes, record });
    } finally { await target.dispose(); }
  }
}

async function settledRecipe(page, heading, recipe, source, info, phase = 'fonts-ready') {
  const record = { recipe: recipe.id, phase, page_asset: source.asset, page_sha256: source.sha256, samples: [] };
  const evidence = monitors.get(page).evidence;
  evidence.recipeFocusChecks ??= [];
  evidence.recipeFocusChecks.push(record);
  try {
    const observed = await pollFocus(async () => {
      const value = await heading.evaluate(recipeObservation);
      try { requireRecipeIdentity(value, recipe, source); }
      catch (error) { record.terminalObservation = value; throw error; }
      return value;
    }, record.samples, { validate: requireRecipePosition });
    record.passed = true;
    return observed;
  } catch (error) {
    record.failureKind = error instanceof FocusConditionTimeout ? 'condition-timeout' : 'observation-error';
    if (Number.isFinite(error.elapsedMs)) record.failureElapsedMs = error.elapsedMs;
    throw error;
  } finally {
    record.passed ??= false;
    await writeFile(info.outputPath('recipe-focus.json'), JSON.stringify({ schema_version: 1,
      source_head: info.config.metadata.sourceHead, source_tree: info.config.metadata.sourceTree,
      mode: info.config.metadata.mode, asset_hashes: info.config.metadata.assetHashes,
      builder_inputs: info.config.metadata.builderInputHashes, checks: evidence.recipeFocusChecks }, null, 2) + '\n');
  }
}

function coldFontFixture(guideUrl, hashes) {
  const expected = fontFiles.map(file => 'assets/fonts/' + file);
  const record = { guide_url: guideUrl, minimum_hold_ms: 250, deadline_ms: 5000,
    expected_assets: expected, expected_hashes: Object.fromEntries(expected.map(asset => [asset, hashes[asset]])),
    requests: [], stylesheet_samples: [], timed_out: false };
  const heldRequests = new WeakMap();
  let gate, release, timer, armed = false, finished = false;
  let glyphSession, glyphNodeId;
  return { record,
    arm() {
      expect(armed).toBe(false);
      armed = true;
      record.started_ms = performance.now();
      gate = new Promise(resolve => { release = resolve; });
      timer = setTimeout(() => {
        record.timed_out = true;
        finished = true;
        release(false);
      }, record.deadline_ms);
    },
    async handle(route) {
      const request = route.request();
      const asset = assetPath(request.url());
      if (!armed || finished || request.resourceType() !== 'font' || !expected.includes(asset) ||
          request.frame().url().split('#')[0] !== guideUrl) return false;
      const held = { asset, url: request.url(), held_ms: performance.now() };
      record.requests.push(held);
      heldRequests.set(request, held);
      if (!await gate) { held.aborted = true; await route.abort('timedout'); return true; }
      held.continued_ms = performance.now();
      held.hold_duration_ms = held.continued_ms - held.held_ms;
      await route.continue();
      held.continued = true;
      return true;
    },
    bindResponse(response, row) {
      const held = heldRequests.get(response.request());
      if (held) held.response = row;
    },
    async glyphs(page, recipe, source) {
      if (!glyphSession) {
        glyphSession = await page.context().newCDPSession(page);
        await glyphSession.send('DOM.enable');
        await glyphSession.send('CSS.enable');
        const { root } = await glyphSession.send('DOM.getDocument', { depth: 0 });
        const { nodeId } = await glyphSession.send('DOM.querySelector', { nodeId: root.nodeId, selector: '#' + recipe.id });
        expect(nodeId).toBeGreaterThan(0);
        glyphNodeId = nodeId;
      }
      const { node } = await glyphSession.send('DOM.describeNode', { nodeId: glyphNodeId });
      const attributes = Object.fromEntries(Array.from({ length: node.attributes.length / 2 },
        (_, index) => [node.attributes[index * 2], node.attributes[index * 2 + 1]]));
      expect(node.nodeName).toBe('H3');
      expect(node.backendNodeId).toBeGreaterThan(0);
      expect(attributes.id).toBe(recipe.id);
      expect(attributes.tabindex).toBe('-1');
      expect(page.url()).toBe(source.url);
      const target = { id: attributes.id, tag: node.nodeName, node_id: glyphNodeId,
        backend_node_id: node.backendNodeId, url: source.url, page_asset: source.asset, page_sha256: source.sha256 };
      if (record.glyph_target) expect(target).toEqual(record.glyph_target);
      else record.glyph_target = target;
      const { fonts } = await glyphSession.send('CSS.getPlatformFontsForNode', { nodeId: glyphNodeId });
      expect(fonts.some(font => font.glyphCount > 0)).toBe(true);
      return { target, fonts };
    },
    async dispose() {
      if (!glyphSession) return;
      const session = glyphSession;
      glyphSession = undefined;
      await session.detach();
    },
    async minimumHold() {
      const until = Math.max(record.stylesheet_applied_ms,
        ...record.requests.map(request => request.held_ms)) + record.minimum_hold_ms;
      while (performance.now() < until) {
        expect(record.timed_out).toBe(false);
        await new Promise(resolve => setTimeout(resolve, Math.ceil(until - performance.now())));
      }
      expect(record.timed_out).toBe(false);
    },
    finish(continueRequests) {
      if (!armed || finished) return;
      finished = true;
      clearTimeout(timer);
      record.released_ms = performance.now();
      release(continueRequests);
    },
  };
}

async function projectOnly(context, coldFonts) {
  // A page requiring a CDN, tracker or backend must fail, not reach the network.
  await context.route('**/*', async route => {
    if (!allowedRequest(route.request().url(), live)) return route.abort('blockedbyclient');
    if (coldFonts && await coldFonts.handle(route)) return;
    await route.continue();
  });
}

async function recipeNavigation(page, locale, info, batch, offset, coldFonts) {
  // The same static anchors must work when production JavaScript is disabled.
  // Only the first destination uses Tab/Enter; every remaining supported token
  // is activated as an actual link, rather than navigating to an invented URL.
  await page.goto('about:blank');
  await ready(page, locale);
  const links = page.locator('.conversion-matrix tbody td a[href]');
  await expect(links).toHaveCount(matrixRecipes.length);
  const guideAsset = locale.language === 'en' ? 'guide/index.html' : 'ja/guide/index.html';
  const guideRoute = origin + '/drmr/' + locale.route + 'guide/';
  const destinations = [];
  for (const [index, recipe] of batch.entries()) {
    const link = links.nth(offset + index);
    await expect(link).toHaveText(recipe.label);
    expect(new URL(await link.getAttribute('href'), page.url()).href).toBe(guideRoute + '#' + recipe.id);
    if (index === 0) {
      const inventory = page.locator(focusSelector);
      await expect(inventory).toHaveCount(focusTargetCounts[locale.language]);
      const ordinal = 12 + offset;
      for (let count = 0; count < ordinal; count++) await page.keyboard.press('Tab');
      await expect(link).toBeFocused();
      await requireTargetFocus(page, link, 'native-recipe-link', ordinal);
      coldFonts?.arm();
    }
    const [response] = await Promise.all([
      page.waitForResponse(value => value.request().isNavigationRequest() && value.url() === guideRoute),
      index === 0 ? page.keyboard.press('Enter') : link.click(),
    ]);
    expect(response.status()).toBe(200);
    const sha256 = digest(await response.body());
    expect(sha256).toBe(info.config.metadata.assetHashes[guideAsset]);
    const url = guideRoute + '#' + recipe.id;
    await expect(page).toHaveURL(url);
    await expect(page.locator('html')).toHaveAttribute('lang', locale.language);
    const heading = page.locator('#' + recipe.id);
    await expect(heading).toHaveCount(1);
    const source = { url, language: locale.language, asset: guideAsset, sha256 };
    if (coldFonts && index === 0) {
      const remaining = () => {
        const value = coldFonts.record.started_ms + coldFonts.record.deadline_ms - performance.now();
        expect(value).toBeGreaterThan(0);
        return value;
      };
      await expect.poll(() => coldFonts.record.requests.length, { timeout: remaining() }).toBe(3);
      const stylesheets = ['assets/site.css', 'assets/guide.css'].map(asset => origin + '/drmr/' + asset);
      await expect.poll(async () => {
        const styles = await page.locator('link[rel="stylesheet"]').evaluateAll(nodes => nodes.map(node => ({
          href: node.href, applied: Boolean(node.sheet && !node.sheet.disabled),
          rules: node.sheet?.cssRules.length ?? 0,
        })));
        coldFonts.record.stylesheet_samples.push(styles);
        return styles.length === 2 && styles.every((style, ordinal) =>
          style.href === stylesheets[ordinal] && style.applied && style.rules > 0);
      }, { timeout: remaining() }).toBe(true);
      coldFonts.record.stylesheet_applied_ms = performance.now();
      await coldFonts.minimumHold();
      coldFonts.record.beforeGlyphs = await coldFonts.glyphs(page, recipe, source);
      expect(coldFonts.record.beforeGlyphs.fonts.filter(font => font.glyphCount > 0)
        .every(font => font.isCustomFont === false)).toBe(true);
      coldFonts.record.beforeFonts = await settledRecipe(page, heading, recipe, source, info, 'fonts-pending');
      expect(coldFonts.record.timed_out).toBe(false);
      expect(performance.now() - coldFonts.record.started_ms).toBeLessThan(5000);
      expect(coldFonts.record.requests.every(request => request.continued_ms === undefined)).toBe(true);
      coldFonts.finish(true);
    }
    // Native Enter does not await navigation. Bind this document before asking
    // for its load/font state, so an already-loaded landing page cannot satisfy it.
    await page.waitForLoadState('load');
    await page.evaluate(async () => { await document.fonts.ready; });
    await expect(page.locator('script')).toHaveCount(0);
    if (coldFonts && index === 0) {
      expect(coldFonts.record.timed_out).toBe(false);
      expect(coldFonts.record.requests.map(request => request.asset).sort())
        .toEqual([...coldFonts.record.expected_assets].sort());
      for (const request of coldFonts.record.requests) {
        expect(request.continued).toBe(true);
        expect(request.hold_duration_ms).toBeGreaterThanOrEqual(250);
        expect(request.continued_ms - coldFonts.record.started_ms).toBeLessThanOrEqual(5000);
      }
      await expect.poll(() => coldFonts.record.requests.every(request => request.response?.status === 200 &&
        request.response.sha256 === coldFonts.record.expected_hashes[request.asset]), { timeout: 5000 }).toBe(true);
      coldFonts.record.fonts_completed_ms = performance.now();
      coldFonts.record.afterGlyphs = await coldFonts.glyphs(page, recipe, source);
      const glyphEvidence = capture => capture.fonts.filter(font => font.glyphCount > 0).map(font => ({
        family: font.familyName, custom: font.isCustomFont, glyphs: font.glyphCount,
      })).sort((left, right) => left.family.localeCompare(right.family));
      expect(glyphEvidence(coldFonts.record.afterGlyphs)).toEqual(glyphEvidence(coldFonts.record.beforeGlyphs));
      await coldFonts.dispose();
    }
    const observed = await settledRecipe(page, heading, recipe, source, info);
    if (coldFonts && index === 0) {
      coldFonts.record.afterFonts = observed;
      expect(observed.fontsStatus).toBe('loaded');
      expect({ rect: observed.rect, viewport: observed.viewport }).toEqual({
        rect: coldFonts.record.beforeFonts.rect, viewport: coldFonts.record.beforeFonts.viewport,
      });
      coldFonts.record.geometry_unchanged = true;
      coldFonts.record.protocol_passed = true;
    }
    await expect(heading).toBeFocused();
    requireRecipe(observed, recipe);
    expect(observed.rect.top).toBeGreaterThanOrEqual(0);
    expect(observed.rect.bottom).toBeLessThanOrEqual(observed.viewport.height);
    expect(observed.rect.left).toBeGreaterThanOrEqual(0);
    expect(observed.rect.right).toBeLessThanOrEqual(observed.viewport.width);
    const row = { recipe, navigation: index === 0 ? 'native-tab-enter' : 'link-click',
      page_asset: guideAsset, page_sha256: sha256, observed };
    destinations.push(row);
    if (index === 0) {
      const path = info.outputPath('recipe-viewport-' + locale.language + '-320.png');
      await page.screenshot({ path, fullPage: false, animations: 'disabled' });
      const after = await heading.evaluate(recipeObservation);
      expect(after).toEqual(observed);
      await info.attach('native-recipe-viewport', { path, contentType: 'image/png' });
      row.visual_checkpoint = { phase: 'native-recipe-fragment-before-any-guide-interaction',
        viewport: observed.viewport, before: observed, after, screenshot: path.split('/').at(-1) };
    }
    // Retain completed destinations before the next navigation can fail.
    await writeFile(info.outputPath('recipe-navigation.json'), JSON.stringify({ schema_version: 1,
      source_head: info.config.metadata.sourceHead, source_tree: info.config.metadata.sourceTree,
      asset_hashes: info.config.metadata.assetHashes, builder_inputs: info.config.metadata.builderInputHashes,
      mode: info.config.metadata.mode, language: locale.language, java_script_enabled: false,
      matrix_destinations: matrixRecipes.length, batch_offset: offset,
      expected_destinations: batch.length, completed_destinations: destinations.length,
      destinations }, null, 2) + '\n');
    await page.goBack({ waitUntil: 'domcontentloaded' });
    await expect(links).toHaveCount(matrixRecipes.length);
  }
  expect(destinations.map(row => row.recipe.id)).toEqual(batch.map(recipe => recipe.id));
  return destinations;
}

for (const locale of locales) for (const width of [320, 768, 1024, 1440]) {
  test(`${locale.language} / ${width}px: layout, assets, keyboard and rendered accessibility`, async ({ page, context, browser }, info) => {
    await projectOnly(context);
    const network = monitor(page);
    await page.setViewportSize({ width, height: 900 });
    await ready(page, locale);
    // Check production HTML before axe injects its test-only engine into the DOM.
    expect(await page.locator('script').count()).toBe(0);
    const initialViewport = await page.evaluate(() => ({
      phase: 'after-navigation-and-fonts-before-any-keyboard-focus-or-scroll',
      scrollX, scrollY, focusedTag: document.activeElement.tagName,
      heading: (() => { const box = document.querySelector('#support-title').getBoundingClientRect();
        return { top: box.top, bottom: box.bottom, left: box.left, right: box.right }; })(),
    }));
    expect(initialViewport.scrollX).toBe(0);
    expect(initialViewport.scrollY).toBe(0);
    const initialScreenshot = info.outputPath(`initial-${locale.language}-${width}.png`);
    await page.screenshot({ path: initialScreenshot, fullPage: false, animations: 'disabled' });
    await info.attach('initial-viewport-before-interaction', { path: initialScreenshot, contentType: 'image/png' });
    const layout = await page.evaluate(layoutObservation);
    const screenshot = info.outputPath(`page-${locale.language}-${width}.png`);
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
    for (const name of [locale.navigation, locale.footer, locale.languageNavigation]) await expect(page.getByRole('navigation', { name, exact: true })).toHaveCount(1);
    for (const name of locale.headings) {
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
    const matrix = await renderedMatrix(page, locale);
    const tree = await page.locator('body').ariaSnapshot();
    for (const name of [locale.navigation, locale.footer, locale.languageNavigation]) expect(tree).toContain(`navigation "${name}"`);
    expect(tree).toContain('main:');
    expect(tree).toContain(`link "${locale.matrix}"`);
    await expect(page.getByRole('group')).toHaveCount(5);
    const renderedCodeGroups = [];
    for (const [index, [, command]] of codeExamples.entries()) {
      const name = locale.groups[index];
      const group = page.getByRole('group', { name, exact: true });
      await expect(group).toHaveCount(1);
      await expect(group).toBeVisible();
      await expect(group).toHaveJSProperty('tagName', 'PRE');
      await expect(group).toHaveAttribute('tabindex', '0');
      const code = (await group.locator('code').innerText()).trim().replace(/\s+/g, ' ');
      expect(code).toBe(command);
      const aria = await group.ariaSnapshot();
      expect(aria).toContain(`group "${name}"`);
      expect(aria).toContain(`code: ${command}`);
      renderedCodeGroups.push({ name, code, aria });
    }
    await artifact(info, 'rendered-aria.yml', tree, 'text/yaml');

    // Native Tab/Enter, not script-triggered click, proves skip-link movement.
    await page.keyboard.press('Tab');
    await expect(page.getByRole('link', { name: locale.skip, exact: true })).toBeFocused();
    await requireTargetFocus(page, page.getByRole('link', { name: locale.skip, exact: true }), 'skip-link', 1);
    await page.keyboard.press('Enter');
    await expect(page.getByRole('main')).toBeFocused();
    await expect(page).toHaveURL(origin + '/drmr/' + locale.route + '#main');

    await page.goto('about:blank');
    await ready(page, locale);
    const focusable = page.locator(focusSelector);
    await expect(focusable).toHaveCount(focusTargetCounts[locale.language]);
    const focusRecords = [];
    let matrixKeyboard;
    for (let i = 0; i < await focusable.count(); i++) {
      await page.keyboard.press('Tab');
      await expect(focusable.nth(i)).toBeFocused();
      const observed = await requireTargetFocus(page, focusable.nth(i), 'tab-order', i + 1);
      if (await focusable.nth(i).getAttribute('class') === 'matrix-scroll') {
        matrixKeyboard = await nativeMatrixScroll(page, focusable.nth(i), width);
      }
      focusRecords.push({ ordinal: i + 1, language: locale.language, page_asset: locale.asset,
        page_sha256: info.config.metadata.assetHashes[locale.asset], ...observed });
    }
    expect(matrixKeyboard).toBeDefined();
    const matrixReturn = await nativeMatrixReturn(page, focusable, matrix.wrapper);
    for (const [name, id] of locale.anchors.map((name, index) => [name, ['support', 'workflow', 'get-started'][index]])) {
      const link = page.getByRole('navigation', { name: locale.navigation, exact: true }).getByRole('link', { name, exact: true });
      await link.focus();
      await page.keyboard.press('Enter');
      await expect(page).toHaveURL(origin + '/drmr/' + locale.route + '#' + id);
      const box = await page.locator('#' + id).boundingBox();
      expect(box.y).toBeGreaterThanOrEqual(0);
      expect(box.y).toBeLessThan(900);
    }
    // Every served asset must match this checkout byte for byte, including notices.
    const servedAssets = [];
    for (const path of assets) {
      const response = await page.request.get(origin + '/drmr/' + assetRoute(path),
        { maxRedirects: 0, headers: { 'Cache-Control': 'no-cache' } });
      expect(response.status()).toBe(200);
      const sha256 = digest(await response.body());
      expect(sha256).toBe(info.config.metadata.assetHashes[path]);
      servedAssets.push({ path, sha256, headers: response.headers() });
    }
    const license = page.getByRole('link', { name: locale.license, exact: true });
    expect(new URL(await license.getAttribute('href'), page.url()).href)
      .toBe(origin + '/drmr/assets/fonts/LICENSE-ibm-plex.txt');
    const platformFonts = await renderedFonts(page, context);
    // Audit the state reached by completing the keyboard workflow, with every
    // column returned to the origin through native keys rather than a DOM reset.
    matrixReturn.beforeAxe = await matrix.wrapper.evaluate(matrixObservation);
    expect(matrixReturn.beforeAxe.scrollLeft).toBe(0);
    requireMatrixLayout(matrixReturn.beforeAxe);
    const accessibility = await new AxeBuilder({ page }).analyze();
    await artifact(info, 'axe.json', accessibility);
    expect(accessibility.violations).toEqual([]);
    expect(accessibility.incomplete).toEqual([]);
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
      language: locale.language, page_asset: locale.asset, page_sha256: info.config.metadata.assetHashes[locale.asset],
      asset_hashes: info.config.metadata.assetHashes, builder_inputs: info.config.metadata.builderInputHashes, servedAssets, chromium: browser.version(), viewport: { width, height: 900 }, production_scripts: 0,
      initial_viewport: initialViewport, matrix: matrix.evidence, matrix_keyboard: matrixKeyboard,
      matrix_return: matrixReturn, layout, fonts, platform_fonts: platformFonts, images, headings, code_groups: renderedCodeGroups, keyboard: focusRecords, network,
      axe: { version: accessibility.testEngine.version, violations: 0, incomplete: accessibility.incomplete.length },
      limitations: ['automated Chromium only; no manual screen-reader or other-browser qualification',
        'external repository links checked by source validator; no external browser navigation'] });
  });
}

for (const locale of locales) test(`${locale.language}: core content and anchor navigation work without production JavaScript`, async ({ browser }, info) => {
  const context = await browser.newContext({ javaScriptEnabled: false, viewport: { width: 320, height: 900 } });
  try {
    await projectOnly(context);
    const page = await context.newPage();
    const network = monitor(page);
    const mounts = live ? ['/drmr/'] : ['/', '/drmr/'];
    const pages = [];
    for (const mount of mounts) {
      await page.goto('about:blank');
      const response = await page.goto(origin + mount + locale.route);
      expect(response.status()).toBe(200);
      const sha256 = digest(await response.body());
      expect(sha256).toBe(info.config.metadata.assetHashes[locale.asset]);
      pages.push({ language: locale.language, asset: locale.asset, sha256, mount });
      await expect(page.locator('html')).toHaveAttribute('lang', locale.language);
      await expect(page.locator('script')).toHaveCount(0);
      await expect(page.getByRole('main')).toBeVisible();
      await expect(page.getByRole('heading', { level: 1 })).toHaveCount(1);
      await expect(page.getByRole('table', { name: locale.matrixCaption, exact: true })).toHaveCount(1);
      await expect(page.getByRole('table', { name: locale.matrixCaption, exact: true }).getByRole('cell')).toHaveCount(20);
      const link = page.getByRole('navigation', { name: locale.navigation, exact: true }).getByRole('link', { name: locale.anchors[0], exact: true });
      for (let i = 0; i < 3; i++) await page.keyboard.press('Tab');
      await expect(link).toBeFocused();
      await page.keyboard.press('Enter');
      await expect(page).toHaveURL(origin + mount + locale.route + '#support');
      await expect(page.locator('#support')).toBeInViewport();
    }
    await Promise.all(monitors.get(page).pending);
    expect(network.console).toEqual([]);
    expect(network.pageErrors).toEqual([]);
    expect(network.failedRequests).toEqual([]);
    expect(network.responses.every(row => allowedRequest(row.url, live) && row.status === 200 &&
      !row.body_error && (!row.asset || row.sha256 === info.config.metadata.assetHashes[row.asset]))).toBe(true);
    await artifact(info, 'evidence.json', { schema_version: 1, source_head: info.config.metadata.sourceHead,
      source_tree: info.config.metadata.sourceTree, asset_hashes: info.config.metadata.assetHashes, builder_inputs: info.config.metadata.builderInputHashes,
      language: locale.language, page_asset: locale.asset, page_sha256: info.config.metadata.assetHashes[locale.asset],
      java_script_enabled: false, mode: info.config.metadata.mode, pages, network, browser: browser.version() });
  } finally {
    await context.close();
  }
});

for (const locale of locales) for (const offset of [0, 15, 30]) {
  test(`${locale.language}: matrix recipes ${offset + 1}–${offset + 15} navigate without JavaScript`, async ({ browser }, info) => {
    const context = await browser.newContext({ javaScriptEnabled: false, viewport: { width: 320, height: 900 } });
    const coldFonts = locale.language === 'en' && offset === 0
      ? coldFontFixture(origin + '/drmr/guide/', info.config.metadata.assetHashes) : undefined;
    try {
      await projectOnly(context, coldFonts);
      const page = await context.newPage();
      const network = monitor(page, coldFonts);
      if (coldFonts) network.coldFonts = coldFonts.record;
      const batch = matrixRecipes.slice(offset, offset + 15);
      expect(batch).toHaveLength(15);
      const recipes = await recipeNavigation(page, locale, info, batch, offset, coldFonts);
      await Promise.all(monitors.get(page).pending);
      expect(network.console).toEqual([]);
      expect(network.pageErrors).toEqual([]);
      expect(network.failedRequests).toEqual([]);
      expect(network.responses.every(row => allowedRequest(row.url, live) && row.status === 200 &&
        !row.body_error && (!row.asset || row.sha256 === info.config.metadata.assetHashes[row.asset]))).toBe(true);
      await artifact(info, 'evidence.json', { schema_version: 1, source_head: info.config.metadata.sourceHead,
        source_tree: info.config.metadata.sourceTree, asset_hashes: info.config.metadata.assetHashes,
        builder_inputs: info.config.metadata.builderInputHashes, language: locale.language,
        java_script_enabled: false, mode: info.config.metadata.mode, matrix_destinations: matrixRecipes.length,
        batch_offset: offset, expected_destinations: batch.length, completed_destinations: recipes.length,
        recipes, network, browser: browser.version() });
    } finally {
      coldFonts?.finish(false);
      try {
        if (coldFonts) await artifact(info, 'cold-font-protocol.json', { schema_version: 1,
          source_head: info.config.metadata.sourceHead, source_tree: info.config.metadata.sourceTree,
          mode: info.config.metadata.mode, asset_hashes: info.config.metadata.assetHashes,
          builder_inputs: info.config.metadata.builderInputHashes, record: coldFonts.record });
      } finally {
        try { await coldFonts?.dispose(); }
        finally { await context.close(); }
      }
    }
  });
}

test('native keyboard switches English to Japanese and back at each supported mount', async ({ page, context }, info) => {
  await projectOnly(context);
  const network = monitor(page);
  const hops = [];
  const mounts = live ? ['/drmr/'] : ['/', '/drmr/'];
  await page.setViewportSize({ width: 320, height: 900 });
  for (const mount of mounts) {
    await page.goto('about:blank');
    await ready(page, english, mount);
    for (const [from, to] of [[locales[0], locales[1]], [locales[1], locales[0]]]) {
      const link = page.getByRole('navigation', { name: from.languageNavigation, exact: true })
        .getByRole('link', { name: from.switchName, exact: true });
      await expect(link).toHaveAttribute('href', from.switchHref);
      await expect(link).toHaveAttribute('hreflang', to.language);
      const focusable = page.locator(focusSelector);
      await expect(focusable).toHaveCount(focusTargetCounts[from.language]);
      for (let index = 0; index < 8; index++) {
        await page.keyboard.press('Tab');
        await expect(focusable.nth(index)).toBeFocused();
      }
      await expect(link).toBeFocused();
      await requireTargetFocus(page, link, 'language-switch', 8);
      const target = origin + mount + to.route;
      const [response] = await Promise.all([
        page.waitForResponse(response => response.url() === target && response.request().isNavigationRequest()),
        page.keyboard.press('Enter'),
      ]);
      await expect(page).toHaveURL(target);
      expect(response.status()).toBe(200);
      const sha256 = digest(await response.body());
      expect(sha256).toBe(info.config.metadata.assetHashes[to.asset]);
      await settled(page, to);
      await expect(page.locator('script')).toHaveCount(0);
      hops.push({ mount, from: from.language, to: to.language, asset: to.asset, sha256, url: response.url() });
    }
  }
  expect(await context.cookies()).toEqual([]);
  expect(network.console).toEqual([]);
  expect(network.pageErrors).toEqual([]);
  expect(network.failedRequests).toEqual([]);
  await artifact(info, 'language-roundtrip.json', { schema_version: 1,
    source_head: info.config.metadata.sourceHead, source_tree: info.config.metadata.sourceTree,
    asset_hashes: info.config.metadata.assetHashes, builder_inputs: info.config.metadata.builderInputHashes, mode: info.config.metadata.mode, hops });
});

test('browser guard controls detect real overflow, invisible focus, blocked matrix scrolling and a missing image', async ({ page, context }, info) => {
  await projectOnly(context);
  monitor(page);
  await page.setViewportSize({ width: 320, height: 900 });
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
  // Reach the matrix with the same native Tab route used by a visitor.
  await page.goto('about:blank');
  await ready(page);
  const wrapper = page.getByRole('region', { name: english.matrixRegion, exact: true });
  const matrixTabOrdinal = 11;
  for (let index = 0; index < matrixTabOrdinal; index++) await page.keyboard.press('Tab');
  await expect(wrapper).toBeFocused();
  await requireTargetFocus(page, wrapper, 'matrix-disabled-control', matrixTabOrdinal);
  const matrixBefore = await wrapper.evaluate(matrixObservation);
  expect(matrixBefore.scrollWidth).toBeGreaterThan(matrixBefore.clientWidth);
  const hiddenContent = await page.addStyleTag({ content: '.conversion-matrix td a > .clear { visibility: hidden !important; }' });
  const matrixHidden = await wrapper.evaluate(matrixObservation);
  expect(matrixHidden.hiddenText.some(value => value.text === 'Clear' && value.hidden)).toBe(true);
  expect(() => requireMatrixLayout(matrixHidden)).toThrow('Rendered matrix text must remain visible');
  await hiddenContent.evaluate(element => element.remove());
  const matrixVisible = await wrapper.evaluate(matrixObservation);
  requireMatrixLayout(matrixVisible);
  const clipping = await page.addStyleTag({ content: '.matrix-scroll { overflow: clip !important; }' });
  await page.keyboard.press('ArrowRight');
  const matrixDisabled = await wrapper.evaluate(matrixObservation);
  expect(matrixDisabled.focused).toBe(true);
  expect(matrixDisabled.scrollLeft).toBe(matrixBefore.scrollLeft);
  expect(() => requireMatrixMovement(matrixBefore, matrixDisabled, 'right'))
    .toThrow('Matrix needs its own native horizontal scroll container');
  await clipping.evaluate(element => element.remove());
  requireMatrixLayout(await wrapper.evaluate(matrixObservation));
  const restoreDeadline = performance.now() + 5000;
  const matrixRestored = await settledMatrixKey(page, wrapper, 'ArrowRight', matrixBefore, restoreDeadline);
  const matrixReturned = await settledMatrixKey(page, wrapper, 'ArrowLeft', matrixRestored, restoreDeadline);
  expect(matrixReturned.scrollLeft).toBe(0);
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
    matrix: { hiddenContent: matrixHidden, visibleAgain: matrixVisible, before: matrixBefore, disabled: matrixDisabled, restored: matrixRestored, returned: matrixReturned },
    missing_image_status: 404, badImages });
});

test('native Tab keeps edge targets fully visible only with the production scroll margin', async ({ page, context }, info) => {
  await projectOnly(context);
  monitor(page);
  const comparisons = [];
  const discoveries = [];
  try {
    for (const width of [320, 1440]) {
      // Discover a naturally clipped edge, never manufacture one by scrolling
      // or refocusing. The full five-second replay below must confirm it.
      await page.setViewportSize({ width, height: 900 });
      await page.goto('about:blank');
      await ready(page);
      await page.addStyleTag({ content: `${focusSelector} { scroll-margin-block: 0 !important; }` });
      const route = [];
      const discovery = { width, viewportHeight: 900, route };
      discoveries.push(discovery);
      const candidates = page.locator(focusSelector);
      await expect(candidates).toHaveCount(focusTargetCounts[english.language]);
      let selected;
      for (let index = 0; index < focusTargetCounts[english.language]; index++) {
        await page.keyboard.press('Tab');
        await expect(candidates.nth(index)).toBeFocused();
        const sample = await candidates.nth(index).evaluate(focusObservation);
        const margin = await candidates.nth(index).evaluate(element => getComputedStyle(element).scrollMarginBlockStart);
        expect(margin).toBe('0px');
        route.push({ ordinal: index + 1, margin, sample });
        if (partialFocusCandidate(sample)) {
          selected = { ordinal: index + 1, identity: sample.target };
          discovery.selected = selected;
          break;
        }
      }
      expect(selected, 'A native Tab target must naturally cross only the bottom edge with margin disabled').toBeDefined();
      const { ordinal, identity } = selected;
      // Reproduce the same ordinal and complete identity in all three states.
      for (const state of ['production', 'disabled-control', 'restored']) {
        await page.setViewportSize({ width, height: 900 });
        await page.goto('about:blank');
        await ready(page);
        if (state === 'disabled-control') {
          await page.addStyleTag({ content: `${focusSelector} { scroll-margin-block: 0 !important; }` });
        }
        const focusable = page.locator(focusSelector);
        await expect(focusable).toHaveCount(focusTargetCounts[english.language]);
        const expected = focusable.nth(ordinal - 1);
        expect(await expected.evaluate(element => getComputedStyle(element).scrollMarginBlockStart))
          .toBe(state === 'disabled-control' ? '0px' : '16px');
        for (let index = 0; index < ordinal; index++) {
          await page.keyboard.press('Tab');
          await expect(focusable.nth(index)).toBeFocused();
        }
        await expect(expected).toBeFocused();
        expect((await expected.evaluate(focusObservation)).target).toEqual(identity);
        const phase = `scroll-margin-${width}-${state}`;
        let failure;
        try {
          await requireTargetFocus(page, expected, phase, ordinal);
        } catch (error) {
          failure = error;
        }
        const check = monitors.get(page).evidence.focusChecks.at(-1);
        comparisons.push({ width, state, ...check });
        for (const sample of check.samples) expect(sample.target).toEqual(identity);
        if (state === 'disabled-control') {
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
      mode: info.config.metadata.mode, discoveries, comparisons });
  }
});
