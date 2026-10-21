'use strict';
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const test = require('node:test');
const vm = require('node:vm');
function load() {
  const box = { window: { App: {} } };
  vm.runInNewContext(fs.readFileSync(path.join(__dirname, '../../static/js/common-report.js'), 'utf8'), box);
  return box.window.App.CommonReport;
}
const checks = () => ({ stream_metadata: 'pass', packet_payloads_equal: true, packet_timing_equal: true, decoded_content_equal: true });
const verification = extra => ({ schema_version: 1, passed: true, media_verified: true, checks: checks(), license_playback_verified: false, ...extra });
test('media, timing and payload evidence are required independently', () => {
  const r = load();
  assert.equal(r.verificationPassed(verification()), true);
  for (const key of ['packet_payloads_equal', 'packet_timing_equal', 'decoded_content_equal']) {
    assert.equal(r.verificationPassed(verification({ checks: { ...checks(), [key]: false } })), false);
  }
  assert.equal(r.verificationPassed({ passed: true }), false);
  assert.equal(r.verificationPassed(verification({ media_verified: false })), false);
});
test('a truncated or incomplete native check cannot be displayed as a completed pass', () => {
  const r = load();
  for (const extra of [{ truncated: true }, { incomplete: true }]) {
    assert.equal(r.nativeState({ errors: 0, warnings: 0, truncated: false, incomplete: false, ...extra }), 'incomplete');
  }
  assert.equal(r.nativeState({ errors: 1, warnings: 0, truncated: false, incomplete: false }), 'fail');
  assert.equal(r.nativeState(null), 'not-checked');
  assert.equal(r.nativeState({ errors: 0, warnings: 1, truncated: false, incomplete: false }), 'warning');
});
test('a version-two plan can have version-one result envelopes without losing output checks', () => {
  const r = load();
  const report = { report_kind: 'verification', report: { schema_version: 1, passed: true, source_unchanged: true, outputs: [{ id: 'clear', output_matches_plan: true, verification: verification(), signaling: { systems: [], verified: false }, conformance: null }] } };
  assert.equal(r.evidencePassed(report), true);
  report.report.outputs[0].output_matches_plan = false;
  assert.equal(r.evidencePassed(report), false);
  report.report.outputs = [];
  assert.equal(r.evidencePassed(report), false);
});
test('unrequested DRM signaling and external checks do not create pass evidence', () => {
  const r = load();
  assert.equal(r.signalingState({ systems: [], verified: false }), 'not-requested');
  assert.equal(r.signalingState({ systems: ['widevine'], verified: false }), 'fail');
  assert.equal(r.conformanceState(null), 'not-requested');
  assert.equal(r.conformanceState({ passed: true, checks: [] }), 'incomplete');
});

test('known failure codes have focused guidance without exposing arbitrary server text', () => {
  const r = load();
  assert.equal(r.errorInfo({ code: 'execution-plan-source-changed', hint: 'secret' }).key, 'commonSourceChanged');
  assert.equal(r.errorInfo({ code: 'invalid-encrypted-segment', hint: 'secret' }).key, 'commonDecryptionFailed');
  assert.equal(r.errorInfo({ code: 'manifest-validator-unavailable' }).key, 'commonValidatorUnavailable');
  assert.equal(r.errorInfo({ code: 'private-sentinel', hint: 'secret' }).code, null);
});
function renderer() {
  class Element {
    constructor(tag) { this.tagName = tag; this.nodeType = 1; this.children = []; this.dataset = {}; this.textContent = ''; this.attrs = new Map(); }
    appendChild(node) { this.children.push(node); return node; }
    replaceChildren() { this.children = []; }
    setAttribute(k, v) { this.attrs.set(k, v); }
    getAttribute(k) { return this.attrs.get(k) || null; }
    removeAttribute(k) { this.attrs.delete(k); }
    addEventListener() {}
  }
  const box = { window: { App: { I18n: { t: (key, values) => key + (values ? JSON.stringify(values) : '') } } },
    document: { createElement: tag => new Element(tag), createTextNode: text => ({ nodeType: 3, textContent: text, children: [] }) } };
  for (const file of ['ui.js', 'common-report.js']) vm.runInNewContext(fs.readFileSync(path.join(__dirname, '../../static/js', file), 'utf8'), box);
  const host = new Element('div');
  const all = node => [node, ...node.children.flatMap(all)];
  return { host, all, render: envelope => box.window.App.CommonReport.render(host, envelope), text: () => all(host).map(node => node.textContent).join('|') };
}
function renderResult(report) {
  const r = renderer(); r.render(report); return r.text();
}
test('a media-tool failure retains its safe code without exposing private tool output', () => {
  const text = renderResult({ report_kind: 'failure', operation: 'verify', report: {
    schema_version: 1, status: 'failed', error: { code: 'media-tool-failed', hint: 'PRIVATE_TOOL_OUTPUT', last_activity: 'verification' },
  } });
  assert.match(text, /media-tool-failed/);
  assert.match(text, /commonMediaToolFailed/);
  assert.doesNotMatch(text, /PRIVATE_TOOL_OUTPUT/);
});
test('native tagged coverage retains status, reason, counts and profile revision', () => {
  const native = { errors: 0, warnings: 0, truncated: false, incomplete: false, document_count: 1, profile: 'dash-static-manifest', profile_revision: '2026-10-21.1', engine_revision: '2026-10-21.1', diagnostics: [],
    coverage: [{ document: 0, rule_id: 'dash.rule', scope: 'in-scope', status: { status: 'not-checked', reason: 'requires-media-evidence' }, passed_checks: 0, failed_checks: 0, not_checked_checks: 1, not_checked_reasons: ['requires-media-evidence'] }] };
  const text = renderResult({ report_kind: 'verification', report: { schema_version: 1, source_unchanged: true, output_matches_plan: true, verification: verification({ native_manifest: native }) } });
  assert.match(text, /commonNotChecked/); assert.match(text, /commonReasonMedia/);
  assert.match(text, /dash-static-manifest/); assert.match(text, /2026-10-21.1/);
  assert.match(text, /commonCoverageCounts/);
});
test('bounded diagnostic rendering explicitly states omitted rows', () => {
  const diagnostics = Array.from({ length: 1001 }, () => ({ rule_id: 'dash.rule', severity: 'warning' }));
  const text = renderResult({ report_kind: 'conversion', report: { schema_version: 1, verification: verification({ native_manifest: { errors: 0, warnings: 1001, truncated: false, incomplete: false, document_count: 1, profile: 'dash-static-manifest', profile_revision: '2026-10-21.1', engine_revision: '2026-10-21.1', diagnostics, coverage: [] } }) } });
  assert.match(text, /commonRowsOmitted/); assert.match(text, /"total":1001/);
});

test('rendered evidence binds its actual envelope and each displayed output check', () => {
  const r = renderer(), planId = 'c'.repeat(64);
  const native = { errors: 1, warnings: 0, truncated: false, incomplete: false, diagnostics: [], coverage: [] };
  const envelope = { job_id: 'job_negative', request_id: 'request_negative', operation: 'verify', report_kind: 'verification', plan_id: planId, plan_ref: 'plan_actual',
    report: { schema_version: 1, source_unchanged: false, outputs: [
      { id: 'clear', output_matches_plan: true, verification: verification(), signaling: { systems: [], verified: false }, conformance: null },
      { id: 'encrypted', output_matches_plan: false, verification: verification({ media_verified: false, checks: { stream_metadata: 'fail', packet_payloads_equal: false, packet_timing_equal: null, decoded_content_equal: true }, native_manifest: native }), signaling: { systems: ['widevine'], verified: false }, conformance: { passed: false, checks: [{ tool: 'dash-if', passed: false, checked_manifest_count: 1, warning_count: 0 }] } }
    ], private_path: '/private/not-dom', url: 'https://private.invalid/?secret=not-dom', key: 'private-key-not-dom', kid: 'private-kid-not-dom' } };
  r.render(envelope);
  for (const [attribute, value] of Object.entries({ 'job-id': 'job_negative', 'request-id': 'request_negative', operation: 'verify', 'report-kind': 'verification', 'plan-id': planId, 'plan-ref': 'plan_actual' })) {
    assert.equal(r.host.getAttribute('data-common-' + attribute), value);
  }
  const outputs = r.all(r.host).filter(node => node.getAttribute?.('data-common-output-index') !== null && node.getAttribute?.('data-common-output-index') !== undefined);
  assert.equal(outputs.length, 2);
  assert.equal(outputs[0].getAttribute('data-common-output-index'), '0'); assert.equal(outputs[0].getAttribute('data-common-output-id'), 'clear');
  assert.equal(outputs[1].getAttribute('data-common-output-index'), '1'); assert.equal(outputs[1].getAttribute('data-common-output-id'), 'encrypted');
  function state(parent, name) {
    const node = r.all(parent).find(node => node.getAttribute?.('data-common-check') === name);
    assert.ok(node, name); return node.getAttribute('data-common-state');
  }
  assert.equal(state(r.host, 'source_unchanged'), 'fail');
  const expected = [
    { output_matches_plan: 'pass', media_verified: 'pass', stream_metadata: 'pass', packet_payloads_equal: 'pass', packet_timing_equal: 'pass', decoded_content_equal: 'pass', license_playback_verified: 'not-checked', native_manifest: 'not-checked', signaling: 'not-requested', conformance: 'not-requested' },
    { output_matches_plan: 'fail', media_verified: 'fail', stream_metadata: 'fail', packet_payloads_equal: 'fail', packet_timing_equal: 'unknown', decoded_content_equal: 'pass', license_playback_verified: 'not-checked', native_manifest: 'fail', signaling: 'fail', conformance: 'fail' }
  ];
  outputs.forEach((output, index) => Object.entries(expected[index]).forEach(([check, value]) => assert.equal(state(output, check), value, check)));
  assert.match(r.text(), /commonFail/); assert.match(r.text(), /commonUnknown/); assert.match(r.text(), /commonNotRequested/); assert.match(r.text(), /commonNotChecked/);
  assert.doesNotMatch(r.text(), /not-dom/);
  assert.equal(r.all(r.host).flatMap(node => [...(node.attrs || [])].map(([, value]) => value)).some(value => String(value).includes('not-dom')), false);
});

test('analysis admission and v1 output bindings describe only the displayed evidence', () => {
  const r = renderer();
  r.render({ report_kind: 'analysis', operation: 'analyze', report: { input_admitted: false, eligible_formats: [] } });
  const admission = r.all(r.host).find(node => node.getAttribute?.('data-common-check') === 'input_admitted');
  assert.ok(admission); assert.equal(admission.getAttribute('data-common-state'), 'fail');
  assert.match(r.text(), /commonInputAdmitted\|commonFail/);
  r.render({ report_kind: 'conversion', operation: 'convert', report: { schema_version: 1, verification: verification() } });
  const single = r.all(r.host).find(node => node.getAttribute?.('data-common-output-index') === '0');
  assert.ok(single); assert.equal(single.getAttribute('data-common-output-id'), null);
  assert.match(r.text(), /commonSingleOutput/);
  assert.equal(r.all(r.host).some(node => node.getAttribute?.('data-common-check') === 'output_matches_plan'), false);
});

test('eligible format items remain readable and keep untrusted values out of the DOM', () => {
  const r = renderer();
  const text = node => node.textContent + node.children.map(text).join('');
  for (const [formats, expected] of [
    [['mp4', 'dash', 'hls'], 'mp4, dash, hls'],
    [['hls-ts'], 'hls-ts'],
    [[], ''],
    [['mp4', 'https://source.invalid/PRIVATE_FORMAT?key=secret', 'hls'], 'mp4, commonUnknown, hls'],
  ]) {
    r.render({ report_kind: 'analysis', report: { input_admitted: true, actual_format: 'hls-ts', eligible_formats: formats } });
    const fields = r.all(r.host).find(node => node.tagName === 'dl' && node.children.some(child => child.textContent === 'commonEligible'));
    const index = fields.children.findIndex(node => node.textContent === 'commonEligible');
    const value = fields.children[index + 1];
    assert.equal(text(value), expected);
    assert.equal(r.all(value).filter(node => node.tagName === 'code').length, formats.length);
    assert.doesNotMatch(r.text(), /PRIVATE_FORMAT|key=secret/);
  }
});

test('report bindings clear on null evidence and reject path-like metadata', () => {
  const r = renderer(), attrs = ['job-id', 'request-id', 'operation', 'report-kind', 'plan-id', 'plan-ref'];
  const envelope = { job_id: 'job_one', request_id: 'request_one', operation: 'verify', report_kind: 'verification', plan_id: 'd'.repeat(64), plan_ref: 'plan_one', report: { source_unchanged: true, verification: verification(), output_matches_plan: true } };
  for (const empty of [null, { ...envelope, report: null }]) {
    r.render(envelope); assert.equal(r.host.getAttribute('data-common-job-id'), 'job_one');
    r.render(empty);
    attrs.forEach(name => assert.equal(r.host.getAttribute('data-common-' + name), null, name));
    assert.equal(r.all(r.host).some(node => node.getAttribute?.('data-common-check')), false);
    assert.match(r.text(), /commonNoResult/);
  }
  r.render({ ...envelope, job_id: 'https://private.invalid/?secret=not-dom', request_id: '/private/keys.json', operation: 'untrusted-not-dom', report_kind: 'untrusted-not-dom', plan_id: 'private-key-not-dom', plan_ref: '../private/config.json' });
  attrs.forEach(name => assert.equal(r.host.getAttribute('data-common-' + name), null, name));
  assert.doesNotMatch(r.text(), /not-dom/);
});
