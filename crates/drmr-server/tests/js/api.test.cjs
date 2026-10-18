'use strict';

const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const test = require('node:test');
const vm = require('node:vm');

test('stop uses the selected encoded job ID and a JSON object with no process parameters', async () => {
  const calls = [];
  const sandbox = { window: {}, AbortController, performance, setTimeout, clearTimeout, fetch: (url, options) => {
    calls.push({ url, options });
    return Promise.resolve({ ok: true, text: () => Promise.resolve('{"state":"running","stop_requested":true}') });
  } };
  vm.runInNewContext(fs.readFileSync(path.join(__dirname, '../../static/js/api.js'), 'utf8'), sandbox);
  for (const id of ['job', '__proto__', 'a/b?x#']) {
    assert.equal((await sandbox.window.App.Api.stopJob(id)).state, 'running');
    const { url, options } = calls.at(-1);
    assert.equal(url, '/api/jobs/' + encodeURIComponent(id) + '/stop');
    assert.equal(options.method, 'POST');
    assert.equal(options.headers['Content-Type'], 'application/json');
    assert.equal(options.body, '{}');
  }
});

const flush = () => new Promise(setImmediate);
function deferred() { let resolve, reject; const promise = new Promise((a, b) => { resolve = a; reject = b; }); return { promise, resolve, reject }; }
function harness(fetch) {
  let now = 0, sequence = 0; const timers = new Map();
  const sandbox = { window: {}, AbortController, performance: { now: () => now }, fetch,
    setTimeout(callback, delay) { const id = ++sequence; timers.set(id, { callback, at: now + delay }); return id; },
    clearTimeout(id) { timers.delete(id); } };
  for (const name of ['api.js', 'jobs.js']) vm.runInNewContext(fs.readFileSync(path.join(__dirname, '../../static/js/' + name), 'utf8'), sandbox);
  async function advance(milliseconds, dispatchTimers = true) { now += milliseconds; if (!dispatchTimers) { await flush(); return; } for (const [id, timer] of [...timers]) if (timer.at <= now) { timers.delete(id); timer.callback(); } await flush(); }
  return { api: sandbox.window.App.Api, jobs: sandbox.window.App.Jobs, timers, advance, sandbox };
}
for (const stage of ['headers', 'body']) {
  test('the same HTTP deadline aborts unresolved ' + stage + ' even when abort is ignored', async () => {
    const pending = deferred(); let signal;
    const h = harness((_url, options) => { signal = options.signal; return stage === 'headers' ? pending.promise : Promise.resolve({ ok: true, text: () => pending.promise }); });
    let outcome; h.api.getCatalog().catch(error => { outcome = error; }); await flush();
    await h.advance(14999); assert.equal(outcome, undefined);
    await h.advance(1); assert.equal(outcome?.code, 'request-timeout'); assert.equal(signal.aborted, true);
    assert.equal(outcome.uncertain, false); assert.equal(h.timers.size, 0);
    pending.resolve(stage === 'headers' ? { ok: true, text: () => Promise.resolve('[]') } : '[]'); await flush();
    assert.equal(outcome.code, 'request-timeout');
  });
}
test('a timed out mutation is outcome-unknown and is never automatically resent', async () => {
  let calls = 0; const h = harness(() => { calls++; return new Promise(() => {}); });
  let error; h.api.postConvert('asset').catch(value => { error = value; }); await flush(); await h.advance(15000);
  assert.equal(error?.uncertain, true); assert.equal(calls, 1);
});
test('known HTTP refusal keeps its status and fixed consumer classification', async () => {
  const h = harness(() => Promise.resolve({ ok: false, status: 409, text: () => Promise.resolve('{"error":"job-busy"}') }));
  await assert.rejects(h.api.postConvert('asset'), error => error.status === 409 && !error.uncertain && error.body.error === 'job-busy');
  assert.equal(h.timers.size, 0);
});
for (const body of ['untrusted text', '[]', 'null', '{}', '{"job_id":7}']) {
  test('a malformed successful launch body is outcome-unknown: ' + body, async () => {
    const h = harness(() => Promise.resolve({ ok: true, text: () => Promise.resolve(body) }));
    await assert.rejects(h.api.postConvert('asset'), error => error.uncertain === true && error.code === 'request-response-invalid');
    assert.equal(h.timers.size, 0);
  });
}
test('SSE loss followed by stalled polling reaches the bounded failure path', async () => {
  let eventSource, requests = 0; const lost = [], waiting = [];
  const h = harness(() => { requests++; return new Promise(() => {}); });
  h.sandbox.EventSource = class { constructor() { eventSource = this; } addEventListener() {} close() {} };
  h.jobs.watch('job', { onLine() {}, onTerminal() { assert.fail('unknown is not terminal'); }, onLost: reason => lost.push(reason), onWaiting: reason => waiting.push(reason) });
  eventSource.onerror(); await flush();
  for (let i = 0; i < 30; i++) { await h.advance(15000); if (i < 29) await h.advance(2000); }
  assert.equal(requests, 30); assert.deepEqual(lost, ['jobUnreachable']); assert.ok(waiting.includes('serverRetrying'));
});

test('a known HTTP refusal retains its status when its body never completes', async () => {
  const h = harness(() => Promise.resolve({ ok: false, status: 409, text: () => new Promise(() => {}) }));
  let outcome; h.api.postConvert('asset').catch(error => { outcome = error; }); await flush(); await h.advance(15000);
  assert.equal(outcome.status, 409); assert.equal(outcome.uncertain, false); assert.equal(outcome.body, null);
});
test('malformed successful Stop and status bodies cannot confirm a job outcome', async () => {
  for (const body of ['{}', '[]', '{"state":"made-up"}']) {
    const h = harness(() => Promise.resolve({ ok: true, text: () => Promise.resolve(body) }));
    await assert.rejects(h.api.stopJob('job'), error => error.uncertain === true);
    await assert.rejects(h.api.getJobStatus('job'), error => error.code === 'request-response-invalid' && error.uncertain === false);
  }
});

test('an overdue body cannot win when delivery of the deadline timer is delayed', async () => {
  const body = deferred(); let signal, outcome;
  const h = harness((_url, options) => { signal = options.signal; return Promise.resolve({ ok: true, text: () => body.promise }); });
  h.api.getCatalog().then(() => { outcome = 'incorrect success'; }, error => { outcome = error.code; });
  await flush(); await h.advance(15000, false); body.resolve('[]'); await flush();
  assert.equal(outcome, 'request-timeout'); assert.equal(signal.aborted, true); assert.equal(h.timers.size, 0);
});

for (const stage of ['fetch', 'body']) {
  test('a ' + stage + ' I/O failure after a mutation stays unknown without exposing its cause', async () => {
    let calls = 0; const h = harness(() => { calls++; return stage === 'fetch' ? Promise.reject(new Error('private-sentinel')) : Promise.resolve({ ok: true, text: () => Promise.reject(new Error('private-sentinel')) }); });
    await assert.rejects(h.api.postConvert('asset'), error => error.uncertain === true && error.code === 'network-error' && !String(error).includes('private-sentinel') && error.cause === undefined);
    assert.equal(calls, 1); assert.equal(h.timers.size, 0);
  });
}
