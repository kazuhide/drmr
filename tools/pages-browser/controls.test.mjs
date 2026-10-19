import { test } from 'node:test';
import assert from 'node:assert/strict';
import { assets, digest, liveMode, allowedRequest } from './source.mjs';
import { matchesPublication } from './wait-published.mjs';
import { resource } from './server.mjs';
import { requireFocus, requireImages, requireLayout, focusObservation } from './checks.mjs';

test('only the fixed static payload resolves at either mount', () => {
  assert.equal(resource('/').relative, 'index.html');
  assert.equal(resource('/drmr/').relative, 'index.html');
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

test('all eight published raw hashes must match once; stale, missing or duplicate rows fail', () => {
  const hashes = Object.fromEntries(assets.map(path => [path, digest(Buffer.from(path))]));
  const rows = assets.map(path => ({ path, status: 200, sha256: hashes[path] }));
  assert.equal(matchesPublication(rows, hashes), true);
  assert.equal(matchesPublication(rows.slice(1), hashes), false);
  assert.equal(matchesPublication([...rows, rows[0]], hashes), false);
  assert.equal(matchesPublication(rows.map((row, i) => i ? row : { ...row, status: 404 }), hashes), false);
  assert.equal(matchesPublication(rows.map((row, i) => i ? row : { ...row, sha256: digest(Buffer.from('stale')) }), hashes), false);
  assert.notEqual(digest(Buffer.from('line\n')), digest(Buffer.from('line\r\n')));
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
