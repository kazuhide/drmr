/* Common workflow state. Selection never owns a process; immutable attempts do. */
(function () {
  'use strict';
  var App = window.App;
  function newId() {
    var bytes = new Uint8Array(16);
    crypto.getRandomValues(bytes);
    return Array.prototype.map.call(bytes, function (v) { return v.toString(16).padStart(2, '0'); }).join('');
  }
  function failure(code) { return { code: code }; }
  function create(options) {
    var api = options.api || App.CommonApi, id = options.newId || newId;
    var data = { serverId: null, capabilities: null, sessions: new Map(), selectedId: null, owner: null, creation: null, error: null, refreshing: false };
    var closed = new Set(), refreshSequence = 0, mutationSequence = 0;
    function changed() {
      try { if (options.onChange) options.onChange(data); } catch (_error) {
        try { if (options.onObserverError) options.onObserverError(); } catch (_ignored) { /* Observation cannot change process ownership. */ }
      }
    }
    function same(v) { return v && v.server_id === data.serverId; }
    function selected() { return data.sessions.get(data.selectedId) || null; }
    function active(owner) { return data.owner === owner && owner.serverId === data.serverId && !closed.has(owner.sessionId); }
    function known(error) { return error && error.uncertain !== true && error.body && error.body.schema_version === 1 && same(error.body) && error.body.error && typeof error.body.error.code === 'string' && /^[a-z0-9][a-z0-9-]{0,127}$/.test(error.body.error.code) && (error.body.error.job_id === null || typeof error.body.error.job_id === 'string'); }
    function safeError(error) { return failure(known(error) ? error.body.error.code : error && error.code === 'request-timeout' ? 'request-timeout' : 'request-outcome-unknown'); }
    function record(session) {
      if (closed.has(session.session_id)) return null;
      var item = data.sessions.get(session.session_id);
      if (!item) { item = { session: session, plan: null, plans: new Map(), analysis: null, report: null, reports: new Map(), lastJob: null, attempt: null, outcome: null, error: null, closing: false }; data.sessions.set(session.session_id, item); }
      else item.session = session;
      return item;
    }
    function claim(item, requestId) {
      if (!item || item.session.request_ids.indexOf(requestId) !== -1) return;
      item.session = Object.assign({}, item.session, { request_ids: item.session.request_ids.concat([requestId]), requests_used: Math.min(32, item.session.requests_used + 1) });
    }
    function bindingError(owner) { if (active(owner)) { owner.phase = 'unknown'; owner.error = failure('response-binding-invalid'); changed(); } }
    function canStart() {
      var item = selected(), execution = data.capabilities && data.capabilities.execution;
      return !!item && !item.closing && item.session.requests_used < 32 && !data.owner && !data.creation && execution && execution.availability === 'idle';
    }
    function planBinding(item, value, ref) { return same(value) && value.session_id === item.session.session_id && value.plan_ref === ref; }
    function outputKey(outputs) {
      return JSON.stringify(outputs.map(function (output) { return [output.id, output.format, output.target, (output.drms || []).slice().sort(), output.conformance || 'none', output.processing == null ? null : output.processing, output.format === 'dash' ? output.dash_addressing || 'template' : null]; }).sort(function (a, b) { return a[0] < b[0] ? -1 : a[0] > b[0] ? 1 : 0; }));
    }
    function newPlanOptions(value, outputs) {
      return value.plan_schema_version === 8 && Array.isArray(value.output_options) && value.output_options.length === outputs.length && outputs.every(function (output) {
        var row = value.output_options.find(function (row) { return row.id === output.id; });
        return row && row.options && row.options.layout === (output.format === 'mp4' ? 'not-applicable' : 'source') && row.options.dash_addressing === (output.format === 'dash' ? output.dash_addressing || 'template' : null);
      });
    }
    function loadPlan(item, ref, planId, expectedOutputs) {
      var server = data.serverId;
      return api.plan(ref).then(function (value) {
        if (server !== data.serverId || data.sessions.get(item.session.session_id) !== item) return null;
        if (!planBinding(item, value, ref) || planId && value.plan_id !== planId || expectedOutputs && (outputKey(value.outputs) !== outputKey(expectedOutputs) || !newPlanOptions(value, expectedOutputs))) throw failure('response-binding-invalid');
        item.plans.set(ref, value); return value;
      });
    }
    async function select(sid) {
      data.selectedId = data.sessions.has(sid) ? sid : null;
      var item = selected(); changed();
      if (!item) return;
      try {
        var refs = item.session.plan_refs;
        var plans = await Promise.all(refs.map(function (ref) { return item.plans.has(ref) ? item.plans.get(ref) : loadPlan(item, ref); }));
        if (data.sessions.get(sid) === item && !item.plan && plans.length) item.plan = plans[plans.length - 1];
      } catch (_e) { item.error = failure('artifact-unavailable'); }
      changed();
    }
    async function refresh() {
      var sequence = ++refreshSequence, mutation = mutationSequence;
      data.refreshing = true;
      try {
        var caps = await api.capabilities();
        if (sequence !== refreshSequence) return;
        if (data.serverId && data.serverId !== caps.server_id) {
          data.sessions.clear(); closed.clear(); data.selectedId = null; data.creation = null;
          if (data.owner) { data.owner.phase = 'server-changed'; data.owner.error = failure('server-changed'); }
          data.error = failure('server-changed');
        } else data.error = null;
        data.serverId = caps.server_id; data.capabilities = caps;
        var list = await api.sessions();
        if (sequence !== refreshSequence || mutation !== mutationSequence || !same(list)) return;
        var seen = new Set();
        list.sessions.forEach(function (session) { if (same(session)) { record(session); seen.add(session.session_id); } });
        data.sessions.forEach(function (_item, sid) { if (!seen.has(sid) && !(data.owner && data.owner.sessionId === sid)) data.sessions.delete(sid); });
        if (data.selectedId && !data.sessions.has(data.selectedId)) data.selectedId = null;
        if (!data.owner) {
          var running = list.sessions.filter(function (session) { return session.active_request_id !== null; })[0];
          if (running) {
            data.owner = { serverId: data.serverId, sessionId: running.session_id, requestId: running.active_request_id, kind: null, jobId: null, planRef: null, planId: null, revision: -1, phase: 'unknown', stop: 'none' };
            await recover();
          } else if (caps.execution.owner === 'common' && caps.execution.job_id) {
            var observed = await api.job(caps.execution.job_id);
            if (same(observed) && observed.job_id === caps.execution.job_id && !data.owner) {
              var item = data.sessions.get(observed.session_id);
              if (item) {
                var owner = { serverId: data.serverId, sessionId: observed.session_id, requestId: observed.request_id, kind: observed.operation, jobId: observed.job_id, planRef: observed.plan_ref, planId: observed.plan_id, revision: -1, phase: 'unknown', stop: 'none' };
                data.owner = owner; item.attempt = owner; await acceptJob(owner, observed);
              }
            }
          }
        }
      } catch (error) { if (sequence === refreshSequence) data.error = safeError(error); }
      finally { if (sequence === refreshSequence) { data.refreshing = false; changed(); } }
    }
    async function createSession(source) {
      if (data.creation || !data.serverId) return;
      var creation = { serverId: data.serverId, requestId: id(), phase: 'submitting', error: null };
      data.creation = creation; mutationSequence++; changed();
      var body = { schema_version: 1, server_id: data.serverId, creation_request_id: creation.requestId, source: source };
      var request;
      try { request = api.createSession(body); } catch (error) { request = Promise.reject(error); }
      body = null; source = null;
      try {
        var session = await request;
        if (data.creation !== creation || creation.serverId !== data.serverId) return;
        if (!same(session) || session.creation_request_id !== creation.requestId) { creation.phase = 'unknown'; creation.error = failure('response-binding-invalid'); return; }
        var item = record(session); data.creation = null;
        if (item) await select(session.session_id);
      } catch (error) {
        if (data.creation !== creation) return;
        if (known(error)) { data.error = safeError(error); data.creation = null; }
        else { creation.phase = 'unknown'; creation.error = safeError(error); }
      } finally { changed(); }
    }
    async function recoverCreation() {
      var creation = data.creation;
      if (!creation || creation.serverId !== data.serverId || creation.checking) return;
      creation.checking = true; changed();
      try {
        var receipt;
        try { receipt = await api.creation(creation.requestId); } catch (error) { if (!(error && error.status === 404 && known(error))) throw error; }
        if (data.creation !== creation) return;
        if (receipt) {
          if (!same(receipt) || receipt.creation_request_id !== creation.requestId) throw failure('response-binding-invalid');
          if (receipt.state === 'closed') { closed.add(receipt.session_id); data.creation = null; return; }
          var session = await api.session(receipt.session_id);
          if (!same(session) || session.session_id !== receipt.session_id || session.creation_request_id !== creation.requestId) throw failure('response-binding-invalid');
          if (data.creation === creation) { record(session); data.creation = null; await select(session.session_id); }
        } else {
          var list = await api.sessions();
          if (!same(list) || data.creation !== creation) return;
          var matches = list.sessions.filter(function (session) { return same(session) && session.creation_request_id === creation.requestId; });
          if (matches.length === 1) { record(matches[0]); data.creation = null; await select(matches[0].session_id); }
          else { creation.phase = 'unknown'; creation.error = failure('request-outcome-unknown'); }
        }
      } catch (error) { if (data.creation === creation) { creation.phase = 'unknown'; creation.error = safeError(error); } }
      finally { creation.checking = false; changed(); }
    }
    function jobMatches(owner, value) {
      return same(value) && value.session_id === owner.sessionId && value.request_id === owner.requestId && (!owner.kind || value.operation === owner.kind) && (!owner.jobId || value.job_id === owner.jobId) && (!owner.planRef || value.plan_ref === owner.planRef) && (!owner.planId || value.plan_id === owner.planId);
    }
    async function artifacts(owner, value, item) {
      var current = function () { return owner.serverId === data.serverId && data.sessions.get(owner.sessionId) === item && item.attempt === owner && !closed.has(owner.sessionId); };
      if (current()) item.error = null;
      var tasks = [];
      if (value.analysis_ref && value.operation === 'analyze') tasks.push(api.analysis(value.analysis_ref).then(function (analysis) {
        if (!current()) return;
        if (!same(analysis) || analysis.analysis_ref !== value.analysis_ref || analysis.session_id !== owner.sessionId || analysis.job_id !== value.job_id || analysis.report_ref !== value.report_ref || value.source_ref !== null && analysis.source_ref !== value.source_ref) throw failure('response-binding-invalid');
        item.analysis = analysis;
      }));
      if (value.plan_ref) tasks.push(loadPlan(item, value.plan_ref, value.plan_id, owner.expectedOutputs).then(function (plan) {
        if (!current() || !plan) return;
        if (plan.plan_id !== value.plan_id || (owner.kind === 'plan' && plan.plan_schema_version !== 8)) throw failure('response-binding-invalid');
        item.plan = plan;
      }));
      if (value.report_ref) tasks.push(api.report(value.report_ref).then(function (report) {
        if (!current()) return;
        if (!same(report) || report.report_ref !== value.report_ref || report.session_id !== owner.sessionId || report.job_id !== value.job_id || report.request_id !== owner.requestId || report.operation !== owner.kind || report.plan_ref !== value.plan_ref || report.plan_id !== value.plan_id || (report.report.plan_id != null && report.report.plan_id !== value.plan_id)) throw failure('response-binding-invalid');
        if (value.analysis_ref && report.analysis_ref !== value.analysis_ref) throw failure('response-binding-invalid');
        item.report = report; item.reports.set(owner.kind, report);
      }));
      try {
        await Promise.all(tasks);
        if (current()) {
          var expected = { analyze: 'analysis', plan: 'plan', convert: 'conversion', verify: 'verification' }[owner.kind];
          var requiredArtifact = owner.kind === 'analyze' ? item.analysis && item.analysis.analysis_ref === value.analysis_ref : item.plan && item.plan.plan_ref === value.plan_ref;
          item.outcome = value.state === 'succeeded' && value.exit_code === 0 && requiredArtifact && item.report && item.report.report_kind === expected && App.CommonReport.evidencePassed(item.report) ? 'succeeded' : value.state === 'succeeded' ? 'unavailable' : value.state;
          if (item.outcome === 'succeeded' && owner.kind === 'verify' && (item.report.report.passed === false || item.report.report.verification && item.report.report.verification.passed !== true)) item.outcome = 'failed';
        }
      } catch (_error) { if (current()) { item.error = failure('artifact-unavailable'); item.outcome = value.state === 'succeeded' ? 'unavailable' : value.state; } }
      changed();
    }
    async function acceptJob(owner, value) {
      if (!active(owner)) return;
      if (!jobMatches(owner, value)) { bindingError(owner); return; }
      if (value.revision <= owner.revision) return;
      owner.kind = value.operation; owner.jobId = value.job_id; owner.revision = value.revision; owner.phase = 'running'; owner.error = value.error;
      if (value.plan_ref) { owner.planRef = value.plan_ref; owner.planId = value.plan_id; }
      if (value.stop_requested) owner.stop = 'requested';
      var item = data.sessions.get(owner.sessionId);
      if (item) { claim(item, owner.requestId); item.attempt = owner; item.lastJob = value; item.error = null; }
      if (value.state !== 'running') {
        if (!value.supervision_confirmed || !value.cleanup_confirmed) { owner.phase = 'unknown'; owner.error = failure('cleanup-unconfirmed'); }
        else {
          data.owner = null;
          if (item) { item.outcome = value.state === 'succeeded' ? 'collecting' : value.state; changed(); await artifacts(owner, value, item); }
        }
      }
      changed();
    }
    async function submit(operation, runtime) {
      var item = selected();
      if (!item) return;
      if (!canStart()) { if (item.session.requests_used >= 32) item.error = failure('request-limit-reached'); changed(); return; }
      if ((operation.kind === 'convert' || operation.kind === 'verify') && (!item.plan || item.plan.plan_ref !== operation.plan_ref)) { item.error = failure('invalid-request'); changed(); return; }
      var owner = { serverId: data.serverId, sessionId: item.session.session_id, requestId: id(), kind: operation.kind, jobId: null, planRef: operation.plan_ref || null, planId: operation.plan_ref ? item.plan.plan_id : null, revision: -1, phase: 'submitting', stop: 'none' };
      var body = { schema_version: 1, server_id: data.serverId, session_id: owner.sessionId, request_id: owner.requestId, operation: JSON.parse(JSON.stringify(operation)), runtime: { keys_ref: runtime.keys_ref, max_workers: runtime.max_workers, decode: runtime.decode === true } };
      owner.expectedOutputs = operation.kind === 'plan' ? body.operation.outputs : null;
      data.owner = owner; mutationSequence++; item.attempt = owner; item.reports.delete(operation.kind); if (operation.kind === 'convert') item.reports.delete('verify'); item.report = null; item.error = null; item.outcome = null; item.lastJob = null; if (operation.kind === 'analyze') { item.analysis = null; item.plan = null; } if (operation.kind === 'plan') item.plan = null; changed();
      try { await acceptJob(owner, await api.submit(body)); }
      catch (error) {
        if (!active(owner)) return;
        if (known(error) && error.body.error.code === 'request-pending') { claim(item, owner.requestId); owner.phase = 'pending'; owner.error = failure('request-pending'); }
        else if (known(error) && error.body.error.job_id === null && error.body.error.code !== 'cleanup-unconfirmed') { item.error = safeError(error); item.outcome = 'rejected'; data.owner = null; }
        else { owner.phase = 'unknown'; owner.error = safeError(error); }
      } finally { body = null; changed(); }
    }
    async function recoverTerminal() {
      var item = selected(), owner = item && item.attempt;
      if (!item || !owner || !item.lastJob || item.recovering || owner.serverId !== data.serverId || closed.has(owner.sessionId)) return;
      item.recovering = true; changed();
      try {
        var value = await api.job(item.lastJob.job_id);
        if (item.attempt !== owner || owner.serverId !== data.serverId || data.sessions.get(owner.sessionId) !== item) return;
        if (!jobMatches(owner, value) || value.revision < item.lastJob.revision) throw failure('response-binding-invalid');
        if (value.state === 'running' || !value.supervision_confirmed || !value.cleanup_confirmed) {
          if (!data.owner) { data.owner = owner; owner.phase = 'unknown'; owner.error = failure('cleanup-unconfirmed'); }
          return;
        }
        item.lastJob = value; await artifacts(owner, value, item);
      } catch (_error) { if (item.attempt === owner) item.error = failure('artifact-unavailable'); }
      finally { item.recovering = false; changed(); }
    }
    async function recover() {
      var owner = data.owner;
      if (!owner) return recoverTerminal();
      if (!owner || !active(owner) || owner.checking) return;
      owner.checking = true; changed();
      try {
        if (owner.jobId) await acceptJob(owner, await api.job(owner.jobId));
        else {
          var receipt = await api.receipt(owner.sessionId, owner.requestId);
          if (!active(owner)) return;
          if (!same(receipt) || receipt.session_id !== owner.sessionId || receipt.request_id !== owner.requestId || owner.kind && receipt.operation !== owner.kind) { bindingError(owner); return; }
          owner.kind = receipt.operation; claim(data.sessions.get(owner.sessionId), owner.requestId);
          if (receipt.state === 'accepted') { owner.jobId = receipt.job_id; await acceptJob(owner, await api.job(receipt.job_id)); }
          else if (receipt.state === 'rejected') { var item = data.sessions.get(owner.sessionId); if (item) { item.error = receipt.error || failure('invalid-request'); item.outcome = 'rejected'; } data.owner = null; }
          else owner.phase = 'pending';
        }
      } catch (error) { if (active(owner)) { owner.phase = 'unknown'; owner.error = safeError(error); } }
      finally { owner.checking = false; changed(); }
    }
    async function stop() {
      var owner = data.owner;
      if (!owner || !active(owner) || !owner.jobId) return recover();
      if (owner.stop === 'sending' || owner.stop === 'unknown' || owner.stop === 'requested') return recover();
      owner.stop = 'sending'; changed();
      try { await acceptJob(owner, await api.stop(owner.jobId)); }
      catch (error) { if (active(owner)) { owner.stop = known(error) ? 'rejected' : 'unknown'; owner.error = safeError(error); } }
      finally { changed(); }
    }
    function fence(sid, server) {
      if (server !== data.serverId) return;
      closed.add(sid); data.sessions.delete(sid);
      if (data.selectedId === sid) data.selectedId = null;
      if (data.owner && data.owner.serverId === server && data.owner.sessionId === sid) data.owner = null;
    }
    async function close(sid) {
      var item = data.sessions.get(sid), server = data.serverId;
      if (!item || item.closing) return;
      item.closing = true; mutationSequence++; changed();
      try {
        await api.close(sid);
        fence(sid, server);
      } catch (error) { if (known(error) && error.status === 404 && error.body.error.code === 'session-not-found') fence(sid, server); else if (data.sessions.get(sid) === item) item.error = safeError(error); }
      finally { item.closing = false; changed(); }
    }
    return { snapshot: function () { return data; }, selected: selected, canStart: canStart, refresh: refresh, select: select, createSession: createSession, recoverCreation: recoverCreation, submit: submit, recover: recover, stop: stop, close: close,
      usePlan: function (ref) { var item = selected(); if (item && item.plans.has(ref)) { item.plan = item.plans.get(ref); changed(); } },
      invalidatePlan: function () { var item = selected(); if (item) { item.plan = null; changed(); } },
    };
  }
  App.CommonState = { create: create, newId: newId };
})();
