/* Common workflow HTTP adapter. Server-issued references carry no path authority. */
(function () {
  'use strict';
  var App = window.App;
  var BASE = '/api/common';
  var ROLES = ['media', 'saved-plan', 'keys', 'fetch-policy', 'output-parent'];
  var OPS = ['analyze', 'plan', 'convert', 'verify'];
  var ACTIVITIES = ['configuration', 'acquisition', 'input-preparation', 'inspection', 'normalization', 'packaging', 'verification', 'publication'];
  function object(v) { return !!v && typeof v === 'object' && !Array.isArray(v); }
  function token(v) { return typeof v === 'string' && /^[A-Za-z0-9][A-Za-z0-9_-]{0,127}$/.test(v); }
  function correlation(v) { return typeof v === 'string' && /^[a-f0-9]{32}$/.test(v); }
  function digest(v) { return typeof v === 'string' && /^[a-f0-9]{64}$/.test(v); }
  function integer(v, max) { return Number.isSafeInteger(v) && v >= 0 && v <= max; }
  function nullable(v, fn) { return v === null || fn(v); }
  function one(v, values) { return values.indexOf(v) !== -1; }
  var DASH_ADDRESSING = ['template', 'template-timeline', 'list', 'base'];
  function singleSchema(schema) { return one(schema, [1, 3, 5, 7]); }
  function keys(v, names) { return object(v) && Object.keys(v).every(function (key) { return one(key, names); }); }
  function processingSelection(v, format) {
    if (!object(v) || Object.keys(v).some(function (key) { return !one(key, ['requested', 'resolved', 'boundaries', 'segment_duration_ms']); }) ||
        !one(v.requested, ['auto', 'segments', 'stitch']) || !one(v.resolved, ['segments', 'stitch']) ||
        v.requested !== 'auto' && v.requested !== v.resolved) return false;
    if (v.resolved === 'segments') return v.boundaries === 'preserve' && v.segment_duration_ms === null && format !== 'mp4';
    return format === 'mp4' ? v.boundaries === 'not-applicable' && v.segment_duration_ms === null : v.boundaries === 'resegment' && integer(v.segment_duration_ms, 4294967295) && v.segment_duration_ms > 0;
  }
  function frozenProcessing(v) {
    if (v.plan_schema_version < 5) return !Object.prototype.hasOwnProperty.call(v, 'processing') && v.outputs.every(function (output) { return !Object.prototype.hasOwnProperty.call(output, 'processing'); });
    if (singleSchema(v.plan_schema_version)) return processingSelection(v.processing, v.outputs[0].format);
    if (!Array.isArray(v.processing) || v.processing.length !== v.outputs.length) return false;
    var seen = new Set();
    return v.processing.every(function (choice) {
      if (!keys(choice, ['id', 'selection']) || !token(choice.id) || seen.has(choice.id)) return false;
      seen.add(choice.id);
      var output = v.outputs.find(function (output) { return output.id === choice.id; });
      return !!output && processingSelection(choice.selection, output.format) &&
        (!Object.prototype.hasOwnProperty.call(output, 'processing') || output.processing === choice.selection.requested);
    });
  }
  function outputOptions(v, format) {
    return keys(v, ['layout', 'dash_addressing']) &&
      (format === 'mp4' ? v.layout === 'not-applicable' : one(v.layout, ['source', 'previous-saved'])) &&
      (format === 'dash' ? one(v.dash_addressing, DASH_ADDRESSING) : v.dash_addressing === null);
  }
  function frozenOptions(v) {
    if (v.plan_schema_version < 7) return !Object.prototype.hasOwnProperty.call(v, 'output_options');
    if (singleSchema(v.plan_schema_version)) return outputOptions(v.output_options, v.outputs[0].format);
    if (!Array.isArray(v.output_options) || v.output_options.length !== v.outputs.length) return false;
    var seen = new Set();
    return v.output_options.every(function (choice) {
      if (!keys(choice, ['id', 'options']) || !token(choice.id) || seen.has(choice.id)) return false;
      seen.add(choice.id);
      var output = v.outputs.find(function (output) { return output.id === choice.id; });
      return !!output && outputOptions(choice.options, output.format) &&
        (!Object.prototype.hasOwnProperty.call(output, 'dash_addressing') || output.dash_addressing === choice.options.dash_addressing);
    });
  }
  function label(v) { return typeof v === 'string' && v.length <= 4096; }
  function envelope(v) { return object(v) && v.schema_version === 1 && token(v.server_id); }
  function error(v) { return object(v) && typeof v.code === 'string' && /^[a-z0-9][a-z0-9-]{0,127}$/.test(v.code) && nullable(v.last_activity, function (x) { return one(x, ACTIVITIES); }) && nullable(v.job_id, token); }
  function progress(v) { return object(v) && integer(v.sequence, Number.MAX_SAFE_INTEGER) && integer(v.elapsed_ms, Number.MAX_SAFE_INTEGER) && integer(v.scopes_started, Number.MAX_SAFE_INTEGER) && integer(v.scopes_closed, Number.MAX_SAFE_INTEGER) && nullable(v.last_activity, function (x) { return one(x, ACTIVITIES); }) && nullable(v.workers, function (x) { return integer(x, 64) && x > 0; }); }
  function session(v) {
    if (!envelope(v) || !token(v.session_id) || !correlation(v.creation_request_id) || !object(v.source) || !one(v.source.kind, ['local', 'url', 'saved-plan'])) return false;
    return label(v.source.label) && Array.isArray(v.plan_refs) && v.plan_refs.every(token) && Array.isArray(v.request_ids) && v.request_ids.every(correlation) && integer(v.requests_used, 32) && nullable(v.active_request_id, correlation);
  }
  function job(v) {
    if (!envelope(v) || !token(v.job_id) || !token(v.session_id) || !correlation(v.request_id) || !one(v.operation, OPS) || !integer(v.revision, Number.MAX_SAFE_INTEGER) || !one(v.state, ['running', 'succeeded', 'failed', 'interrupted'])) return false;
    if (['stop_requested', 'supervision_confirmed', 'cleanup_confirmed'].some(function (key) { return typeof v[key] !== 'boolean'; })) return false;
    return nullable(v.exit_code, Number.isSafeInteger) && ['source_ref', 'analysis_ref', 'plan_ref', 'report_ref'].every(function (key) { return nullable(v[key], token); }) && nullable(v.plan_id, digest) && nullable(v.progress, progress) && nullable(v.error, error);
  }
  function plan(v) {
    return envelope(v) && token(v.plan_ref) && token(v.session_id) && digest(v.plan_id) && one(v.plan_schema_version, [1, 2, 3, 4, 5, 6, 7, 8]) && (v.plan_schema_version < 3 || one(v.required_depth, ["native", "decoded"])) && Array.isArray(v.outputs) && v.outputs.length > 0 && v.outputs.length <= 8 && (!singleSchema(v.plan_schema_version) || v.outputs.length === 1) && v.outputs.every(function (output) {
      return keys(output, ['id', 'format', 'target', 'drms', 'conformance', 'processing', 'dash_addressing']) && (singleSchema(v.plan_schema_version) ? output.id === null : token(output.id)) && one(output.format, ['mp4', 'dash', 'hls', 'hls-ts']) && one(output.target, ['clear', 'cenc', 'cbcs', 'hls-aes128', 'hls-sample-aes']) && (singleSchema(v.plan_schema_version) || Array.isArray(output.drms) && output.drms.length <= 3 && output.drms.every(function (drm) { return one(drm, ['widevine', 'playready', 'fairplay']); })) && (singleSchema(v.plan_schema_version) || one(output.conformance, ['none', 'required'])) && (!Object.prototype.hasOwnProperty.call(output, 'processing') || one(output.processing, ['auto', 'segments', 'stitch'])) && (!Object.prototype.hasOwnProperty.call(output, 'dash_addressing') || v.plan_schema_version === 8 && output.format === 'dash' && one(output.dash_addressing, DASH_ADDRESSING));
    }) && frozenProcessing(v) && frozenOptions(v) && one(v.destination_state, ['absent', 'present', 'mixed', 'unavailable']) && v.export_url === BASE + '/plans/' + encodeURIComponent(v.plan_ref) + '/export';
  }
  function capabilities(v) {
    return envelope(v) && Array.isArray(v.roots) && v.roots.every(function (r) { return object(r) && token(r.root_ref) && label(r.label) && Array.isArray(r.roles) && r.roles.every(function (role) { return one(role, ROLES); }); }) && object(v.limits) && v.limits.sessions === 8 && v.limits.requests_per_session === 32 && v.limits.max_outputs === 8 && object(v.worker_policy) && integer(v.worker_policy.maximum, 64) && v.worker_policy.maximum > 0 && v.worker_policy.automatic === true && object(v.execution) && one(v.execution.availability, ['idle', 'busy', 'cleanup-unconfirmed', 'closing']) && nullable(v.execution.job_id, token) && nullable(v.execution.owner, function (x) { return x === 'common'; }) && v.observation === 'poll';
  }
  function filePage(v) {
    return envelope(v) && token(v.root_ref) && one(v.role, ROLES) && nullable(v.directory_ref, token) && (v.role === 'output-parent' ? token(v.output_parent_ref) && v.output_parent_ref !== v.root_ref && v.output_parent_ref !== v.directory_ref : v.output_parent_ref === null) && Array.isArray(v.entries) && v.entries.length <= 100 && v.entries.every(function (e) {
      return object(e) && label(e.label) && (e.kind === 'directory' ? token(e.directory_ref) : e.kind === 'file' && token(e.file_ref));
    }) && nullable(v.next_cursor, token);
  }
  function creation(v) { return envelope(v) && correlation(v.creation_request_id) && token(v.session_id) && one(v.state, ['created', 'closed']); }
  function receipt(v) { return envelope(v) && token(v.session_id) && correlation(v.request_id) && one(v.operation, OPS) && one(v.state, ['pending', 'accepted', 'rejected']) && nullable(v.job_id, token) && nullable(v.error, error) && (v.state !== 'accepted' || token(v.job_id)); }
  function analysis(v) { return envelope(v) && ['analysis_ref', 'session_id', 'job_id', 'source_ref', 'report_ref'].every(function (k) { return token(v[k]); }); }
  function report(v) { return envelope(v) && ['report_ref', 'session_id', 'job_id'].every(function (k) { return token(v[k]); }) && correlation(v.request_id) && one(v.operation, OPS) && nullable(v.analysis_ref, token) && nullable(v.plan_ref, token) && nullable(v.plan_id, digest) && (v.report_kind === 'plan' ? [1, 2, 3, 4, 5, 6, 7, 8].indexOf(v.report_schema_version) !== -1 : [1, 2].indexOf(v.report_schema_version) !== -1) && object(v.report) && v.report_schema_version === v.report.schema_version && one(v.report_kind, ['analysis', 'plan', 'conversion', 'verification', 'failure']) && object(v.report); }
  function invalid(code, uncertain) { var e = new Error(code); e.code = code; e.uncertain = !!uncertain; return e; }
  function read(path, validate, options) {
    var mutation = !!(options && options.method && options.method !== 'GET');
    return App.Api.fetchJson(BASE + path, options).then(function (value) {
      if (!validate(value)) throw invalid('request-response-invalid', mutation);
      return value;
    });
  }
  function at(collection, ref, validate) {
    if (!token(ref)) return Promise.reject(invalid('invalid-request'));
    return read('/' + collection + '/' + encodeURIComponent(ref), validate);
  }
  function post(path, body, validate) { return read(path, validate, { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify(body) }); }
  App.CommonApi = {
    isToken: token, isCorrelation: correlation,
    processingSelection: processingSelection,
    capabilities: function () { return read('/capabilities', capabilities); },
    sessions: function () { return read('/sessions', function (v) { return envelope(v) && Array.isArray(v.sessions) && v.sessions.length <= 8 && v.sessions.every(session); }); },
    files: function (selection) {
      if (!selection || !token(selection.root_ref) || !one(selection.role, ROLES) || (selection.directory_ref != null && !token(selection.directory_ref)) || (selection.cursor != null && !token(selection.cursor))) return Promise.reject(invalid('invalid-request'));
      var query = new URLSearchParams({ root_ref: selection.root_ref, role: selection.role, limit: '100' });
      if (selection.directory_ref != null) query.set('directory_ref', selection.directory_ref);
      if (selection.cursor != null) query.set('cursor', selection.cursor);
      return read('/files?' + query.toString(), function (v) { return filePage(v) && v.root_ref === selection.root_ref && v.role === selection.role && v.directory_ref === (selection.directory_ref || null); });
    },
    createSession: function (body) { return post('/sessions', body, session); },
    creation: function (id) { return correlation(id) ? at('session-requests', id, creation) : Promise.reject(invalid('invalid-request')); },
    session: function (id) { return at('sessions', id, session); },
    close: function (id) { return token(id) ? read('/sessions/' + encodeURIComponent(id), function (v) { return v === null; }, { method: 'DELETE' }) : Promise.reject(invalid('invalid-request')); },
    submit: function (body) { return post('/jobs', body, job); },
    receipt: function (sid, id) { return token(sid) && correlation(id) ? read('/sessions/' + encodeURIComponent(sid) + '/requests/' + id, receipt) : Promise.reject(invalid('invalid-request')); },
    job: function (id) { return at('jobs', id, job); },
    stop: function (id) { return token(id) ? post('/jobs/' + encodeURIComponent(id) + '/stop', {}, job) : Promise.reject(invalid('invalid-request')); },
    analysis: function (ref) { return at('analyses', ref, analysis); },
    plan: function (ref) { return at('plans', ref, plan); },
    report: function (ref) { return at('reports', ref, report); },
  };
})();
