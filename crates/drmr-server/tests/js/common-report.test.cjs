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
const nativeVerification = extra => ({ schema_version: 2, required_depth: 'native', scope: 'comparison', passed: true, checks: { stream_metadata: 'pass', packet_payloads_equal: true, packet_timing_equal: true }, decode_evidence: { status: 'not-checked', reason: 'not-requested' }, license_playback_verified: false, ...extra });
test('native evidence never pretends to establish decoded equality', () => {
  const r = load();
  assert.equal(r.verificationPassed(nativeVerification()), true);
  assert.equal(r.verificationPassed(nativeVerification({ required_depth: 'decoded' })), false);
  assert.equal(r.verificationPassed(nativeVerification({ failure: 'child-check-failed' })), false);
  assert.equal(r.verificationPassed(nativeVerification({ decode_evidence: { status: 'content-compared', equal: false } })), false);
  assert.equal(r.verificationPassed(nativeVerification({ decode_evidence: { status: 'source-decoded' } })), false);
  assert.equal(r.verificationPassed(nativeVerification({ required_depth: 'decoded', decode_evidence: { status: 'content-compared', equal: true } })), true);
  assert.equal(r.verificationPassed(nativeVerification({ required_depth: 'decoded', scope: 'source-admission', decode_evidence: { status: 'source-decoded' } })), true);
});
test('current reports and plans reach the overall evidence predicate with their actual scope', () => {
  const r = load();
  for (const schema of [1, 2, 3, 4, 5, 6, 7, 8, 9, 10]) {
    const report = { schema_version: schema, plan_id: 'a'.repeat(64) };
    if (schema >= 3) report.required_depth = 'native';
    assert.equal(r.evidencePassed({ report_kind: 'plan', report }), true);
    report.required_depth = schema >= 3 ? 'unknown' : 'native';
    assert.equal(r.evidencePassed({ report_kind: 'plan', report }), false);
  }
  const source = nativeVerification({ scope: 'source-admission' });
  const analysis = { report_kind: 'analysis', report: { schema_version: 2, input_admitted: true, verification: source } };
  assert.equal(r.evidencePassed(analysis), true);
  source.scope = 'comparison';
  assert.equal(r.evidencePassed(analysis), false);
  const child = { id: 'clear', output_matches_plan: true, verification: nativeVerification(), signaling: { systems: [], verified: false }, conformance: null };
  const bundle = { report_kind: 'verification', report: { schema_version: 2, passed: true, source_unchanged: true, outputs: [child] } };
  assert.equal(r.evidencePassed(bundle), true);
  child.verification = verification();
  assert.equal(r.evidencePassed(bundle), false);
  child.verification = nativeVerification({ checks: { stream_metadata: 'pass', packet_payloads_equal: true, packet_timing_equal: false } });
  assert.equal(r.evidencePassed(bundle), false);
  const conversion = { report_kind: 'conversion', report: { schema_version: 2, verification: nativeVerification() } };
  assert.equal(r.evidencePassed(conversion), true);
  conversion.report.status = 'failed';
  assert.equal(r.evidencePassed(conversion), false);
  delete conversion.report.status;
  conversion.report.verification.failure = 'child-check-failed';
  assert.equal(r.evidencePassed(conversion), false);
  delete conversion.report.verification;
  assert.equal(r.evidencePassed(conversion), false);
});
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

const nativeFailureCodes = [
  "source-layout-path-unsafe",
  "source-layout-collision",
  "source-layout-not-supported",
  "output-options-invalid",
  "execution-plan-output-options-changed",
  "dash-addressing-requires-dash",
  "dash-addressing-source-not-supported",
  "dash-nominal-duration-not-representable",
  "dash-nominal-time-template-not-supported",
  "dash-indexed-absolute-fragment-offset-not-supported",
  "dash-index-sap-not-supported",
  "dash-index-timeline-not-representable",
  "dash-index-not-representable",
  "dash-index-timescale-not-supported",
  "dash-presentation-origin-timescale-not-supported",
  "dash-indexed-output-collision",
  "invalid-avc-syntax",
  "native-aac-clock-mismatch",
  "native-aac-config-invalid",
  "native-aac-config-not-supported",
  "native-avc-access-unit-size-limit",
  "native-avc-sync-sample-mismatch",
  "native-cipher-counter-overflow",
  "native-codec-configuration-not-supported",
  "native-codec-profile-not-supported",
  "native-media-clock-overflow",
  "native-mp4-invalid",
  "native-mp4-profile-not-supported",
  "native-presentation-metadata-invalid",
  "native-presentation-window-not-supported",
  "native-sample-evidence-incomplete",
  "native-sample-evidence-invalid",
  "native-sample-protection-mismatch",
  "native-source-changed",
  "native-track-changed",
  "native-video-dimensions-mismatch",
  "presentation-window-outside-media",
  "ts-aac-mp4-invalid",
  "ts-aac-mp4-write-failed",
  "ts-avc-picture-duration-not-supported",
  "ts-avc-separate-colour-plane-not-supported",
  "ts-avc-slice-groups-not-supported",
  "ts-avc-slice-order-not-supported",
  "ts-segment-evidence-incomplete",
  "ts-segment-evidence-limit",
  "ts-segment-evidence-missing",
  "ts-segment-evidence-unfinished",
  "ts-video-duration-clock-required",
  "ts-video-reorder-limit",
  "verification-native-evidence-required"
]
;
test('native refusal codes stay closed and visible across Rust and JavaScript', () => {
  const r = load();
  const rust = fs.readFileSync(path.join(__dirname, '../../src/common/projection.rs'), 'utf8');
  const allowed = new Set(rust.split('const FAILURE_CODES: &[&str] = &[', 2)[1].split('];', 1)[0].match(/"[a-z0-9-]+"/g).map(value => JSON.parse(value)));
  for (const code of nativeFailureCodes) {
    assert.match(code, /^[a-z0-9][a-z0-9-]{0,127}$/);
    assert.ok(allowed.has(code), code);
    assert.equal(r.errorInfo({ code, hint: 'PRIVATE_NATIVE_DETAIL' }).code, code);
    const text = renderResult({ report_kind: 'failure', operation: 'analyze', report: {
      schema_version: 1, status: 'failed', error: { code, hint: 'PRIVATE_NATIVE_DETAIL', last_activity: 'verification' }
    } });
    assert.ok(text.includes(code), code);
    assert.ok(!text.includes('PRIVATE_NATIVE_DETAIL'));
  }
  for (const code of ['native-private-sentinel', 'source-layout-private-sentinel', 'dash-private-sentinel', 'ts-private-sentinel', 'observer-stopped', 'native-source-changed/private', 'x'.repeat(129)]) {
    assert.equal(r.errorInfo({ code, hint: 'PRIVATE_NATIVE_DETAIL' }).code, null);
  }
});

test('known failure codes have focused guidance without exposing arbitrary server text', () => {
  const r = load();
  assert.equal(r.errorInfo({ code: 'execution-plan-source-changed', hint: 'secret' }).key, 'commonSourceChanged');
  assert.equal(r.errorInfo({ code: 'invalid-encrypted-segment', hint: 'secret' }).key, 'commonDecryptionFailed');
  assert.equal(r.errorInfo({ code: 'manifest-validator-unavailable' }).key, 'commonValidatorUnavailable');
  assert.equal(r.errorInfo({ code: 'dash-nominal-duration-not-representable', hint: 'secret' }).key, 'commonDashTimelineRequired');
  assert.equal(r.errorInfo({ code: 'source-layout-collision', hint: 'secret' }).key, 'commonUnsupported');
  assert.equal(r.errorInfo({ code: 'private-sentinel', hint: 'secret' }).code, null);
});

test('resolved processing is rendered from fixed labels and legacy absence stays unclaimed', () => {
  const processing = { route: 'fmp4-segments', mode: 'segments', boundaries: 'preserve' };
  const r = renderer();
  r.render({ report_kind: 'conversion', operation: 'convert', report: { schema_version: 2, processing, verification: nativeVerification() } });
  assert.match(r.text(), /commonProcessingSegments/); assert.match(r.text(), /commonBoundaryPreserve/);
  assert.equal(r.all(r.host).find(node => node.getAttribute?.('data-common-processing-mode')).getAttribute('data-common-processing-mode'), 'segments');
  for (const value of ['PRIVATE_VALUE', '__proto__']) {
    const text = renderResult({ report_kind: 'conversion', report: { schema_version: 2,
      processing: { route: value, mode: value, boundaries: value }, verification: nativeVerification() } });
    assert.ok(!text.includes(value)); assert.ok(!text.includes('commonBoundaryPreserve'));
  }
  const legacy = renderResult({ report_kind: 'conversion', report: { schema_version: 2,
    processing: { route: 'fmp4-hls-presentation' }, verification: nativeVerification() } });
  assert.ok(!legacy.includes('commonBoundaryResegment'));
  assert.ok(!legacy.includes('commonProcessingStitch'));
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
test('the rendered decode badge states not checked rather than pass or failure', () => {
  const r = renderer();
  r.render({ report_kind: 'conversion', operation: 'convert', report: { schema_version: 2, verification: nativeVerification() } });
  const decode = r.all(r.host).find(node => node.attrs && node.attrs.get('data-common-check') === 'decode_evidence');
  assert.ok(decode);
  assert.equal(decode.attrs.get('data-common-state'), 'not-checked');
  assert.match(r.text(), /commonNotChecked/);
  assert.doesNotMatch(r.text(), /commonMediaVerified/);
});
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
