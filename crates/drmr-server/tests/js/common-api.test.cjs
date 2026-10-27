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
  for (const kind of ['url']) {
    const session = envelope({ session_id: 'session_1', creation_request_id: 'a'.repeat(32),
      source: { kind, label: 'Source URL' }, plan_refs: [], request_ids: [], requests_used: 0, active_request_id: null });
    const api = load(url => Promise.resolve(url === '/api/common/sessions' ? envelope({ sessions: [session] }) : session));
    assert.equal((await api.session('session_1')).session_id, 'session_1');
    assert.equal((await api.sessions()).sessions[0].source.label, 'Source URL');
  }
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
test('current saved plans require explicit coverage while legacy versions remain readable', async () => {
  for (const schema of [1, 2, 3, 4]) {
    const single = schema === 1 || schema === 3;
    const plan = envelope({ plan_ref: 'plan_1', session_id: 'session_1', plan_id: 'b'.repeat(64), plan_schema_version: schema,
      outputs: single ? [{ id: null, format: 'mp4', target: 'clear' }] : [{ id: 'clear', format: 'mp4', target: 'clear', drms: [], conformance: 'none' }],
      destination_state: 'absent', export_url: '/api/common/plans/plan_1/export' });
    if (schema >= 3) plan.required_depth = 'native';
    const api = load(() => Promise.resolve(plan));
    assert.equal((await api.plan('plan_1')).plan_schema_version, schema);
    if (schema >= 3) {
      delete plan.required_depth;
      await assert.rejects(api.plan('plan_1'), { code: 'request-response-invalid' });
    }
  }
});

test('a retired HLS URL session response cannot be accepted as the common source contract', async () => {
  const response = envelope({ session_id: 'session_1', creation_request_id: 'a'.repeat(32), source: { kind: 'hls-url', label: 'Source URL' }, plan_refs: [], request_ids: [], requests_used: 0, active_request_id: null });
  const api = load(() => Promise.resolve(response));
  await assert.rejects(api.session('session_1'), { code: 'request-response-invalid' });
});

test('saved schemas five and six require complete fixed processing selections', async () => {
  for (const schema of [5, 6]) {
    const selection = { requested: 'auto', resolved: 'stitch', boundaries: 'resegment', segment_duration_ms: 2000 };
    const plan = envelope({ plan_ref: 'plan_1', session_id: 'session_1', plan_id: 'b'.repeat(64), plan_schema_version: schema,
      required_depth: 'native', outputs: [{ id: schema === 5 ? null : 'hls', format: 'hls', target: 'clear', drms: [], conformance: 'none' }],
      processing: schema === 5 ? selection : [{ id: 'hls', selection }],
      destination_state: 'absent', export_url: '/api/common/plans/plan_1/export' });
    assert.equal((await load(() => Promise.resolve(plan)).plan('plan_1')).plan_schema_version, schema);
    for (const field of ['requested', 'resolved', 'boundaries', 'segment_duration_ms']) {
      const changed = structuredClone(plan), item = schema === 5 ? changed.processing : changed.processing[0].selection;
      item[field] = 'PRIVATE_PROCESSING';
      await assert.rejects(load(() => Promise.resolve(changed)).plan('plan_1'), { code: 'request-response-invalid' });
    }
    const missing = structuredClone(plan); delete missing.processing;
    await assert.rejects(load(() => Promise.resolve(missing)).plan('plan_1'), { code: 'request-response-invalid' });
    if (schema === 6) {
      const duplicate = structuredClone(plan); duplicate.processing.push(duplicate.processing[0]);
      await assert.rejects(load(() => Promise.resolve(duplicate)).plan('plan_1'), { code: 'request-response-invalid' });
      const different = structuredClone(plan); different.processing[0].id = 'other';
      await assert.rejects(load(() => Promise.resolve(different)).plan('plan_1'), { code: 'request-response-invalid' });
    }
  }
});

test('saved segment targets retain positive bounded milliseconds rather than a fixed two seconds', async () => {
  for (const schema of [5, 6]) {
    const selection = { requested: 'auto', resolved: 'stitch', boundaries: 'resegment', segment_duration_ms: 6000 };
    const plan = envelope({ plan_ref: 'plan_1', session_id: 'session_1', plan_id: 'b'.repeat(64), plan_schema_version: schema,
      required_depth: 'native', outputs: [{ id: schema === 5 ? null : 'hls', format: 'hls', target: 'clear', drms: [], conformance: 'none' }],
      processing: schema === 5 ? selection : [{ id: 'hls', selection }],
      destination_state: 'absent', export_url: '/api/common/plans/plan_1/export' });
    for (const duration of [1, 2000, 6000, 4294967295]) {
      selection.segment_duration_ms = duration;
      await load(() => Promise.resolve(plan)).plan('plan_1');
    }
    for (const duration of [null, 0, -1, 1.5, 4294967296, '6000', Infinity, NaN]) {
      selection.segment_duration_ms = duration;
      await assert.rejects(load(() => Promise.resolve(plan)).plan('plan_1'), { code: 'request-response-invalid' });
    }
  }
});

function outputPlan(schema, mode = 'template', format = 'dash') {
  const single = schema === 7 || schema === 9;
  const selection = { requested: 'auto', resolved: 'stitch', boundaries: format === 'mp4' ? 'not-applicable' : 'resegment', segment_duration_ms: format === 'mp4' ? null : 6000 };
  const output = { id: single ? null : 'output', format, target: 'clear' };
  if (!single) Object.assign(output, { drms: [], conformance: 'none', processing: 'auto', ...(format === 'dash' ? { dash_addressing: mode } : {}) });
  const options = { layout: format === 'mp4' ? 'not-applicable' : 'source', dash_addressing: format === 'dash' ? mode : null };
  return envelope({ plan_ref: 'plan_1', session_id: 'session_1', plan_id: 'b'.repeat(64), plan_schema_version: schema,
    required_depth: 'native', outputs: [output], processing: single ? selection : [{ id: 'output', selection }],
    output_options: single ? options : [{ id: 'output', options }], destination_state: 'absent', export_url: '/api/common/plans/plan_1/export' });
}

test('new saved plans expose closed path-free placement and four DASH addressing forms', async () => {
  for (const schema of [7, 8, 9, 10]) {
    for (const mode of ['template', 'template-timeline', 'list', 'base']) {
      const response = outputPlan(schema, mode);
      assert.equal((await load(() => Promise.resolve(response)).plan('plan_1')).plan_schema_version, schema);
      const options = schema === 7 || schema === 9 ? response.output_options : response.output_options[0].options;
      options.layout = 'previous-saved';
      await load(() => Promise.resolve(response)).plan('plan_1');
    }
    for (const format of ['mp4', 'hls', 'hls-ts']) {
      const response = outputPlan(schema, 'template', format);
      await load(() => Promise.resolve(response)).plan('plan_1');
    }
  }
});

test('new saved options reject paths, unknown fields, missing choices and foreign output bindings', async () => {
  for (const schema of [7, 8, 9, 10]) {
    for (const change of [
      options => { options.layout = { mode: 'source', tracks: [{ source: '/private/media' }] }; },
      options => { options.layout = 'normalized'; },
      options => { options.layout = 'not-applicable'; },
      options => { options.dash_addressing = 'template-duration'; },
      options => { options.dash_addressing = 'https://private.invalid/key'; },
      options => { options.dash_addressing = null; },
      options => { delete options.dash_addressing; },
      options => { options.tracks = [{ source: '/private/media' }]; },
    ]) {
      const response = outputPlan(schema);
      change(schema === 7 || schema === 9 ? response.output_options : response.output_options[0].options);
      await assert.rejects(load(() => Promise.resolve(response)).plan('plan_1'), { code: 'request-response-invalid' });
    }
    for (const invalid of [null, {}, [], { layout: 'source' }]) {
      const response = outputPlan(schema); response.output_options = invalid;
      await assert.rejects(load(() => Promise.resolve(response)).plan('plan_1'), { code: 'request-response-invalid' });
    }
    const missing = outputPlan(schema); delete missing.output_options;
    await assert.rejects(load(() => Promise.resolve(missing)).plan('plan_1'), { code: 'request-response-invalid' });
  }
  for (const change of [
    response => { response.output_options[0].id = 'other'; },
    response => { response.output_options.push(response.output_options[0]); },
    response => { response.outputs[0].dash_addressing = 'list'; },
  ]) {
    const response = outputPlan(8); change(response);
    await assert.rejects(load(() => Promise.resolve(response)).plan('plan_1'), { code: 'request-response-invalid' });
  }
  for (const format of ['mp4', 'hls']) {
    const response = outputPlan(8, 'template', format);
    response.output_options[0].options.dash_addressing = 'base';
    await assert.rejects(load(() => Promise.resolve(response)).plan('plan_1'), { code: 'request-response-invalid' });
  }
});
