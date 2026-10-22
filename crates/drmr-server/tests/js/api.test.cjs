'use strict';

const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const test = require('node:test');
const vm = require('node:vm');

const flush = () => new Promise(setImmediate);
function deferred() { let resolve, reject; const promise = new Promise((a, b) => { resolve = a; reject = b; }); return { promise, resolve, reject }; }
function harness(fetch) {
  let now = 0, sequence = 0; const timers = new Map();
  const sandbox = { window: {}, AbortController, performance: { now: () => now }, fetch,
    setTimeout(callback, delay) { const id = ++sequence; timers.set(id, { callback, at: now + delay }); return id; },
    clearTimeout(id) { timers.delete(id); } };
  for (const name of ['api.js']) vm.runInNewContext(fs.readFileSync(path.join(__dirname, '../../static/js/' + name), 'utf8'), sandbox);
  async function advance(milliseconds, dispatchTimers = true) { now += milliseconds; if (!dispatchTimers) { await flush(); return; } for (const [id, timer] of [...timers]) if (timer.at <= now) { timers.delete(id); timer.callback(); } await flush(); }
  return { api: sandbox.window.App.Api, timers, advance, sandbox };
}
for (const stage of ['headers', 'body']) {
  test('the same HTTP deadline aborts unresolved ' + stage + ' even when abort is ignored', async () => {
    const pending = deferred(); let signal;
    const h = harness((_url, options) => { signal = options.signal; return stage === 'headers' ? pending.promise : Promise.resolve({ ok: true, text: () => pending.promise }); });
    let outcome; h.api.fetchJson('/api/common/sessions').catch(error => { outcome = error; }); await flush();
    await h.advance(14999); assert.equal(outcome, undefined);
    await h.advance(1); assert.equal(outcome?.code, 'request-timeout'); assert.equal(signal.aborted, true);
    assert.equal(outcome.uncertain, false); assert.equal(h.timers.size, 0);
    pending.resolve(stage === 'headers' ? { ok: true, text: () => Promise.resolve('[]') } : '[]'); await flush();
    assert.equal(outcome.code, 'request-timeout');
  });
}
test('a timed out mutation is outcome-unknown and is never automatically resent', async () => {
  let calls = 0; const h = harness(() => { calls++; return new Promise(() => {}); });
  let error; h.api.fetchJson('/api/common/jobs', { method: 'POST' }).catch(value => { error = value; }); await flush(); await h.advance(15000);
  assert.equal(error?.uncertain, true); assert.equal(calls, 1);
});
test('known HTTP refusal keeps its status and fixed consumer classification', async () => {
  const h = harness(() => Promise.resolve({ ok: false, status: 409, text: () => Promise.resolve('{"error":"job-busy"}') }));
  await assert.rejects(h.api.fetchJson('/api/common/jobs', { method: 'POST' }), error => error.status === 409 && !error.uncertain && error.body.error === 'job-busy');
  assert.equal(h.timers.size, 0);
});
test('a malformed JSON mutation response is outcome-unknown', async () => {
  const h = harness(() => Promise.resolve({ ok: true, text: () => Promise.resolve('untrusted text') }));
  await assert.rejects(h.api.fetchJson('/api/common/jobs', { method: 'POST' }), error => error.uncertain === true && error.code === 'request-response-invalid');
  assert.equal(h.timers.size, 0);
});

test('a known HTTP refusal retains its status when its body never completes', async () => {
  const h = harness(() => Promise.resolve({ ok: false, status: 409, text: () => new Promise(() => {}) }));
  let outcome; h.api.fetchJson('/api/common/jobs', { method: 'POST' }).catch(error => { outcome = error; }); await flush(); await h.advance(15000);
  assert.equal(outcome.status, 409); assert.equal(outcome.uncertain, false); assert.equal(outcome.body, null);
});
test('an overdue body cannot win when delivery of the deadline timer is delayed', async () => {
  const body = deferred(); let signal, outcome;
  const h = harness((_url, options) => { signal = options.signal; return Promise.resolve({ ok: true, text: () => body.promise }); });
  h.api.fetchJson('/api/common/sessions').then(() => { outcome = 'incorrect success'; }, error => { outcome = error.code; });
  await flush(); await h.advance(15000, false); body.resolve('[]'); await flush();
  assert.equal(outcome, 'request-timeout'); assert.equal(signal.aborted, true); assert.equal(h.timers.size, 0);
});

for (const stage of ['fetch', 'body']) {
  test('a ' + stage + ' I/O failure after a mutation stays unknown without exposing its cause', async () => {
    let calls = 0; const h = harness(() => { calls++; return stage === 'fetch' ? Promise.reject(new Error('private-sentinel')) : Promise.resolve({ ok: true, text: () => Promise.reject(new Error('private-sentinel')) }); });
    await assert.rejects(h.api.fetchJson('/api/common/jobs', { method: 'POST' }), error => error.uncertain === true && error.code === 'network-error' && !String(error).includes('private-sentinel') && error.cause === undefined);
    assert.equal(calls, 1); assert.equal(h.timers.size, 0);
  });
}
