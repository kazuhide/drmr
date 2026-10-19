import { locales } from './locales.mjs';
import { matrixRows, matrixColumns, matrixObservation, requireMatrixLayout, requireMatrixMovement, completeMatrixKey, MatrixKeyDeadline } from './matrix.mjs';
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { assets, assetRoute, assetPath, digest, liveMode, allowedRequest } from './source.mjs';
import { matchesPublication } from './wait-published.mjs';
import { resource } from './server.mjs';
import { requireFocus, requireImages, requireLayout, layoutObservation, focusObservation, requireClippedFocusTimeout, partialFocusCandidate, pollFocus, FocusConditionTimeout, FocusObservationDeadline } from './checks.mjs';

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
  assert.deepEqual(matrixRows[0].output, [['Clear', 'cenc', 'cbcs'], ['Clear', 'cenc', 'cbcs'], ['Clear', 'cbcs', 'AES-128'], []]);
  assert.deepEqual(matrixRows[3].output.slice(0, 2), [[], ['Clear', 'cenc', 'cbcs']]);
  assert.deepEqual(matrixRows[4].output, [[], ['Clear', 'cenc', 'cbcs'], [], ['Clear', 'AES-128', 'SAMPLE-AES']]);
  for (const index of [0, 1, 2]) assert.deepEqual(matrixRows[index].output[3], []);
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
