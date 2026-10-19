import { locales } from './locales.mjs';
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { assets, assetRoute, assetPath, digest, liveMode, allowedRequest } from './source.mjs';
import { matchesPublication } from './wait-published.mjs';
import { resource } from './server.mjs';
import { requireFocus, requireImages, requireLayout, focusObservation, requireClippedFocusTimeout, pollFocus, FocusConditionTimeout, FocusObservationDeadline } from './checks.mjs';

test('only the fixed static payload resolves at either mount', () => {
  assert.equal(resource('/').relative, 'index.html');
  assert.equal(resource('/drmr/').relative, 'index.html');
  for (const path of ['/ja/', '/ja/index.html', '/drmr/ja/', '/drmr/ja/index.html']) {
    assert.equal(resource(path).relative, 'ja/index.html');
    assert.equal(assetPath('http://127.0.0.1:4173' + path), 'ja/index.html');
  }
  assert.equal(assetRoute('ja/index.html'), 'ja/');
  assert.equal(assets.length, 9);
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

test('all nine published raw hashes must match once; stale, missing or duplicate rows fail', () => {
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
