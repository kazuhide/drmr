import { test } from 'node:test';
import assert from 'node:assert/strict';
import { allowedConsoleRequest, requireConsoleCi } from './console-source.mjs';
import { validateLifecycle } from './console-lifecycle.mjs';

test('console browser qualification refuses missing or foreign CI context', () => {
  const valid = { CI: 'true', GITHUB_ACTIONS: 'true', RUNNER_OS: 'Linux', GITHUB_SHA: 'a'.repeat(40),
    PW_REPORT_ROOT: '/evidence', DRMR_CONSOLE_BINARY: '/binary', DRMR_CONSOLE_BUILD: '/build' };
  assert.doesNotThrow(() => requireConsoleCi(valid));
  for (const name of Object.keys(valid)) {
    const missing = { ...valid }; delete missing[name];
    assert.throws(() => requireConsoleCi(missing));
  }
  for (const [name, value] of [['CI', 'false'], ['GITHUB_ACTIONS', 'false'], ['RUNNER_OS', 'macOS'], ['GITHUB_SHA', 'a'.repeat(39)]]) {
    assert.throws(() => requireConsoleCi({ ...valid, [name]: value }));
  }
});

test('console requests stay on their two owned exact loopback origins', () => {
  for (const url of ['http://127.0.0.1:4174/', 'http://127.0.0.1:4174/api/common/files?role=media', 'http://127.0.0.1:4175/media.m3u8']) {
    assert.equal(allowedConsoleRequest(url), true);
  }
  for (const url of ['http://localhost:4174/', 'http://127.0.0.1:4176/', 'https://127.0.0.1:4174/',
    'http://name@127.0.0.1:4174/', 'http://127.0.0.1:4175@example.invalid/', 'https://example.invalid/', 'file:///tmp/media.mp4']) {
    assert.equal(allowedConsoleRequest(url), false, url);
  }
});

test('console lifecycle requires bound successful exit, owned cleanup and authenticated media requests', () => {
  const identity = { sourceHead: 'a'.repeat(40), sourceTree: 'b'.repeat(40), binarySha256: 'c'.repeat(64),
    buildSha256: 'd'.repeat(64), buildProfile: { opt_level: '0' }, assetHashes: { 'index.html': 'e'.repeat(64) }, fixtureSha256: 'f'.repeat(64) };
  const good = { ...identity, passed: true, status: { code: 0, signal: null }, serverReaped: true,
    ownedTemporaryRemoved: true, fixtureRequests: [{ resource: 'media.m3u8', authenticated: true }, { resource: 'segment-000.ts', authenticated: true }] };
  assert.doesNotThrow(() => validateLifecycle(good, identity));
  for (const patch of [{ passed: false }, { status: { code: 1, signal: null } },
    { status: { code: null, signal: 'SIGTERM' } }, { serverReaped: false }, { ownedTemporaryRemoved: false },
    { sourceHead: '0'.repeat(40) }, { binarySha256: '0'.repeat(64) }, { buildSha256: '0'.repeat(64) },
    { assetHashes: {} }, { fixtureRequests: [] },
    { fixtureRequests: [{ resource: 'media.m3u8', authenticated: true }] },
    { fixtureRequests: [{ resource: 'media.m3u8', authenticated: true }, { resource: 'segment-000.ts', authenticated: false }] },
    { fixtureRequests: [{ resource: 'media.m3u8', authenticated: true }, { resource: '../private.json', authenticated: true }] }]) {
    assert.throws(() => validateLifecycle({ ...good, ...patch }, identity));
  }
  for (const key of Object.keys(good)) {
    const missing = { ...good }; delete missing[key];
    assert.throws(() => validateLifecycle(missing, identity), key);
  }
});
