'use strict';
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const test = require('node:test');
const vm = require('node:vm');
function load(fetchJson) {
  const sandbox = { window: { App: { Api: { fetchJson } } }, URLSearchParams };
  vm.runInNewContext(fs.readFileSync(path.join(__dirname, '../../static/js/common-api.js'), 'utf8'), sandbox);
  return sandbox.window.App.CommonApi;
}
const envelope = body => ({ schema_version: 1, server_id: 'server_1', ...body });
const page = overrides => envelope({ root_ref: 'root_1', role: 'media', directory_ref: null, output_parent_ref: null, entries: [], next_cursor: null, ...overrides });
test('folder selection uses the separately issued output parent, including the root page', async () => {
  let request;
  const api = load((url, options) => { request = { url, options }; return Promise.resolve(page({ role: 'output-parent', output_parent_ref: 'parent_1' })); });
  const result = await api.files({ root_ref: 'root_1', role: 'output-parent' });
  assert.equal(result.output_parent_ref, 'parent_1');
  assert.equal(request.url, '/api/common/files?root_ref=root_1&role=output-parent&limit=100');
  assert.equal(request.options, undefined);
});
test('a role cannot borrow a directory or root reference as its output parent', async () => {
  for (const response of [page({ output_parent_ref: 'parent_1' }), page({ role: 'output-parent' })]) {
    const api = load(() => Promise.resolve(response));
    await assert.rejects(api.files({ root_ref: 'root_1', role: 'media' }), { code: 'request-response-invalid' });
  }
});
test('a malformed successful mutation keeps the request outcome unknown', async () => {
  const api = load(() => Promise.resolve(envelope({ session_id: 'session_1' })));
  await assert.rejects(api.createSession({}), error => error.code === 'request-response-invalid' && error.uncertain === true);
});
test('URL sessions use a fixed label for creation recovery and every listing', async () => {
  const session = envelope({ session_id: 'session_1', creation_request_id: 'a'.repeat(32),
    source: { kind: 'hls-url', label: 'HLS URL' }, plan_refs: [], request_ids: [], requests_used: 0, active_request_id: null });
  const api = load(url => Promise.resolve(url === '/api/common/sessions' ? envelope({ sessions: [session] }) : session));
  assert.equal((await api.session('session_1')).session_id, 'session_1');
  assert.equal((await api.sessions()).sessions[0].source.label, 'HLS URL');
});
test('common references are encoded opaque tokens and never arbitrary URLs or paths', async () => {
  let calls = 0;
  const api = load(() => { calls++; return Promise.resolve(null); });
  for (const ref of ['../key', '/tmp/key', 'https://other.invalid/key', '', '__proto__']) {
    await assert.rejects(api.job(ref), { code: 'invalid-request' });
  }
  assert.equal(calls, 0);
});
test('Stop sends only an empty object to the exact common job endpoint', async () => {
  let request;
  const job = envelope({ job_id: 'job_1', session_id: 'session_1', request_id: 'a'.repeat(32), operation: 'analyze', revision: 0, state: 'running', stop_requested: true, supervision_confirmed: false, cleanup_confirmed: false, exit_code: null, source_ref: null, analysis_ref: null, plan_ref: null, plan_id: null, report_ref: null, progress: null, error: null });
  const api = load((url, options) => { request = { url, options }; return Promise.resolve(job); });
  await api.stop('job_1');
  assert.equal(request.url, '/api/common/jobs/job_1/stop');
  assert.equal(request.options.method, 'POST');
  assert.equal(request.options.body, '{}');
});
test('export links must be the exact same-origin attachment endpoint', async () => {
  const plan = envelope({ plan_ref: 'plan_1', session_id: 'session_1', plan_id: 'b'.repeat(64), plan_schema_version: 2, outputs: [{ id: 'clear', format: 'mp4', target: 'clear', drms: [], conformance: 'none' }], destination_state: 'absent', export_url: 'https://other.invalid/export' });
  const api = load(() => Promise.resolve(plan));
  await assert.rejects(api.plan('plan_1'), { code: 'request-response-invalid' });
});
