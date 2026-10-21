'use strict';
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const test = require('node:test');
const vm = require('node:vm');
const flush = () => new Promise(setImmediate);
const sid = 'session_1', rid = 'a'.repeat(32), pid = 'b'.repeat(64);
const env = body => ({ schema_version: 1, server_id: 'server_1', ...body });
const session = overrides => env({ session_id: sid, creation_request_id: 'c'.repeat(32), source: { kind: 'local', label: 'input.mp4' }, plan_refs: [], request_ids: [], requests_used: 0, active_request_id: null, ...overrides });
const caps = () => env({ roots: [], limits: { sessions: 8, requests_per_session: 32, max_outputs: 8 }, worker_policy: { maximum: 64, automatic: true }, execution: { availability: 'idle', job_id: null, owner: null }, observation: 'poll' });
const job = overrides => env({ job_id: 'job_1', session_id: sid, request_id: rid, operation: 'analyze', revision: 0, state: 'running', stop_requested: false, supervision_confirmed: false, cleanup_confirmed: false, exit_code: null, source_ref: null, analysis_ref: null, plan_ref: null, plan_id: null, report_ref: null, progress: null, error: null, ...overrides });
const unknown = () => Object.assign(new Error('request-timeout'), { code: 'request-timeout', uncertain: true });
const refused = (status, code) => Object.assign(new Error(code), { status, body: env({ error: { code, hint: 'Fixed refusal.', last_activity: null, job_id: null } }) });
function deferred() { let resolve; const promise = new Promise(r => { resolve = r; }); return { promise, resolve }; }
async function harness(overrides = {}) {
  const calls = [];
  const api = {
    capabilities: async () => caps(), sessions: async () => env({ sessions: [session()] }),
    session: async () => session(), close: async () => null,
    submit: async body => { calls.push(body); return job(); },
    receipt: async () => { throw refused(404, 'request-not-found'); },
    job: async () => job(), ...overrides
  };
  const sandbox = { window: { App: { CommonApi: api } }, Map, crypto: { getRandomValues: array => array.fill(10) } };
  for (const file of ['common-report.js', 'common-state.js']) vm.runInNewContext(fs.readFileSync(path.join(__dirname, '../../static/js', file), 'utf8'), sandbox);
  const state = sandbox.window.App.CommonState.create({ api, newId: () => rid });
  await state.refresh(); await state.select(sid);
  return { state, calls, api };
}
test('a lost launch fixes its ID before POST and recovers with GET only; live 404 retains ownership', async () => {
  let posts = 0, gets = 0, state;
  const h = await harness({ submit: async body => { posts++; assert.equal(state.snapshot().owner.requestId, body.request_id); throw unknown(); },
    receipt: async (_sid, id) => { gets++; assert.equal(id, rid); throw refused(404, 'request-not-found'); } });
  state = h.state;
  await state.submit({ kind: 'analyze' }, { keys_ref: null, max_workers: null });
  const owner = state.snapshot().owner;
  assert.equal(owner.phase, 'unknown');
  await state.recover(); await state.recover();
  assert.equal(state.snapshot().owner, owner); assert.equal(posts, 1); assert.equal(gets, 2);
  assert.equal(state.canStart(), false);
});
test('an accepted receipt binds only its exact session, request and operation', async () => {
  const h = await harness({ submit: async () => { throw unknown(); },
    receipt: async () => env({ session_id: sid, request_id: rid, operation: 'plan', state: 'accepted', job_id: 'other_job', error: null }) });
  await h.state.submit({ kind: 'analyze' }, { keys_ref: null, max_workers: null });
  await h.state.recover();
  assert.equal(h.state.snapshot().owner.phase, 'unknown');
  assert.equal(h.state.snapshot().owner.jobId, null);
});
test('explicit same-server session close fences a delayed launch and never resubmits it', async () => {
  const pending = deferred(); let posts = 0;
  const h = await harness({ submit: () => { posts++; return pending.promise; } });
  const launching = h.state.submit({ kind: 'analyze' }, { keys_ref: null, max_workers: null });
  await h.state.close(sid);
  pending.resolve(job()); await launching;
  assert.equal(h.state.snapshot().owner, null); assert.equal(h.state.snapshot().sessions.has(sid), false);
  assert.equal(posts, 1);
});
test('busy close and cleanup-unconfirmed observations retain the same owner', async () => {
  const h = await harness({ close: async () => { throw refused(409, 'session-busy'); }, job: async () => job({ revision: 1, error: { code: 'cleanup-unconfirmed' } }) });
  await h.state.submit({ kind: 'analyze' }, { keys_ref: null, max_workers: null });
  const owner = h.state.snapshot().owner;
  await h.state.close(sid); await h.state.recover();
  assert.equal(h.state.snapshot().owner, owner); assert.equal(h.state.canStart(), false);
});
test('selection changes and stale revisions cannot release the global owner', async () => {
  const h = await harness({ sessions: async () => env({ sessions: [session(), session({ session_id: 'session_2', creation_request_id: 'd'.repeat(32) })] }),
    submit: async () => job({ revision: 2 }), job: async () => job({ revision: 1, state: 'succeeded', cleanup_confirmed: true, supervision_confirmed: true, exit_code: 0 }) });
  await h.state.submit({ kind: 'analyze' }, { keys_ref: null, max_workers: null });
  const owner = h.state.snapshot().owner;
  await h.state.select('session_2'); await h.state.recover();
  assert.equal(h.state.snapshot().selectedId, 'session_2'); assert.equal(h.state.snapshot().owner, owner);
  assert.equal(owner.revision, 2); assert.equal(h.state.canStart(), false);
});
test('unknown Stop is sent once and follows only the bound job via GET', async () => {
  let stops = 0, gets = 0;
  const h = await harness({ stop: async id => { stops++; assert.equal(id, 'job_1'); throw unknown(); },
    job: async () => { gets++; return job({ revision: gets, stop_requested: true }); } });
  await h.state.submit({ kind: 'analyze' }, { keys_ref: null, max_workers: null });
  await h.state.stop(); await h.state.stop();
  assert.equal(stops, 1); assert.ok(gets >= 1); assert.ok(h.state.snapshot().owner);
});
test('a failed independent verification retains its negative report and cannot become success', async () => {
  const plan = env({ plan_ref: 'plan_1', session_id: sid, plan_id: pid, plan_schema_version: 2, outputs: [{ id: 'clear', format: 'mp4', target: 'clear', drms: [], conformance: 'none' }], destination_state: 'present', export_url: '/api/common/plans/plan_1/export' });
  const h = await harness({ sessions: async () => env({ sessions: [session({ source: { kind: 'saved-plan', label: 'plan.json' }, plan_refs: ['plan_1'] })] }),
    plan: async () => plan,
    submit: async () => job({ operation: 'verify', state: 'failed', revision: 1, supervision_confirmed: true, cleanup_confirmed: true, exit_code: 1, plan_ref: 'plan_1', plan_id: pid, report_ref: 'report_1' }),
    report: async () => env({ report_ref: 'report_1', session_id: sid, job_id: 'job_1', request_id: rid, operation: 'verify', analysis_ref: null, plan_ref: 'plan_1', plan_id: pid, report_kind: 'verification', report_schema_version: 1, report: { schema_version: 1, plan_id: pid, passed: false, source_unchanged: true, outputs: [] } }) });
  await h.state.submit({ kind: 'verify', plan_ref: 'plan_1' }, { keys_ref: null, max_workers: null });
  const record = h.state.selected();
  assert.equal(h.state.snapshot().owner, null); assert.equal(record.outcome, 'failed');
  assert.equal(record.report.report.passed, false);
});
test('lost session creation uses its original correlation ID and never retains a secret URL', async () => {
  let posts = 0;
  const h = await harness({ createSession: async body => { posts++; assert.equal(body.creation_request_id, rid); throw unknown(); },
    creation: async () => { throw refused(404, 'session-request-not-found'); } });
  await h.state.createSession({ kind: 'hls-url', url: 'https://example.invalid/list?secret=sentinel', fetch_policy_ref: null });
  await h.state.recoverCreation();
  assert.equal(posts, 1); assert.equal(h.state.snapshot().creation.phase, 'unknown');
  assert.doesNotMatch(JSON.stringify(h.state.snapshot().creation), /sentinel|https:/);
});
test('a full 32-request session permits export and verify-only reimport guidance, but no new POST', async () => {
  const h = await harness({ sessions: async () => env({ sessions: [session({ requests_used: 32 })] }) });
  assert.equal(h.state.canStart(), false);
  await h.state.submit({ kind: 'analyze' }, { keys_ref: null, max_workers: null });
  assert.equal(h.calls.length, 0); assert.equal(h.state.selected().error.code, 'request-limit-reached');
});

test('a missing bound result can be fetched again with GET after confirmed process cleanup', async () => {
  let reports = 0, posts = 0;
  const terminal = job({ revision: 1, state: 'succeeded', supervision_confirmed: true, cleanup_confirmed: true, exit_code: 0, analysis_ref: 'analysis_1', report_ref: 'report_1' });
  const h = await harness({ submit: async () => { posts++; return terminal; }, job: async () => terminal,
    analysis: async () => env({ analysis_ref: 'analysis_1', session_id: sid, job_id: 'job_1', source_ref: 'source_1', report_ref: 'report_1' }),
    report: async () => { reports++; if (reports === 1) throw refused(404, 'artifact-unavailable'); return env({ report_ref: 'report_1', session_id: sid, job_id: 'job_1', request_id: rid, operation: 'analyze', analysis_ref: 'analysis_1', plan_ref: null, plan_id: null, report_kind: 'analysis', report_schema_version: 1, report: { schema_version: 1, input_admitted: true, input_checks: { stream_metadata: 'pass', packet_payloads_equal: true, packet_timing_equal: true, decoded_content_equal: true } } }); } });
  await h.state.submit({ kind: 'analyze' }, { keys_ref: null, max_workers: null });
  assert.equal(h.state.selected().outcome, 'unavailable'); assert.equal(h.state.snapshot().owner, null);
  await h.state.recover();
  assert.equal(h.state.selected().outcome, 'succeeded'); assert.equal(reports, 2); assert.equal(posts, 1);
});
test('an explicit close returning same-server missing-session authority still fences delayed admission', async () => {
  const h = await harness({ submit: async () => { throw unknown(); }, close: async () => { throw refused(404, 'session-not-found'); } });
  await h.state.submit({ kind: 'analyze' }, { keys_ref: null, max_workers: null });
  await h.state.close(sid);
  assert.equal(h.state.snapshot().owner, null); assert.equal(h.state.snapshot().sessions.has(sid), false);
});
test('a rejected response without the common schema cannot release an uncertain launch', async () => {
  const h = await harness({ submit: async () => { const error = refused(400, 'invalid-request'); error.body.schema_version = 9; throw error; } });
  await h.state.submit({ kind: 'analyze' }, { keys_ref: null, max_workers: null });
  assert.ok(h.state.snapshot().owner); assert.equal(h.state.snapshot().owner.phase, 'unknown');
});
test('an accepted claim updates session capacity without waiting for another listing', async () => {
  const h = await harness({ sessions: async () => env({ sessions: [session({ requests_used: 31 })] }) });
  await h.state.submit({ kind: 'analyze' }, { keys_ref: null, max_workers: null });
  assert.equal(h.state.selected().session.requests_used, 32);
});

test('request-pending is a retained admission claim, not a known rejection or permission to repost', async () => {
  let posts = 0, gets = 0;
  const h = await harness({ submit: async () => { posts++; throw refused(409, 'request-pending'); },
    receipt: async () => { gets++; return env({ session_id: sid, request_id: rid, operation: 'analyze', state: 'pending', job_id: null, error: null }); } });
  await h.state.submit({ kind: 'analyze' }, { keys_ref: null, max_workers: null });
  const owner = h.state.snapshot().owner;
  assert.ok(owner); assert.equal(owner.phase, 'pending'); assert.equal(h.state.canStart(), false);
  await h.state.recover(); await h.state.recover();
  assert.equal(h.state.snapshot().owner, owner); assert.equal(posts, 1); assert.equal(gets, 2);
});

test('a throwing view observer cannot prevent a reserved attempt from reaching its single POST', async () => {
  let submitted = 0;
  const sandbox = { window: { App: {} }, Map };
  for (const file of ['common-report.js', 'common-state.js']) vm.runInNewContext(fs.readFileSync(path.join(__dirname, '../../static/js', file), 'utf8'), sandbox);
  const state = sandbox.window.App.CommonState.create({ newId: () => rid, onChange: () => { throw new Error('view unavailable'); },
    api: { capabilities: async () => caps(), sessions: async () => env({ sessions: [session()] }), submit: async () => { submitted++; return job(); } } });
  await state.refresh(); await state.select(sid);
  await state.submit({ kind: 'analyze' }, { keys_ref: null, max_workers: null });
  assert.equal(submitted, 1); assert.equal(state.snapshot().owner.jobId, 'job_1');
});

test('a new plan cannot substitute other output conditions under the accepted job binding', async () => {
  const output = { id: 'clear', format: 'mp4', target: 'clear', drms: [], conformance: 'none' };
  const plan = env({ plan_ref: 'plan_1', session_id: sid, plan_id: pid, plan_schema_version: 2, outputs: [{ ...output, format: 'hls' }], destination_state: 'absent', export_url: '/api/common/plans/plan_1/export' });
  const h = await harness({ submit: async () => job({ operation: 'plan', state: 'succeeded', revision: 1, supervision_confirmed: true, cleanup_confirmed: true, exit_code: 0, plan_ref: 'plan_1', plan_id: pid, report_ref: 'report_1' }),
    plan: async () => plan,
    report: async () => env({ report_ref: 'report_1', session_id: sid, job_id: 'job_1', request_id: rid, operation: 'plan', analysis_ref: null, plan_ref: 'plan_1', plan_id: pid, report_kind: 'plan', report_schema_version: 2, report: { schema_version: 2, plan_id: pid } }) });
  await h.state.submit({ kind: 'plan', analysis_ref: 'analysis_1', output_parent_ref: 'parent_1', destination_name: 'result', outputs: [output] }, { keys_ref: null, max_workers: null });
  assert.equal(h.state.selected().outcome, 'unavailable'); assert.equal(h.state.selected().plan, null);
});
