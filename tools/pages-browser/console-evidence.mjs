import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';

// Do not turn omitted decoding into successful decoder execution or equality.
export function assertVerification(report, { scope = 'comparison', depth = 'decoded' } = {}) {
  assert.ok(['source-admission', 'comparison'].includes(scope));
  assert.equal(report.schema_version, 2);
  assert.equal(report.scope, scope);
  assert.equal(report.required_depth, depth);
  assert.equal(report.passed, true);
  assert.equal(Object.hasOwn(report, 'failure'), false);
  assert.equal(Object.hasOwn(report, 'media_verified'), false);
  assert.equal(report.license_playback_verified, false);
  assert.deepEqual(report.checks, { stream_metadata: 'pass', packet_payloads_equal: true, packet_timing_equal: true });
  if (depth === 'native') {
    assert.deepEqual(report.decode_evidence, { status: 'not-checked', reason: 'not-requested' });
  } else {
    assert.equal(depth, 'decoded');
    assert.deepEqual(report.decode_evidence, scope === 'source-admission'
      ? { status: 'source-decoded' } : { status: 'content-compared', equal: true });
  }
  if (report.native_manifest) {
    assert.equal(report.native_manifest.errors, 0);
    assert.ok(Number.isSafeInteger(report.native_manifest.warnings) && report.native_manifest.warnings >= 0);
    assert.equal(report.native_manifest.incomplete, false);
    assert.equal(report.native_manifest.truncated, false);
  }
}

export function decodeBadgeState(report) {
  const evidence = report.decode_evidence;
  if (evidence?.status === 'not-checked') return 'not-checked';
  if (evidence?.status === 'source-decoded') return report.scope === 'source-admission' ? 'pass' : 'fail';
  if (evidence?.status === 'content-compared') return report.scope === 'comparison' && evidence.equal === true ? 'pass' : 'fail';
  return 'fail';
}

export function planMinimumDepth(plan) {
  assert.equal(plan.kind, 'execution-plan');
  assert.ok([1, 2, 3, 4].includes(plan.schema_version));
  if (plan.schema_version < 3) {
    assert.equal(Object.hasOwn(plan, 'required_depth'), false);
    return 'decoded';
  }
  assert.ok(['native', 'decoded'].includes(plan.required_depth));
  return plan.required_depth;
}

// Retain real legacy-import coverage without keeping media tied to an old binary.
// Field order matches the version-one/two Rust Identity structures; removing the
// policy field strengthens a native plan to the legacy complete-decode minimum.
export function legacyPlan(current) {
  planMinimumDepth(current);
  assert.ok([3, 4].includes(current.schema_version));
  const plan = { kind: current.kind, schema_version: current.schema_version - 2, plan_id: '',
    source: current.source, source_format: current.source_format };
  if (current.schema_version === 3) plan.intent = current.intent;
  else { plan.output_root = current.output_root; plan.outputs = current.outputs; }
  const { plan_id: ignored, ...identity } = plan;
  plan.plan_id = createHash('sha256').update(JSON.stringify(identity)).digest('hex');
  return plan;
}
