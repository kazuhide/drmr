import { guideChapters, requireGuideLayout, requireGuideMovement, codeAriaTemplate, requireGuideCheckpoint,
  guideFiguresObservation, requireGuideFigures, guideScrollObservation, requireGuideCodeEdge, requireGuidePrintCode,
  guideSourceExampleCount } from './guide.mjs';
import { locales } from './locales.mjs';
import { matrixRows, matrixColumns, matrixObservation, requireMatrixLayout, requireMatrixMovement, completeMatrixKey, MatrixKeyDeadline } from './matrix.mjs';
import { matrixRecipes, matrixRecipeExampleCount, matrixRecipeBatches, landingFocusTargetCounts, recipeObservation, requireRecipe,
  requireRecipeIdentity, requireRecipePosition, requireMatrixLinkExposure } from './recipes.mjs';
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { runInNewContext } from 'node:vm';
import { readFileSync, readdirSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { assets, assetRoute, assetPath, digest, liveMode, allowedRequest } from './source.mjs';
import * as identity from './source.mjs';
import { matchesPublication } from './wait-published.mjs';
import { resource } from './server.mjs';
import { requireFocus, requireImages, requireLayout, layoutObservation, focusObservation, requireClippedFocusTimeout, partialFocusCandidate, pollFocus, FocusConditionTimeout, FocusObservationDeadline } from './checks.mjs';

test('only landing cases use two parallel workers and every suite deadline remains fixed', () => {
  // Evaluate configuration objects only: no Playwright runtime, browser or CI launch.
  const evaluate = (file, landing) => {
    const fixture = { sourceHead: 'config-fixture', sourceTree: 'config-tree' };
    const source = readFileSync(new URL('./' + file, import.meta.url), 'utf8')
      .replace(/^import[^\n]*\n/gm, '').replaceAll('import.meta.url', '"file:///config-fixture.mjs"')
      .replace('export default defineConfig(', 'result = defineConfig(');
    let ciCalls = 0;
    const context = { defineConfig: value => value, landing, result: null,
      source: () => fixture, liveMode, publicOrigin: identity.publicOrigin, localOrigin: identity.localOrigin,
      requireCi: value => { assert.equal(value, false); ciCalls++; },
      process: { env: { GITHUB_SHA: fixture.sourceHead, PW_REPORT_ROOT: '/ci-evidence' }, version: 'node-config-fixture' },
      createRequire: () => ({ resolve: () => '/fixture/playwright-core/package.json' }),
      readFileSync: () => JSON.stringify({ packages: {}, browsers: [] }), dirname, join, resolve, URL,
      locales, matrixRecipeBatches };
    runInNewContext(source, context, { timeout: 1000 });
    assert.equal(ciCalls, landing ? 0 : 1);
    return JSON.parse(JSON.stringify(context.result));
  };
  const landing = evaluate('playwright.config.mjs');
  const guide = evaluate('guide.config.mjs', landing);
  assert.equal(landing.fullyParallel, true);
  assert.equal(landing.workers, 2);
  assert.equal(guide.fullyParallel, false);
  assert.equal(guide.workers, 1);
  for (const config of [landing, guide]) {
    assert.equal(config.timeout, 30_000);
    assert.equal(config.globalTimeout, 180_000);
    assert.equal(config.expect.timeout, 5000);
    assert.equal(config.use.actionTimeout, 5000);
    assert.equal(config.use.navigationTimeout, 10_000);
    assert.equal(config.retries, 0);
  }
  assert.equal(landing.metadata.browserCases, 23);
  assert.equal(guide.metadata.browserCases, 12);
});

test('all independent landing cases retain unique evidence namespaces including both final recipe batches', () => {
  const names = [];
  const register = (name, callback) => { names.push(name); assert.equal(typeof callback, 'function'); };
  register.afterEach = callback => assert.equal(typeof callback, 'function');
  const source = readFileSync(new URL('./browser.spec.mjs', import.meta.url), 'utf8')
    .replace(/^import[\s\S]*?;\n/gm, '');
  runInNewContext(source, { test: register, process: { env: {} }, liveMode,
    publicOrigin: identity.publicOrigin, localOrigin: identity.localOrigin,
    locales, english: locales[0], matrixRecipeBatches }, { timeout: 1000 });
  assert.equal(names.length, 23);
  assert.equal(new Set(names).size, names.length);
  for (const language of ['en', 'ja']) {
    assert.ok(names.includes(`${language}: matrix recipes 61–65 navigate without JavaScript`));
    assert.equal(names.filter(name => name.startsWith(language + ': matrix recipes ')).length, 5);
  }
});

test('only the fixed static payload resolves at either mount', () => {
  assert.equal(resource('/').relative, 'index.html');
  assert.equal(resource('/drmr/').relative, 'index.html');
  for (const path of ['/ja/', '/ja/index.html', '/drmr/ja/', '/drmr/ja/index.html']) {
    assert.equal(resource(path).relative, 'ja/index.html');
    assert.equal(assetPath('http://127.0.0.1:4173' + path), 'ja/index.html');
  }
  assert.equal(assetRoute('ja/index.html'), 'ja/');
  assert.equal(assets.length, 12);
  assert.throws(() => assetRoute('../keys.json'));
  assert.equal(resource('/drmr/ja/keys.json'), null);
  assert.equal(resource('/drmr/ja/%2e%2e%2f%2e%2e%2fkeys.json'), null);
  assert.equal(resource('/drmr/assets/site.css').type, 'text/css; charset=utf-8');
  for (const path of ['/drmr/../../README.md', '/drmr/%2e%2e%2fREADME.md', '/drmr/assets%5csite.css',
    '/.git/config', '/drmr/assets/site.css?secret=1', '/drmr/assets/', '/drmr/%zz']) {
    assert.equal(resource(path), null, path);
  }
});

test('layout controls fail for overflow or clipped content', () => {
  requireLayout({ clientWidth: 320, scrollWidth: 320, clipped: [] });
  assert.throws(() => requireLayout({ clientWidth: 320, scrollWidth: 321, clipped: [] }));
  assert.throws(() => requireLayout({ clientWidth: 320, scrollWidth: 320, clipped: ['H1'] }));
});

test('focus controls refuse hidden, offscreen or unoutlined focus', () => {
  const visible = { matchesTarget: true, width: 100, height: 32, inViewport: true, outlineWidth: 3, outlineStyle: 'solid' };
  requireFocus(visible);
  for (const changed of [{ matchesTarget: false }, { width: 0 }, { inViewport: false }, { outlineWidth: 0 }, { outlineStyle: 'none' }]) {
    assert.throws(() => requireFocus({ ...visible, ...changed }));
  }
});

test('image controls refuse missing or unfinished images', () => {
  requireImages([{ complete: true, width: 128 }]);
  for (const images of [[], [{ complete: false, width: 128 }], [{ complete: true, width: 0 }]]) {
    assert.throws(() => requireImages(images));
  }
});


test('published checks have one fixed project origin and refuse arbitrary modes or URLs', () => {
  assert.equal(liveMode(undefined), false);
  assert.equal(liveMode('true'), true);
  for (const value of ['false', 'https://example.com', '']) assert.throws(() => liveMode(value));
  assert.equal(allowedRequest('https://kazuhide.github.io/drmr/', true), true);
  for (const url of ['https://example.com/drmr/', 'https://kazuhide.github.io/',
    'https://kazuhide.github.io/drmr-other/', 'http://kazuhide.github.io/drmr/',
    'https://kazuhide.github.io/drmr/?secret=1', 'https://user@kazuhide.github.io/drmr/']) {
    assert.equal(allowedRequest(url, true), false, url);
  }
});

test('all twelve published raw hashes must match once; stale, missing or duplicate rows fail', () => {
  const hashes = Object.fromEntries(assets.map(path => [path, digest(Buffer.from(path))]));
  const rows = assets.map(path => ({ path, status: 200, sha256: hashes[path] }));
  assert.equal(matchesPublication(rows, hashes), true);
  assert.equal(matchesPublication(rows.slice(1), hashes), false);
  assert.equal(matchesPublication([...rows, rows[0]], hashes), false);
  assert.equal(matchesPublication(rows.map((row, i) => i ? row : { ...row, status: 404 }), hashes), false);
  assert.equal(matchesPublication(rows.map((row, i) => i ? row : { ...row, sha256: digest(Buffer.from('stale')) }), hashes), false);
  assert.notEqual(digest(Buffer.from('line\n')), digest(Buffer.from('line\r\n')));
  const staleJapanese = rows.map(row => row.path === 'ja/index.html' ? { ...row, sha256: digest(Buffer.from('stale-ja')) } : row);
  assert.equal(matchesPublication(staleJapanese, hashes), false);
  for (const asset of ['guide/index.html', 'ja/guide/index.html', 'assets/guide.css']) {
    assert.equal(matchesPublication(rows.map(row => row.path === asset ? { ...row, sha256: digest(Buffer.from('stale')) } : row), hashes), false);
  }
});

test('focus measurements retain identity and refuse partial or entirely offscreen rectangles', () => {
  const names = ['document', 'getComputedStyle', 'innerWidth', 'innerHeight', 'scrollX', 'scrollY'];
  const saved = new Map(names.map(name => [name, Object.getOwnPropertyDescriptor(globalThis, name)]));
  let rect = { top: 800, bottom: 899, left: 16, right: 304, x: 16, y: 800, width: 288, height: 99 };
  const target = { tagName: 'PRE', id: '', textContent: 'Build command', getAttribute: () => null,
    getBoundingClientRect: () => rect };
  try {
    Object.assign(globalThis, { document: { activeElement: target }, innerWidth: 320, innerHeight: 900,
      scrollX: 0, scrollY: 5331, getComputedStyle: () => ({ outlineWidth: '3px', outlineStyle: 'solid' }) });
    const complete = focusObservation(target);
    requireFocus(complete);
    assert.deepEqual(complete.rect, rect);
    assert.deepEqual(complete.scroll, { x: 0, y: 5331 });
    assert.equal(complete.target.tag, 'PRE');
    assert.ok(Number.isFinite(complete.at));
    for (const changed of [{ bottom: 901 }, { top: -1 }, { left: -1 }, { right: 321 },
      { top: 901, bottom: 1000 }]) {
      const original = rect;
      rect = { ...rect, ...changed };
      assert.throws(() => requireFocus(focusObservation(target)));
      rect = original;
    }
    assert.throws(() => requireFocus(focusObservation({ ...target, id: 'different-target' })));
  } finally {
    for (const name of names) {
      if (saved.get(name)) Object.defineProperty(globalThis, name, saved.get(name));
      else delete globalThis[name];
    }
  }
});


test('an arbitrary error after two healthy clipped samples is not a condition timeout', () => {
  const partial = { matchesTarget: true, width: 239, height: 22, outlineWidth: 3, outlineStyle: 'solid',
    rect: { top: 878.328125, bottom: 900.328125, left: 16, right: 255, width: 239, height: 22 },
    viewport: { width: 1440, height: 900 } };
  const check = { passed: false, samples: [partial, partial] };
  for (const error of [new Error('measurement-protocol-or-persistence-failure'), new TypeError('invalid-measurement'),
    Object.assign(new Error('not-an-owned-timeout'), { name: 'FocusConditionTimeout' })]) {
    assert.throws(() => requireClippedFocusTimeout(error, check), value => value === error);
  }
});


const healthyPartial = () => ({ matchesTarget: true, inViewport: false, width: 239, height: 22,
  outlineWidth: 3, outlineStyle: 'solid',
  rect: { top: 878.328125, bottom: 900.328125, left: 16, right: 255, width: 239, height: 22 },
  viewport: { width: 1440, height: 900 } });

// The scheduler advances an injected clock only after queued observation
// microtasks finish. No control waits five wall-clock seconds.
function virtualDeadline() {
  let clock = 0;
  return { now: () => clock,
    pause: async milliseconds => { clock += milliseconds; },
    schedule: callback => {
      const timer = setImmediate(() => { clock = 5000; callback(); });
      return () => clearImmediate(timer);
    } };
}

async function timedPartial(sample = healthyPartial()) {
  const samples = [];
  try {
    await pollFocus(async () => sample, samples, virtualDeadline());
    assert.fail('A clipped target cannot pass');
  } catch (failure) {
    return { failure, check: { passed: false, samples } };
  }
}

test('only the owned condition deadline with healthy partial samples is accepted', async () => {
  const { failure, check } = await timedPartial();
  assert.ok(failure instanceof FocusConditionTimeout);
  requireClippedFocusTimeout(failure, check);
  assert.throws(() => requireClippedFocusTimeout(failure, { ...check, samples: [...check.samples] }));
  for (const changed of [
    { top: 901, bottom: 923 }, { top: -1 }, { left: -1 }, { right: 1441 },
    { width: 0 }, { height: 0 }, { bottom: NaN },
  ]) {
    const sample = healthyPartial();
    sample.rect = { ...sample.rect, ...changed };
    const invalid = await timedPartial(sample);
    assert.throws(() => requireClippedFocusTimeout(invalid.failure, invalid.check));
  }
});

test('two partial samples followed by evaluation or polling errors remain fatal', async () => {
  for (const stage of ['evaluation', 'polling']) {
    const arbitrary = new Error(stage + '-failure');
    const samples = [];
    const timer = virtualDeadline();
    let calls = 0;
    const measure = async () => {
      if (stage === 'evaluation' && calls++ === 2) throw arbitrary;
      return healthyPartial();
    };
    await assert.rejects(pollFocus(measure, samples, { ...timer, pause: async milliseconds => {
      if (stage === 'polling' && samples.length === 2) throw arbitrary;
      await timer.pause(milliseconds);
    } }), error => {
      assert.equal(error, arbitrary);
      assert.equal(samples.length, 2);
      assert.throws(() => requireClippedFocusTimeout(error, { passed: false, samples }), value => value === arbitrary);
      return true;
    });
  }
});

test('complete focus passes within the budget but never after the deadline', async () => {
  const complete = { ...healthyPartial(), inViewport: true,
    rect: { ...healthyPartial().rect, top: 877, bottom: 899 } };
  const samples = [];
  assert.equal(await pollFocus(async () => complete, samples), complete);
  let clock = 0;
  const late = [];
  await assert.rejects(pollFocus(async () => { clock = 5001; return complete; }, late,
    { now: () => clock }), error => error instanceof FocusObservationDeadline);
});


test('a never-settling measurement cannot outlive the absolute observation deadline', async () => {
  let clock = 0;
  let expire;
  const samples = [];
  let started = false;
  const pending = pollFocus(() => { started = true; return new Promise(() => {}); }, samples, {
    now: () => clock,
    schedule: callback => { expire = callback; return () => {}; },
  }).then(() => ({ kind: 'passed' }), error => ({ kind: 'failed', error }));
  await Promise.resolve();
  await Promise.resolve();
  assert.equal(started, true);
  clock = 5000;
  expire();
  const outcome = await Promise.race([pending,
    new Promise(resolve => setImmediate(() => resolve({ kind: 'still-pending' })))]);
  assert.equal(outcome.kind, 'failed');
  assert.ok(outcome.error instanceof FocusObservationDeadline);
  assert.equal(outcome.error.operation, 'measurement');
  assert.equal(samples.length, 0);
});


test('an unfinished poll wait is fatal and its late completion cannot start another measurement', async () => {
  let clock = 0;
  let expire, release;
  let measurements = 0;
  const samples = [];
  const pending = pollFocus(async () => { measurements++; return healthyPartial(); }, samples, {
    now: () => clock,
    schedule: callback => { expire = callback; return () => {}; },
    pause: async milliseconds => {
      if (samples.length === 2) return new Promise(resolve => { release = resolve; });
      clock += milliseconds;
    },
  });
  for (let i = 0; i < 100 && !release; i++) await Promise.resolve();
  assert.equal(typeof release, 'function');
  clock = 5000;
  expire();
  await assert.rejects(pending, error => {
    assert.ok(error instanceof FocusObservationDeadline);
    assert.equal(error.operation, 'poll-wait');
    assert.throws(() => requireClippedFocusTimeout(error, { passed: false, samples }), value => value === error);
    return true;
  });
  assert.equal(samples.length, 2);
  release();
  for (let i = 0; i < 10; i++) await Promise.resolve();
  assert.equal(measurements, 2);
  assert.equal(samples.length, 2);
});

test('late successful measurements never append samples or publish success after expiry', async () => {
  let clock = 0;
  let expire, release;
  let cancellations = 0;
  const samples = [];
  const pending = pollFocus(() => new Promise(resolve => { release = resolve; }), samples, {
    now: () => clock,
    schedule: callback => { expire = callback; return () => { cancellations++; }; },
  });
  await Promise.resolve();
  await Promise.resolve();
  clock = 5000;
  expire();
  await assert.rejects(pending, error => error instanceof FocusObservationDeadline);
  assert.equal(cancellations, 1);
  release({ ...healthyPartial(), inViewport: true });
  for (let i = 0; i < 10; i++) await Promise.resolve();
  assert.equal(samples.length, 0);
});


test('a completed delayed pause enters the final idle window without another measurement', async () => {
  let clock = 0;
  let measurements = 0;
  let pauses = 0;
  const samples = [];
  await assert.rejects(pollFocus(async () => { measurements++; return healthyPartial(); }, samples, {
    now: () => clock,
    pause: async () => { clock = ++pauses === 1 ? 100 : 4950; },
    schedule: callback => {
      const timer = setImmediate(() => { clock = 5000; callback(); });
      return () => clearImmediate(timer);
    },
  }), error => {
    assert.ok(error instanceof FocusConditionTimeout);
    requireClippedFocusTimeout(error, { passed: false, samples });
    return true;
  });
  assert.equal(measurements, 2);
  assert.equal(samples.length, 2);
});


test('both fixed locale routes bind separate pages and reciprocal native language links', () => {
  assert.deepEqual(locales.map(locale => locale.language), ['en', 'ja']);
  for (const locale of locales) {
    const other = locales.find(candidate => candidate.language !== locale.language);
    assert.equal(assets.includes(locale.asset), true);
    assert.equal(assetRoute(locale.asset), locale.route);
    assert.equal(locale.switchLanguage, other.language);
    assert.equal(locale.groups.length, 5);
    assert.equal(locale.profiles.length, 5);
    assert.ok(locale.matrixCaption && locale.matrixRegion);
    assert.deepEqual(locale.matrixCorner.split(' / ').length, 2);
    assert.equal(locale.headings[0], locale.language === 'en' ? 'Input and output support' : '入力と出力の対応表');
    assert.equal(locale.headings.length, 4);
    assert.equal(new Set(locale.groups).size, 5);
    for (const mount of ['/', '/drmr/']) {
      const current = 'http://127.0.0.1:4173' + mount + locale.route;
      const next = new URL(locale.switchHref, current).href;
      assert.equal(next, 'http://127.0.0.1:4173' + mount + other.route);
      assert.equal(resource(new URL(next).pathname).relative, other.asset);
      assert.equal(assetPath(next), other.asset);
    }
  }
});


function matrixSample(changes = {}) {
  return { overflowX: 'auto', clientWidth: 286, clientHeight: 640,
    rect: { left: 16, right: 304 }, contentLeft: 17, contentRight: 303,
    documentWidth: 320, documentScrollWidth: 320, scrollWidth: 648, scrollLeft: 0,
    pageX: 0, pageY: 250, focused: true, clippedCells: [], hiddenText: [], renderedTextCount: 30,
    rowHeaders: Array.from({ length: 5 }, () => ({ left: 17, right: 153, position: 'sticky' })),
    ...changes };
}

test('conversion facts unify MP4 and distinguish TS video and TS audio-only routes', () => {
  assert.deepEqual(matrixColumns, ['MP4', 'DASH', 'HLS fMP4', 'HLS MPEG-TS']);
  assert.deepEqual(matrixRows.map(row => row.id), ['mp4', 'dash', 'hls-fmp4', 'hls-ts-video', 'hls-ts-audio']);
  assert.equal(matrixRows.flatMap(row => row.output).length, 20);
  assert.deepEqual(matrixRows[0].input, ['Clear', 'cenc', 'cbcs']);
  assert.deepEqual(matrixRows[2].input, ['Clear', 'cenc', 'cbcs', 'AES-128']);
  assert.deepEqual(matrixRows[0].output, [['Clear', 'cenc', 'cbcs'], ['Clear', 'cenc', 'cbcs'], ['Clear', 'cenc', 'cbcs', 'AES-128'], ['Clear', 'AES-128', 'SAMPLE-AES']]);
  assert.deepEqual(matrixRows[3].output.slice(0, 2), [['Clear', 'cenc', 'cbcs'], ['Clear', 'cenc', 'cbcs']]);
  assert.deepEqual(matrixRows[4].output, [['Clear', 'cenc', 'cbcs'], ['Clear', 'cenc', 'cbcs'], ['Clear', 'cenc', 'cbcs', 'AES-128'], ['Clear', 'AES-128', 'SAMPLE-AES']]);
  for (const index of [0, 1, 2]) assert.deepEqual(matrixRows[index].output[3], ['Clear', 'AES-128', 'SAMPLE-AES']);
});

test('matrix bounds refuse page overflow, clipped cells and unanchored row headers', () => {
  requireMatrixLayout(matrixSample());
  for (const change of [{ overflowX: 'hidden' }, { clientWidth: 0 },
    { rect: { left: -1, right: 304 } }, { documentScrollWidth: 321 },
    { clippedCells: ['SAMPLE-AES'] }, { hiddenText: [{ text: 'Clear', hidden: true }] }, { renderedTextCount: 0 }, { rowHeaders: [] },
    { rowHeaders: Array.from({ length: 5 }, () => ({ left: 17, right: 153, position: 'static' })) },
    { rowHeaders: Array.from({ length: 5 }, () => ({ left: 17, right: 330, position: 'sticky' })) }]) {
    assert.throws(() => requireMatrixLayout(matrixSample(change)));
  }
});

test('native matrix movement cannot pass for a stationary matrix, lost focus or document movement', () => {
  const before = matrixSample();
  const after = matrixSample({ scrollLeft: 40 });
  requireMatrixMovement(before, after, 'right');
  requireMatrixMovement(after, before, 'left');
  for (const change of [{ scrollLeft: 0 }, { focused: false }, { pageX: 1 }, { pageY: 251 },
    { rowHeaders: Array.from({ length: 5 }, () => ({ left: 18, right: 154, position: 'sticky' })) }]) {
    assert.throws(() => requireMatrixMovement(before, { ...after, ...change }, 'right'));
  }
  assert.throws(() => requireMatrixMovement(before, after, 'left'));
});

test('layout only permits matrix-owned content inside its full scrollable bounds', () => {
  const saved = Object.getOwnPropertyDescriptor(globalThis, 'document');
  const rectangle = (left, right) => ({ left, right, width: right - left });
  const wrapper = { tagName: 'DIV', textContent: 'Matrix', clientLeft: 1, scrollLeft: 0, scrollWidth: 648,
    getBoundingClientRect: () => rectangle(16, 304), closest: () => wrapper };
  let tableRight = 665;
  const table = { tagName: 'TABLE', textContent: 'Facts', getBoundingClientRect: () => rectangle(17, tableRight), closest: () => wrapper };
  const outside = { tagName: 'P', textContent: 'Unrelated', getBoundingClientRect: () => rectangle(16, 304), closest: () => null };
  try {
    Object.defineProperty(globalThis, 'document', { configurable: true, value: {
      documentElement: { clientWidth: 320, scrollWidth: 320 }, querySelectorAll: () => [wrapper, table, outside],
    } });
    requireLayout(layoutObservation());
    tableRight = 666;
    assert.throws(() => requireLayout(layoutObservation()));
    tableRight = 665;
    outside.getBoundingClientRect = () => rectangle(16, 321);
    assert.throws(() => requireLayout(layoutObservation()));
    outside.getBoundingClientRect = () => rectangle(16, 304);
    wrapper.getBoundingClientRect = () => rectangle(16, 321);
    assert.throws(() => requireLayout(layoutObservation()));
  } finally {
    if (saved) Object.defineProperty(globalThis, 'document', saved);
    else delete globalThis.document;
  }
});


test('rendered matrix guards refuse hidden Clear children even when their cell stays visible', () => {
  const names = ['document', 'getComputedStyle', 'NodeFilter', 'scrollX', 'scrollY'];
  const saved = new Map(names.map(name => [name, Object.getOwnPropertyDescriptor(globalThis, name)]));
  const rectangle = (left, right) => ({ left, right, top: 20, bottom: 60, width: right - left, height: 40 });
  const box = (rect, style = {}) => ({ style, parentElement: null, texts: [], textContent: '',
    scrollWidth: rect.width, clientWidth: rect.width, scrollHeight: 40, clientHeight: 40,
    getBoundingClientRect: () => rect });
  const wrapper = box({ left: 16, right: 304, top: 10, bottom: 650, width: 288, height: 640 }, { overflowX: 'auto' });
  Object.assign(wrapper, { clientLeft: 1, clientWidth: 286, clientHeight: 640, scrollWidth: 648, scrollLeft: 0 });
  const headers = Array.from({ length: 5 }, () => box(rectangle(17, 153), { position: 'sticky' }));
  const cell = box(rectangle(153, 281));
  const clear = box(rectangle(165, 202));
  cell.parentElement = wrapper;
  clear.parentElement = cell;
  const text = { textContent: 'Clear', parentElement: clear, rects: [rectangle(165, 202)] };
  cell.texts = [text]; cell.textContent = 'Clear';
  wrapper.querySelectorAll = selector => selector === 'tbody th[scope="row"]' ? headers :
    selector === 'th,td' ? [...headers, cell] : [cell];
  try {
    Object.assign(globalThis, { scrollX: 0, scrollY: 250, NodeFilter: { SHOW_TEXT: 4 },
      getComputedStyle: element => ({ display: 'block', visibility: 'visible', opacity: '1', ...element.style }),
      document: { activeElement: wrapper, documentElement: { clientWidth: 320, scrollWidth: 320 },
        createTreeWalker: element => { let index = 0; return { nextNode: () => element.texts[index++] || null }; },
        createRange: () => { let node; return { selectNodeContents: value => { node = value; }, getClientRects: () => node.rects }; },
      } });
    requireMatrixLayout(matrixObservation(wrapper));
    clear.style.visibility = 'hidden';
    assert.throws(() => requireMatrixLayout(matrixObservation(wrapper)), /Rendered matrix text/);
    clear.style.visibility = 'visible';
    cell.style.opacity = '0';
    assert.throws(() => requireMatrixLayout(matrixObservation(wrapper)), /Rendered matrix text/);
    cell.style.opacity = '1';
    text.rects = [];
    assert.throws(() => requireMatrixLayout(matrixObservation(wrapper)), /Rendered matrix text/);
    text.rects = [rectangle(165, 290)];
    assert.throws(() => requireMatrixLayout(matrixObservation(wrapper)), /Rendered matrix text/);
    text.rects = [rectangle(165, 202)];
    requireMatrixLayout(matrixObservation(wrapper));
  } finally {
    for (const name of names) {
      const previous = saved.get(name);
      if (previous) Object.defineProperty(globalThis, name, previous);
      else delete globalThis[name];
    }
  }
});


test('matrix keys wait for their own native scrollend before observing or issuing the next key', async () => {
  const calls = [];
  let release;
  let entered;
  const waiting = new Promise(resolve => { entered = resolve; });
  const ended = new Promise(resolve => { release = resolve; });
  const pending = completeMatrixKey({ before: matrixSample(), direction: 'right', deadline: 5000,
    now: () => 0, schedule: () => () => {},
    arm: async () => { calls.push('arm'); return { dispose: async () => { calls.push('dispose'); } }; },
    press: async () => { calls.push('press'); },
    wait: async () => { calls.push('wait'); entered(); await ended; },
    observe: async () => { calls.push('observe'); return matrixSample({ scrollLeft: 40 }); },
  });
  await waiting;
  // A partial animation delta can exist, but cannot complete this operation.
  assert.deepEqual(calls, ['arm', 'press', 'wait']);
  release();
  assert.equal((await pending).scrollLeft, 40);
  assert.deepEqual(calls, ['arm', 'press', 'wait', 'observe', 'dispose']);
});

test('matrix key failures retain their cause, remove the listener and never start a later observation', async () => {
  const failure = new Error('native-input-failed');
  let disposed = 0;
  let observed = 0;
  await assert.rejects(completeMatrixKey({ before: matrixSample(), direction: 'right', deadline: 5000,
    now: () => 0, schedule: () => () => {},
    arm: async () => ({ dispose: async () => { disposed++; } }),
    press: async () => { throw failure; }, wait: async () => assert.fail('no wait after input failure'),
    observe: async () => { observed++; return matrixSample({ scrollLeft: 40 }); },
  }), error => error === failure);
  assert.equal(disposed, 1);
  assert.equal(observed, 0);
});

test('each pending matrix-key operation is bounded by the same five-second traversal deadline', async () => {
  for (const stage of ['setup', 'keypress', 'scrollend', 'geometry', 'cleanup']) {
    const timer = virtualDeadline();
    const never = () => new Promise(() => {});
    const owner = { dispose: stage === 'cleanup' ? never : async () => {} };
    await assert.rejects(completeMatrixKey({ before: matrixSample(), direction: 'right', deadline: 5000,
      now: timer.now, schedule: timer.schedule,
      arm: stage === 'setup' ? never : async () => owner,
      press: stage === 'keypress' ? never : async () => {},
      wait: stage === 'scrollend' ? never : async () => {},
      observe: stage === 'geometry' ? never : async () => matrixSample({ scrollLeft: 40 }),
    }), error => error instanceof MatrixKeyDeadline);
  }
});

test('OFF discovery selects only a healthy partial bottom and refuses unrelated failures', () => {
  assert.equal(partialFocusCandidate(healthyPartial()), true);
  assert.equal(partialFocusCandidate({ ...healthyPartial(), inViewport: true }), false);
  for (const change of [{ matchesTarget: false }, { outlineWidth: 0 }, { outlineStyle: 'none' },
    { width: 0 }, { rect: { ...healthyPartial().rect, top: 901, bottom: 923 } },
    { rect: { ...healthyPartial().rect, left: -1 } }]) {
    assert.throws(() => partialFocusCandidate({ ...healthyPartial(), ...change }));
  }
  assert.throws(() => partialFocusCandidate(null), TypeError);
});


test('nested guide routes and generation inputs belong to the exact payload contract', () => {
  assert.equal(assets.length, 12);
  for (const [path, expected] of [['/guide/', 'guide/index.html'],
    ['/drmr/guide/index.html', 'guide/index.html'],
    ['/ja/guide/', 'ja/guide/index.html'], ['/drmr/ja/guide/', 'ja/guide/index.html']]) {
    assert.equal(resource(path).relative, expected);
    assert.equal(assetPath('http://127.0.0.1:4173' + path), expected);
  }
  assert.equal(resource('/drmr/assets/guide.css').type, 'text/css; charset=utf-8');
  assert.deepEqual(identity.builderInputs, ['tools/build_guide_site.py', 'tools/conversion_examples.py', 'tools/templates/guide.html',
    'site-content/guide/en.md', 'site-content/guide/ja.md']);
  for (const path of ['/drmr/guide/keys.json', '/drmr/ja/guide/%2e%2e%2fprivate.json',
    '/drmr/site-content/guide/en.md', '/drmr/tools/build_guide_site.py']) {
    assert.equal(resource(path), null);
  }
});

test('each supported protection token has one stable recipe identity and unavailable cells have none', () => {
  assert.equal(matrixRecipes.length, 65);
  assert.equal(new Set(matrixRecipes.map(recipe => recipe.id)).size, 65);
  assert.equal(matrixRecipes[0].id, 'recipe-mp4-mp4-clear');
  assert.equal(matrixRecipes.at(-1).id, 'recipe-hls-ts-audio-hls-ts-hls-sample-aes');
  for (const row of matrixRows) {
    assert.equal(matrixRecipes.filter(recipe => recipe.row === row.id).length, 13);
    for (const [column, labels] of row.output.entries()) {
      assert.deepEqual(matrixRecipes.filter(recipe => recipe.row === row.id && recipe.column === column)
        .map(recipe => recipe.label), labels);
    }
  }
});

test('every matrix recipe belongs to one complete navigation batch including the last five', () => {
  assert.deepEqual(matrixRecipeBatches.map(batch => batch.offset), [0, 15, 30, 45, 60]);
  assert.deepEqual(matrixRecipeBatches.map(batch => batch.end), [15, 30, 45, 60, 65]);
  assert.deepEqual(matrixRecipeBatches.map(batch => batch.recipes.length), [15, 15, 15, 15, 5]);
  assert.deepEqual(matrixRecipeBatches.flatMap(batch => batch.recipes), matrixRecipes);
  assert.equal(new Set(matrixRecipeBatches.flatMap(batch => batch.recipes.map(recipe => recipe.id))).size, 65);
  assert.deepEqual(landingFocusTargetCounts, { en: 102, ja: 101 });
});

test('guide code inventory counts literal Markdown examples and every independently expected recipe figure', () => {
  assert.equal(matrixRecipes.filter(recipe => recipe.target === 'clear').length, 20);
  assert.equal(matrixRecipes.filter(recipe => recipe.target !== 'clear').length, 45);
  assert.equal(matrixRecipeExampleCount, 65);
  for (const [language, literalCount] of [['en', 45], ['ja', 45]]) {
    const source = readFileSync(new URL(`../../site-content/guide/${language}.md`, import.meta.url), 'utf8');
    assert.equal(guideSourceExampleCount(source, matrixRecipeExampleCount), literalCount + 65);
  }
  const source = '```sh\ndrmr --version\n```\n\n{{conversion-examples}}\n';
  assert.equal(guideSourceExampleCount(source, 65), 66);
  assert.equal(guideSourceExampleCount(source.replace('drmr --version', '{{conversion-examples}}'), 65), 66);
  for (const invalid of [source.replace('{{conversion-examples}}', ''), source + '{{conversion-examples}}\n',
    source.replace('```sh', '```html'), source.replace('```\n\n', ''),
    source.replace('drmr --version\n', ''), '{{conversion-examples}}\n']) {
    assert.throws(() => guideSourceExampleCount(invalid, 65));
  }
  for (const count of [0, -1, 1.5, NaN, Infinity]) assert.throws(() => guideSourceExampleCount(source, count));
});

function recipeSample(recipe = matrixRecipes.find(value => value.id === 'recipe-hls-ts-video-dash-cbcs')) {
  const extension = recipe.row === 'mp4' ? 'mp4' : recipe.row === 'dash' ? 'mpd' : 'm3u8';
  const profiles = recipe.row.startsWith('hls-ts-') ? ['hls-aes128', 'hls-sample-aes']
    : recipe.row === 'hls-fmp4' ? ['cenc-cbcs', 'hls-aes128'] : ['cenc-cbcs'];
  const paragraph = (text, links = []) => ({ tag: 'P', text, links });
  const root = 'https://github.com/kazuhide/drmr/blob/main/docs/examples/';
  const encrypted = recipe.target !== 'clear';
  const keyLink = mode => ({ text: mode === 'clear' ? 'keys.json'
    : { 'cenc-cbcs': 'cenc / cbcs', 'hls-aes128': 'AES-128', 'hls-sample-aes': 'SAMPLE-AES' }[mode],
    href: root + `keys/${recipe.format}-${recipe.target}-from-${mode}.json` });
  const command = keys => `drmr convert "https://example.com/input.${extension}" \\\n  --download-dir src --outputs outputs.json --output result${keys ? ' --keys-file keys.json' : ''}\n`;
  const code = encrypted ? command(true)
    : '# Clear input\n' + command(false) + '\n# Encrypted input\n' + command(true);
  const entrypoint = `result/${recipe.format}/` + (recipe.format === 'mp4' ? 'media.mp4'
    : recipe.format === 'dash' ? 'manifest.mpd' : 'master.m3u8');
  const blocks = [paragraph('outputs.json', [{ text: 'outputs.json', href: root + `outputs/${recipe.format}-${recipe.target}.json` }]),
    paragraph(encrypted ? 'Clear input: keys.json.' : 'Clear input: no keys.json.', encrypted ? [keyLink('clear')] : []),
    paragraph('Encrypted input: save the matching complete example as keys.json.', profiles.map(keyLink)),
    { tag: 'FIGURE', figureClass: true, language: 'sh', preCount: 1, codeCount: 1, tabindex: '0', code, rendered: code },
    paragraph('Result: ' + entrypoint)];
  return { id: recipe.id, tag: 'H3', tabindex: '-1', chapter: 'url-input', heading: 'HLS to DASH',
    presented: true, setupTag: 'P', setupHrefs: ['#recipe-setup'], blocks };
}

function mutateRecipeCode(value, transform) {
  const result = structuredClone(value);
  const figure = result.blocks.find(block => block.tag === 'FIGURE');
  figure.code = transform(figure.code); figure.rendered = figure.code;
  return result;
}

test('recipe checks require complete file links, matching destinations and rendered commands', () => {
  const expected = matrixRecipes.find(value => value.id === 'recipe-hls-ts-video-dash-cbcs');
  const good = recipeSample(expected);
  requireRecipe(good, expected);
  for (const changed of [{ id: 'recipe-hls-ts-video-dash-clear' }, { tag: 'H2' }, { tabindex: null },
    { chapter: 'verification' }, { heading: '' }, { presented: false }, { setupTag: 'DIV' },
    { setupHrefs: [] }, { setupHrefs: ['#recipe-setup', '#unrelated'] }, { blocks: [] }]) {
    assert.throws(() => requireRecipe({ ...good, ...changed }, expected));
  }
  for (let index = 0; index < good.blocks.length; index++) {
    const changed = structuredClone(good); changed.blocks.splice(index, 1);
    assert.throws(() => requireRecipe(changed, expected));
  }
  for (const field of [{ tag: 'PRE' }, { figureClass: false }, { language: 'text' },
    { preCount: 2 }, { codeCount: 2 }, { tabindex: null }, { rendered: '' }]) {
    const changed = structuredClone(good); Object.assign(changed.blocks[3], field);
    assert.throws(() => requireRecipe(changed, expected));
  }
});

test('all 65 recipes link their exact output file and clear-input keys only when needed', () => {
  for (const expected of matrixRecipes) {
    const good = recipeSample(expected);
    requireRecipe(good, expected);
    const indices = expected.target === 'clear' ? [0] : [0, 1];
    for (const index of indices) {
      for (const change of [links => [], links => [...links, links[0]], links => [{ ...links[0], text: 'other.json' }],
        links => [{ ...links[0], href: links[0].href.replace('/examples/', '/examples/stale/') }]]) {
        const changed = structuredClone(good); changed.blocks[index].links = change(good.blocks[index].links);
        assert.throws(() => requireRecipe(changed, expected));
      }
    }
    if (expected.target === 'clear') {
      const changed = structuredClone(good);
      changed.blocks[1].links = [{ text: 'keys.json', href: 'https://example.com/keys.json' }];
      assert.throws(() => requireRecipe(changed, expected));
    }
    const changed = structuredClone(good); changed.blocks.at(-1).text = 'Result: result/incorrect.bin';
    assert.throws(() => requireRecipe(changed, expected));
  }
});

test('each recipe links complete key files for every admitted encrypted input profile in order', () => {
  for (const expected of matrixRecipes) {
    const good = recipeSample(expected);
    requireRecipe(good, expected);
    const paragraph = good.blocks[2];
    assert.ok(paragraph.links.length >= 1);
    for (let index = 0; index < paragraph.links.length; index++) {
      for (const change of [links => links.filter((_, current) => current !== index),
        links => [...links, links[index]], links => links.map((link, current) => current === index
          ? { ...link, href: link.href.replace('-from-', '-unsupported-from-') } : link),
        links => links.map((link, current) => current === index ? { ...link, text: '' } : link)]) {
        const changed = structuredClone(good); changed.blocks[2].links = change(paragraph.links);
        assert.throws(() => requireRecipe(changed, expected), expected.id);
      }
    }
    if (paragraph.links.length > 1) {
      const changed = structuredClone(good); changed.blocks[2].links.reverse();
      assert.throws(() => requireRecipe(changed, expected));
    }
  }
});

test('all linked output and key examples are complete matching schema-3 and schema-2 files', () => {
  const directory = new URL('../../docs/examples/', import.meta.url);
  const inspected = { outputs: new Set(), keys: new Set() };
  for (const recipe of matrixRecipes) {
    const sample = recipeSample(recipe);
    const outputName = `${recipe.format}-${recipe.target}.json`;
    const outputSpec = JSON.parse(readFileSync(new URL('outputs/' + outputName, directory), 'utf8'));
    assert.deepEqual(outputSpec, { schema_version: 3, outputs: [
      { id: recipe.format, format: recipe.format, target: recipe.target, drms: [] }] }, outputName);
    inspected.outputs.add(outputName);
    for (const link of [...sample.blocks[1].links, ...sample.blocks[2].links]) {
      const name = link.href.split('/').at(-1);
      const mode = name.slice(name.indexOf('-from-') + '-from-'.length, -'.json'.length);
      const input = mode === 'clear' ? null : mode === 'cenc-cbcs'
        ? { keys: [{ kid: '000102030405060708090a0b0c0d0e0f', key: '00112233445566778899aabbccddeeff' }] }
        : { hls_keys: [{ uri: mode === 'hls-sample-aes' ? 'skd://source-content' : 'https://example.com/keys/content.key',
          key: '00112233445566778899aabbccddeeff', ...(mode === 'hls-sample-aes'
            ? { iv: '000102030405060708090a0b0c0d0e0f' } : {}) }] };
      const outputs = {};
      if (recipe.target !== 'clear') {
        const key = { key: 'ffeeddccbbaa99887766554433221100' };
        if (['cenc', 'cbcs'].includes(recipe.target)) key.kid = '101112131415161718191a1b1c1d1e1f';
        if (['hls', 'hls-ts'].includes(recipe.format)) key.hls_key_uri = 'https://keys.example.invalid/content.key';
        if (recipe.target === 'hls-sample-aes') key.iv = '000102030405060708090a0b0c0d0e0f';
        outputs[recipe.format] = key;
      }
      const content = readFileSync(new URL('keys/' + name, directory), 'utf8');
      assert.deepEqual(JSON.parse(content), { schema_version: 2, ...(input ? { input } : {}), outputs }, name);
      inspected.keys.add(name);
    }
  }
  assert.equal(inspected.outputs.size, 13); assert.equal(inspected.keys.size, 48);
  for (const role of ['outputs', 'keys']) assert.deepEqual(
    readdirSync(new URL(role + '/', directory)).filter(name => name.endsWith('.json')).sort(), [...inspected[role]].sort());
});

test('identical keyed conversions share one exact command while clear output retains both key roles', () => {
  for (const expected of matrixRecipes) {
    const good = recipeSample(expected);
    requireRecipe(good, expected);
    const code = good.blocks[3].code;
    assert.equal([...code.matchAll(/^drmr convert /gm)].length, expected.target === 'clear' ? 2 : 1);
    const changes = [
      ['drmr convert', 'drmr analyze'], ['--download-dir src', '--download-dir other'],
      ['--outputs outputs.json', '--outputs stale.json'], ['--output result', '--output result.mp4'],
      ['https://example.com/', 'src/'], ['input.', 'encrypted.'],
      [' \\\n', '\n'], [' \\\n', ' \\ \n'], [' \\\n', ' \\\n\n'], [' \\\n', ' \\\n \t \n'],
      ...(expected.target === 'clear' ? [['# Clear input', '# cbc1 input'], ['# Encrypted input', '# Clear input']] : []),
    ];
    for (const [before, after] of changes) assert.throws(() => requireRecipe(mutateRecipeCode(good,
      value => value.replace(before, after)), expected), `${expected.id}: ${before}`);
    for (const change of [value => value.replace(/"https:\/\/[^"\n]+"/, url => url.slice(1, -1)),
      value => value + 'drmr verify --plan plan.json\n', value => value + ' &&\n',
      value => '# Clear input\n' + value]) {
      assert.throws(() => requireRecipe(mutateRecipeCode(good, change), expected));
    }
    if (expected.target === 'clear') assert.throws(() => requireRecipe(mutateRecipeCode(good,
      value => value.split('\n\n').reverse().join('\n\n')), expected));
    for (let index = 0; index < (expected.target === 'clear' ? 2 : 1); index++) {
      const required = index === 1 || expected.target !== 'clear';
      const change = value => value.split('\n\n').map((group, current) => current !== index ? group : required
        ? group.replace(' --keys-file keys.json', '') : group + ' --keys-file keys.json').join('\n\n');
      assert.throws(() => requireRecipe(mutateRecipeCode(good, change), expected));
    }
  }
});

function recipePositionSample() {
  return { ...recipeSample(), focused: true,
    url: 'http://127.0.0.1:4173/drmr/guide/#recipe-hls-ts-video-dash-cbcs', pageLanguage: 'en',
    rect: { left: 16, right: 304, top: 24, bottom: 84 }, viewport: { width: 320, height: 900 } };
}

test('recipe identity is immutable and position rejects unfocused, nonfinite or clipped headings', () => {
  const good = recipePositionSample();
  const recipe = matrixRecipes.find(value => value.id === good.id);
  const source = { url: good.url, language: 'en' };
  requireRecipeIdentity(good, recipe, source);
  requireRecipePosition(good);
  for (const changed of [{ id: 'recipe-mp4-mp4-clear' }, { tag: 'H2' }, { tabindex: null },
    { chapter: 'verification' }, { url: source.url + '-stale' }, { pageLanguage: 'ja' }]) {
    assert.throws(() => requireRecipeIdentity({ ...good, ...changed }, recipe, source));
  }
  assert.throws(() => requireRecipePosition({ ...good, focused: false }));
  for (const changed of [{ left: -1 }, { right: 321 }, { top: -3288.53125 }, { bottom: 901 },
    { right: 16 }, { bottom: 24 }, { top: NaN }, { left: Infinity }]) {
    assert.throws(() => requireRecipePosition({ ...good, rect: { ...good.rect, ...changed } }));
  }
  for (const changed of [{ width: 0 }, { height: Infinity }]) {
    assert.throws(() => requireRecipePosition({ ...good, viewport: { ...good.viewport, ...changed } }));
  }
});

test('the actual recipe observer preserves all settings, commands and links independently of page locale', () => {
  const recipe = matrixRecipes.find(value => value.id === 'recipe-hls-ts-video-dash-cbcs');
  const sample = recipeSample(recipe);
  for (const language of ['en', 'ja']) {
    const url = 'http://127.0.0.1:4173/drmr/' + (language === 'ja' ? 'ja/' : '') + 'guide/#' + recipe.id;
    const nodes = sample.blocks.map(block => {
      if (block.tag !== 'FIGURE') {
        const links = block.links.map(link => Object.freeze({ textContent: link.text,
          getAttribute: name => name === 'href' ? link.href : null }));
        return { tagName: block.tag, textContent: block.text,
          querySelectorAll: selector => selector === 'a' ? Object.freeze(links) : Object.freeze([]) };
      }
      const code = Object.freeze({ textContent: block.code, innerText: block.rendered });
      const pre = Object.freeze({ getAttribute: name => name === 'tabindex' ? '0' : null });
      return { tagName: 'FIGURE', classList: Object.freeze({ contains: name => name === 'guide-example' }),
        getAttribute: name => name === 'data-code-language' ? block.language : null,
        querySelector: selector => selector === 'pre code' ? code : selector === 'pre' ? pre : null,
        querySelectorAll: selector => selector === 'pre' ? Object.freeze([pre])
          : selector === 'pre code' ? Object.freeze([code]) : Object.freeze([]) };
    });
    for (let index = 0; index < nodes.length; index++) nodes[index].nextElementSibling = nodes[index + 1] ?? null;
    nodes.forEach(Object.freeze);
    const link = Object.freeze({ getAttribute: name => name === 'href' ? '#recipe-setup' : null });
    const setup = Object.freeze({ tagName: 'P', nextElementSibling: nodes[0],
      querySelectorAll: selector => selector === 'a' ? Object.freeze([link]) : Object.freeze([]) });
    const box = Object.freeze({ left: 16, right: 304, top: 24, bottom: 84, width: 288, height: 60 });
    const element = Object.freeze({ id: recipe.id, tagName: 'H3', textContent: 'HLS → DASH / cbcs',
      nextElementSibling: setup, getAttribute: name => name === 'tabindex' ? '-1' : null,
      closest: selector => selector === '[data-guide-chapter]' ? Object.freeze({ id: 'url-input' }) : null,
      getBoundingClientRect: () => box });
    const sandbox = { element, innerWidth: 320, innerHeight: 900,
      location: Object.freeze({ href: url }), document: Object.freeze({ activeElement: element,
        documentElement: Object.freeze({ lang: language }), readyState: 'complete',
        fonts: Object.freeze({ status: 'loaded' }) }),
      getComputedStyle: () => Object.freeze({ display: 'block', visibility: 'visible' }) };
    const raw = runInNewContext(`(${recipeObservation.toString()})(element)`, sandbox, { timeout: 1000 });
    // Playwright returns serialized values across its browser/runner boundary.
    // This fixture checks that shape; it does not establish browser behavior.
    const observed = JSON.parse(JSON.stringify(raw));
    assert.equal(observed.pageLanguage, language);
    assert.deepEqual(observed.blocks, sample.blocks);
    assert.equal(observed.url, url);
    requireRecipeIdentity(observed, recipe, { url, language });
    requireRecipe(observed, recipe);
    requireRecipePosition(observed);
    assert.throws(() => requireRecipeIdentity({ ...observed, pageLanguage: 'sh' }, recipe, { url, language }));
    const changed = structuredClone(observed); changed.blocks[3].language = language;
    assert.throws(() => requireRecipe(changed, recipe));
  }
});

test('a recipe validator retains raw displaced samples and accepts only the actual focused viewport result', async () => {
  const good = recipePositionSample();
  const displaced = { ...good, rect: { ...good.rect, top: -3288.53125, bottom: -3228.53125 } };
  const samples = [];
  let calls = 0;
  const result = await pollFocus(async () => calls++ ? good : displaced, samples,
    { ...virtualDeadline(), validate: requireRecipePosition });
  assert.equal(result, good);
  assert.deepEqual(samples, [displaced, good]);
  assert.equal(samples[0].rect.top, -3288.53125);
  // The original default still requires a real keyboard outline; recipe
  // positions never invent outline fields for a pointer-focused heading.
  assert.throws(() => requireFocus(good));
});

test('custom focus validators retain the same absolute deadline and propagate source or validator errors', async () => {
  const good = recipePositionSample();
  const recipe = matrixRecipes.find(value => value.id === good.id);
  const source = { url: good.url, language: 'en' };
  const invalid = { ...good, url: good.url + '-stale' };
  const samples = [];
  await assert.rejects(pollFocus(async () => {
    requireRecipeIdentity(invalid, recipe, source);
    return invalid;
  }, samples, { ...virtualDeadline(), validate: requireRecipePosition }), /Expected values/);
  assert.deepEqual(samples, []);
  const failure = new TypeError('recipe-validator-failure');
  await assert.rejects(pollFocus(async () => good, [], { ...virtualDeadline(), validate: () => { throw failure; } }),
    error => error === failure);
  let clock = 0, validated = false;
  const late = [];
  await assert.rejects(pollFocus(async () => { clock = 5001; return good; }, late,
    { now: () => clock, validate: () => { validated = true; } }), error => error instanceof FocusObservationDeadline);
  assert.deepEqual(late, []);
  assert.equal(validated, false);
  const displaced = { ...good, rect: { ...good.rect, top: -1 } };
  await assert.rejects(pollFocus(async () => displaced, [], { ...virtualDeadline(), validate: requireRecipePosition }),
    error => error instanceof FocusConditionTimeout && error.elapsedMs >= 5000);
});

test('matrix link checks refuse point occlusion by sticky headers without weakening ordinary focus', () => {
  requireMatrixLinkExposure({ matrixLink: false });
  const points = Array.from({ length: 5 }, () => ({ x: 200, y: 300, exposed: true }));
  requireMatrixLinkExposure({ matrixLink: true, points });
  assert.throws(() => requireMatrixLinkExposure({ matrixLink: true, points: [] }));
  assert.throws(() => requireMatrixLinkExposure({ matrixLink: true, points: points.map((point, index) =>
    index ? point : { ...point, exposed: false }) }));
});


test('guide guards refuse missing or hidden chapters and page overflow', () => {
  const good = { clientWidth: 320, scrollWidth: 320, clipped: [],
    chapters: guideChapters.map(id => ({ id, key: id, title: id, presented: true })) };
  requireGuideLayout(good);
  for (const changed of [{ scrollWidth: 321 }, { clipped: [{ tag: 'PRE' }] },
    { chapters: good.chapters.slice(1) },
    { chapters: good.chapters.map((row, i) => i ? row : { ...row, presented: false }) },
    { chapters: good.chapters.map((row, i) => i ? row : { ...row, title: '' }) }]) {
    assert.throws(() => requireGuideLayout({ ...good, ...changed }));
  }
});

test('guide scrolling uses the shared deadline and propagates its own guard failure after cleanup', async () => {
  const before = { focused: true, tag: 'PRE', chapter: 'installation', identity: 'example-1-title',
    pageX: 0, pageY: 1000, clientWidth: 288, scrollWidth: 500, scrollLeft: 0,
    clientHeight: 574, scrollHeight: 1048, scrollTop: 0, overflowX: 'auto', overflowY: 'auto',
    documentWidth: 320, documentHeight: 900,
    rect: { left: 16, right: 304, width: 288, top: 100, bottom: 676, height: 576 } };
  const after = { ...before, scrollLeft: 40 };
  requireGuideMovement(before, after, 'right');
  for (const changed of [{ focused: false }, { identity: 'other' }, { pageY: 1001 },
    { scrollLeft: 0 }, { scrollTop: 1 }, { overflowX: 'clip' }, { rect: { left: 16, right: 321 } }]) {
    assert.throws(() => requireGuideMovement(before, { ...after, ...changed }, 'right'));
  }
  let disposed = 0;
  const actual = new Error('guide-owned-movement-refusal');
  await assert.rejects(completeMatrixKey({ before, direction: 'right', deadline: performance.now() + 5000,
    arm: async () => ({ dispose: async () => { disposed++; } }), press: async () => {},
    wait: async () => {}, observe: async () => after,
    validateMovement: () => { throw actual; },
  }), error => error === actual);
  assert.equal(disposed, 1);
});

test('guide vertical scrolling retains the same fully visible code owner and horizontal origin', () => {
  const before = { focused: true, tag: 'PRE', chapter: 'windows', identity: 'example-96-title',
    pageX: 0, pageY: 1000, clientWidth: 288, scrollWidth: 800, scrollLeft: 0,
    clientHeight: 574, scrollHeight: 1048, scrollTop: 0, overflowX: 'auto', overflowY: 'auto',
    documentWidth: 320, documentHeight: 900,
    rect: { left: 16, right: 304, width: 288, top: 100, bottom: 676, height: 576 } };
  const bottom = { ...before, scrollTop: 474 };
  requireGuideMovement(before, bottom, 'down');
  requireGuideMovement(bottom, before, 'up');
  for (const changed of [{ focused: false }, { tag: 'DIV' }, { chapter: 'installation' },
    { identity: 'other' }, { pageX: 1 }, { pageY: 1001 }, { scrollTop: 0 }, { scrollLeft: 1 },
    { overflowY: 'clip' }, { clientHeight: 573 }, { scrollHeight: 1049 },
    { rect: { ...before.rect, top: -1 } }, { rect: { ...before.rect, bottom: 901 } }]) {
    assert.throws(() => requireGuideMovement(before, { ...bottom, ...changed }, 'down'));
  }
  assert.throws(() => requireGuideMovement(bottom, bottom, 'up'));
  assert.throws(() => requireGuideMovement(before, bottom, 'unknown'));
});

test('guide code edges allow fractional scrollTop rounding only while the actual code edge is visible', () => {
  const top = { focused: true, tag: 'PRE', scrollLeft: 0, scrollTop: 0, scrollHeight: 1048, clientHeight: 574,
    client: { top: 101, bottom: 675 }, code: { top: 117, bottom: 1133, height: 1016 } };
  const bottom = { ...top, scrollTop: 474.5, code: { top: -357.5, bottom: 658.5, height: 1016 } };
  requireGuideCodeEdge(top, 'top');
  requireGuideCodeEdge(bottom, 'bottom');
  requireGuideCodeEdge({ ...bottom, scrollTop: 473.5 }, 'bottom');
  for (const changed of [{ focused: false }, { tag: 'DIV' }, { scrollLeft: 1 }, { scrollTop: 473 },
    { scrollTop: 475 }, { scrollTop: 0 }, { scrollHeight: 574 },
    { code: { ...bottom.code, bottom: 675.01 } }, { code: { ...bottom.code, bottom: 101 } },
    { code: { ...bottom.code, height: 0 } }, { code: null }]) {
    assert.throws(() => requireGuideCodeEdge({ ...bottom, ...changed }, 'bottom'));
  }
  for (const changed of [{ scrollTop: 0.5 }, { code: { ...top.code, top: 100.99 } },
    { code: { ...top.code, top: 675 } }]) {
    assert.throws(() => requireGuideCodeEdge({ ...top, ...changed }, 'top'));
  }
  assert.throws(() => requireGuideCodeEdge(top, 'unknown'));
});

test('the actual guide scroll observer retains vertical, client and code geometry through transport', () => {
  const rect = { left: 16, right: 304, width: 288, top: 100, bottom: 676, height: 576 };
  const code = { top: -357.5, bottom: 658.5, height: 1016 };
  const element = { tagName: 'PRE', scrollLeft: 0, scrollTop: 474.5, scrollWidth: 800, clientWidth: 288,
    scrollHeight: 1048, clientHeight: 574, clientTop: 1,
    getBoundingClientRect: () => rect, closest: () => ({ id: 'windows' }),
    getAttribute: name => name === 'aria-labelledby' ? 'example-96-title' : null,
    querySelector: selector => selector === 'code' ? { getBoundingClientRect: () => code } : null };
  const raw = runInNewContext(`(${guideScrollObservation.toString()})(element)`, {
    element, document: { activeElement: element, documentElement: { clientWidth: 320, clientHeight: 900 } },
    scrollX: 0, scrollY: 1000, getComputedStyle: () => ({ overflowX: 'auto', overflowY: 'auto' }),
  }, { timeout: 1000 });
  const observed = JSON.parse(JSON.stringify(raw));
  assert.deepEqual(observed, { focused: true, tag: 'PRE', chapter: 'windows', identity: 'example-96-title',
    scrollLeft: 0, scrollWidth: 800, clientWidth: 288, scrollTop: 474.5, scrollHeight: 1048, clientHeight: 574,
    pageX: 0, pageY: 1000, documentWidth: 320, documentHeight: 900, rect,
    client: { top: 101, bottom: 675 }, code, overflowX: 'auto', overflowY: 'auto' });
  requireGuideCodeEdge(observed, 'bottom');
});

test('guide print code refuses missing, truncated, hidden, height-limited or clipped examples', () => {
  const source = ['drmr --version\n', 'first line\nlast line\n'];
  const values = source.map(code => ({ code, rendered: code, shown: true, maxBlockSize: 'none',
    overflowX: 'visible', overflowY: 'visible', whiteSpace: 'pre-wrap',
    clientWidth: 288, scrollWidth: 288, clientHeight: 100, scrollHeight: 100 }));
  requireGuidePrintCode(values, source);
  assert.throws(() => requireGuidePrintCode(values.slice(1), source));
  assert.throws(() => requireGuidePrintCode([], []));
  for (const changed of [{ code: 'first line\n' }, { rendered: 'first line\n' }, { shown: false },
    { maxBlockSize: '576px' }, { overflowX: 'auto' }, { overflowY: 'clip' }, { whiteSpace: 'pre' },
    { clientWidth: 0 }, { scrollWidth: 289 }, { clientHeight: 0 }, { scrollHeight: 101 }]) {
    assert.throws(() => requireGuidePrintCode([values[0], { ...values[1], ...changed }], source));
  }
});


test('every guide generation input contributes its exact raw hash to source identity', () => {
  const original = identity.builderHashes(path => Buffer.from(path + '\n'));
  assert.deepEqual(Object.keys(original), identity.builderInputs);
  for (const changed of identity.builderInputs) {
    const modified = identity.builderHashes(path => Buffer.from(path + (path === changed ? '\r\n' : '\n')));
    assert.notEqual(modified[changed], original[changed]);
    for (const other of identity.builderInputs.filter(path => path !== changed)) assert.equal(modified[other], original[other]);
  }
});


test('guide code ARIA templates retain complete quoted and multiline code as one exact scalar', () => {
  const code = '{\n  "key": "value: \\"quoted\\"",\n  "path": "C:\\\\tools"\n}';
  const template = codeAriaTemplate(code);
  assert.equal(JSON.parse(template.slice('- code: '.length)), code.trim().replace(/\s+/g, ' '));
  assert.throws(() => codeAriaTemplate('  \n'));
});


test('guide viewport checkpoints reject wrong sources, phases, native owners, clipping and capture movement', () => {
  const expected = { source_head: 'a'.repeat(40), source_tree: 'b'.repeat(40), language: 'en',
    page_asset: 'guide/index.html', page_sha256: 'c'.repeat(64), viewport: { width: 320, height: 900 } };
  const target = { tag: 'PRE', id: '', label: null, href: null, text: 'drmr --version' };
  const observation = { scroll: { focused: true, tag: 'PRE', chapter: 'installation', identity: 'example-1-title',
    scrollLeft: 0, scrollTop: 0, pageX: 0, pageY: 1000 }, focus: { target, active: target, matchesTarget: true,
    rect: { top: 20, bottom: 100, left: 16, right: 304, width: 288, height: 80 },
    viewport: { width: 320, height: 900 }, scroll: { x: 0, y: 1000 }, inViewport: true,
    outlineWidth: 3, outlineStyle: 'solid' } };
  const record = { ...expected, phase: 'native-code-viewport', selected: { tag: 'PRE', chapter: 'installation', identity: 'example-1-title' },
    before: observation, after: structuredClone(observation) };
  requireGuideCheckpoint(record, expected);
  for (const mutate of [row => { row.source_head = 'd'.repeat(40); }, row => { row.page_sha256 = 'd'.repeat(64); },
    row => { row.phase = 'native-table-viewport'; }, row => { row.selected.identity = 'other'; },
    row => { row.viewport.height = 901; }, row => { row.before.focus.viewport.width = 321; }, row => { row.before.focus.rect.bottom = 901; }, row => { row.after.focus.matchesTarget = false; },
    row => { row.after.focus.outlineStyle = 'none'; }, row => { row.after.scroll.scrollLeft = 1; },
    row => { row.after.scroll.scrollTop = 1; },
    row => { row.after.scroll.pageY++; }, row => { row.after.focus.rect.top++; }]) {
    const invalid = structuredClone(record); mutate(invalid);
    assert.throws(() => requireGuideCheckpoint(invalid, expected));
  }
  const vertical = structuredClone(record);
  vertical.phase = 'native-code-vertical-viewport';
  vertical.selected.chapter = 'windows';
  vertical.selected.function_sha256 = 'e'.repeat(64);
  vertical.selected.code_sha256 = 'f'.repeat(64);
  vertical.before.scroll.chapter = vertical.after.scroll.chapter = 'windows';
  requireGuideCheckpoint(vertical, expected);
  for (const mutate of [row => { delete row.selected.function_sha256; }, row => { row.selected.code_sha256 = 'unknown'; },
    row => { row.selected.chapter = row.before.scroll.chapter = row.after.scroll.chapter = 'installation'; },
    row => { row.after.scroll.scrollTop = 1; }, row => { row.after.scroll.identity = 'other'; },
    row => { row.after.focus.rect.bottom = 901; }]) {
    const invalid = structuredClone(vertical); mutate(invalid);
    assert.throws(() => requireGuideCheckpoint(invalid, expected));
  }
});

function guideFigureFixture() {
  const facts = [
    { tag: 'FIGURE', role: null, language: 'sh', labelledBy: 'example-1-title',
      captionCount: 1, captionId: 'example-1-title', captionText: 'Example 1: sh',
      preCount: 1, codeCount: 1, tabindex: '0', code: 'drmr --version\n', rendered: 'drmr --version\n' },
    { tag: 'FIGURE', role: null, language: 'json', labelledBy: 'example-2-title',
      captionCount: 1, captionId: 'example-2-title', captionText: 'Example 2: json',
      preCount: 1, codeCount: 1, tabindex: '0', code: '{\n  "key": "value"\n}\n', rendered: '{\n  "key": "value"\n}\n' },
  ];
  const aria = String.raw`- banner:
  - link "Home"
- main:
  - 'figure "Example 1: sh"':
    - text: "Example 1: sh"
    - code: drmr --version
  - heading "Next section" [level=2]
  - 'figure "Example 2: json"':
    - text: "Example 2: json"
    - code: "{ \"key\": \"value\" }"
- contentinfo:
  - text: End`;
  return { facts, aria };
}

test('guide figures retain every actual ARIA subtree, accessible name and complete code in DOM order', () => {
  const { facts, aria } = guideFigureFixture();
  const groups = requireGuideFigures(facts, aria);
  assert.deepEqual(groups.map(group => group.label), ['Example 1: sh', 'Example 2: json']);
  assert.deepEqual(groups.map(group => group.code), facts.map(figure => figure.code));
  assert.equal(groups[0].aria, '- \'figure "Example 1: sh"\':\n  - text: "Example 1: sh"\n  - code: drmr --version');
  assert.equal(groups[1].language, 'json');
  assert.equal(groups[1].rendered, facts[1].rendered);
});

test('guide figure validation rejects missing, duplicate, reordered, truncated or unknown ARIA observations', () => {
  const { facts, aria } = guideFigureFixture();
  for (const mutate of [
    value => value.replace("  - 'figure \"Example 1: sh\"':\n    - text: \"Example 1: sh\"\n    - code: drmr --version\n", ''),
    value => value.replace('figure "Example 2: json"', 'figure "Example 1: sh"'),
    value => value.replace('figure "Example 1: sh"', 'img "Example 1: sh"'),
    value => value.replace('drmr --version', 'drmr'),
    value => value.replace('    - code: drmr --version', '    - code: drmr --version\n    - link "Unrelated"'),
    value => value.replace('    - text: "Example 1: sh"', '    - text: "Wrong caption"'),
    value => value.replace('    - code: drmr --version', '    - code: "unterminated'),
    value => value + '\n- figure "Unexpected empty figure"',
  ]) assert.throws(() => requireGuideFigures(facts, mutate(aria)));
  assert.throws(() => requireGuideFigures([...facts].reverse(), aria));
  assert.throws(() => requireGuideFigures(facts.slice(1), aria));
  assert.throws(() => requireGuideFigures([], '- main:'));
});

test('guide figure validation still rejects hidden rendered code and incorrect DOM metadata or focusability', () => {
  const { facts, aria } = guideFigureFixture();
  for (const changed of [{ tag: 'PRE' }, { role: 'group' }, { language: 'html' },
    { labelledBy: 'wrong-caption' }, { captionCount: 0 }, { captionId: '' }, { preCount: 2 },
    { codeCount: 2 }, { tabindex: null }, { code: '' }, { rendered: '' }]) {
    assert.throws(() => requireGuideFigures([{ ...facts[0], ...changed }, facts[1]], aria));
  }
});

test('the actual guide DOM readback preserves metadata and full code through transport', () => {
  const { facts, aria } = guideFigureFixture();
  const figures = facts.map(fact => ({ tagName: fact.tag,
    getAttribute: name => ({ role: fact.role, 'data-code-language': fact.language,
      'aria-labelledby': fact.labelledBy })[name] ?? null,
    querySelectorAll: selector => ({
      figcaption: [{ id: fact.captionId, innerText: fact.captionText }],
      pre: [{ getAttribute: name => name === 'tabindex' ? fact.tabindex : null }],
      'pre code': [{ textContent: fact.code, innerText: fact.rendered }],
    })[selector] || [] }));
  const raw = runInNewContext(`(${guideFiguresObservation.toString()})()`, {
    document: { querySelectorAll: selector => selector === 'figure.guide-example' ? figures : [] },
  }, { timeout: 1000 });
  const observed = JSON.parse(JSON.stringify(raw));
  assert.deepEqual(observed, facts);
  assert.equal(requireGuideFigures(observed, aria).length, 2);
});
