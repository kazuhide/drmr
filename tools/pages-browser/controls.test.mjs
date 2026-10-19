import { test } from 'node:test';
import assert from 'node:assert/strict';
import { assets, digest, liveMode, allowedRequest } from './source.mjs';
import { matchesPublication } from './wait-published.mjs';
import { resource } from './server.mjs';
import { requireFocus, requireImages, requireLayout } from './checks.mjs';

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
  const visible = { width: 100, height: 32, inViewport: true, outlineWidth: 3, outlineStyle: 'solid' };
  requireFocus(visible);
  for (const changed of [{ width: 0 }, { inViewport: false }, { outlineWidth: 0 }, { outlineStyle: 'none' }]) {
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
