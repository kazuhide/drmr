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
  assert.ok([1, 2, 3, 4, 5, 6, 7, 8, 9, 10].includes(plan.schema_version));
  if (plan.schema_version < 3) {
    assert.equal(Object.hasOwn(plan, 'required_depth'), false);
    return 'decoded';
  }
  assert.ok(['native', 'decoded'].includes(plan.required_depth));
  if (plan.schema_version >= 5) {
    const selections = plan.schema_version % 2 === 1 ? [plan.processing] : plan.processing?.map(row => row.selection);
    assert.ok(Array.isArray(selections) && selections.length > 0);
    for (const selection of selections) {
      assert.ok(selection && ['auto', 'segments', 'stitch'].includes(selection.requested));
      assert.ok(['segments', 'stitch'].includes(selection.resolved));
      assert.ok(['preserve', 'resegment', 'not-applicable'].includes(selection.boundaries));
      assert.ok(selection.requested === 'auto' || selection.requested === selection.resolved);
    }
  }
  if (plan.schema_version >= 7) {
    const single = plan.schema_version % 2 === 1, options = single ? [plan.output_options] : plan.output_options?.map(row => row.options);
    assert.ok(Array.isArray(options) && options.length === (single ? 1 : plan.outputs.length));
    for (const [index, option] of options.entries()) {
      assert.ok(option && typeof option === 'object' && !Array.isArray(option));
      const format = single ? plan.intent.format : plan.outputs[index].format;
      if (!single) assert.equal(plan.output_options[index].id, plan.outputs[index].id);
      if (format === 'dash') assert.ok(['template', 'template-timeline', 'list', 'base'].includes(option.dash_addressing));
      else assert.equal(Object.hasOwn(option, 'dash_addressing'), false);
    }
  }
  return plan.required_depth;
}

// Retain real legacy-import coverage without keeping media tied to an old binary.
// Field order matches the version-one/two Rust Identity structures; removing the
// policy field strengthens a native plan to the legacy complete-decode minimum.
export function legacyPlan(current) {
  return compatibilityPlan(current, current.schema_version % 2 === 1 ? 1 : 2);
}
export function compatibilityPlan(current, schemaVersion) {
  planMinimumDepth(current);
  assert.ok([3, 4, 5, 6, 7, 8, 9, 10].includes(current.schema_version));
  const single = current.schema_version % 2 === 1;
  assert.ok((single ? [1, 3, 5] : [2, 4, 6]).includes(schemaVersion) && schemaVersion <= current.schema_version);
  const plan = { kind: current.kind, schema_version: schemaVersion, plan_id: '',
    source: current.source, source_format: current.source_format };
  if (single) plan.intent = current.intent;
  else { plan.output_root = current.output_root; plan.outputs = current.outputs.map(({ processing, dash_addressing: ignored, ...output }) => schemaVersion >= 6 && processing !== undefined ? { ...output, processing } : output); }
  if (schemaVersion >= 3) plan.required_depth = current.required_depth;
  if (schemaVersion >= 5) plan.processing = current.processing;
  const { plan_id: ignored, ...identity } = plan;
  plan.plan_id = createHash('sha256').update(JSON.stringify(identity)).digest('hex');
  return plan;
}
