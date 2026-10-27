import { test } from 'node:test';
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { assertVerification, compatibilityPlan, decodeBadgeState, legacyPlan, planMinimumDepth } from './console-evidence.mjs';

function native(scope = 'comparison') {
  return { schema_version: 2, required_depth: 'native', scope, passed: true,
    checks: { stream_metadata: 'pass', packet_payloads_equal: true, packet_timing_equal: true },
    decode_evidence: { status: 'not-checked', reason: 'not-requested' }, license_playback_verified: false };
}
function decoded(scope = 'comparison') {
  return { ...native(scope), required_depth: 'decoded', decode_evidence: scope === 'comparison'
    ? { status: 'content-compared', equal: true } : { status: 'source-decoded' } };
}
test('console media evidence keeps native coverage distinct from actual decoding', () => {
  for (const scope of ['comparison', 'source-admission']) {
    assert.doesNotThrow(() => assertVerification(native(scope), { scope, depth: 'native' }));
    assert.doesNotThrow(() => assertVerification(decoded(scope), { scope }));
    assert.throws(() => assertVerification(native(scope), { scope }));
    assert.throws(() => assertVerification(decoded(scope), { scope, depth: 'native' }));
  }
  assert.equal(decodeBadgeState(native()), 'not-checked');
  assert.equal(decodeBadgeState(decoded()), 'pass');
  assert.equal(decodeBadgeState(decoded('source-admission')), 'pass');
  assert.equal(decodeBadgeState({ ...decoded(), decode_evidence: { status: 'content-compared', equal: false } }), 'fail');
  assert.equal(decodeBadgeState({ ...decoded(), decode_evidence: { status: 'source-decoded' } }), 'fail');
});
test('console media evidence refuses failed native checks, false decode and stale schema claims', () => {
  const mutations = [value => { value.passed = false; }, value => { value.schema_version = 1; },
    value => { value.checks.stream_metadata = 'fail'; }, value => { value.checks.packet_payloads_equal = false; },
    value => { value.checks.packet_timing_equal = false; }, value => { value.checks.decoded_content_equal = true; },
    value => { value.decode_evidence.equal = false; }, value => { value.decode_evidence = { status: 'source-decoded' }; },
    value => { value.decode_evidence = { status: 'not-checked', reason: 'not-requested' }; },
    value => { value.failure = 'child-check-failed'; }, value => { value.media_verified = true; },
    value => { value.license_playback_verified = true; }, value => { value.required_depth = 'unknown'; },
    value => { value.native_manifest = { errors: 1, warnings: 0, truncated: false, incomplete: false }; }];
  for (const mutate of mutations) {
    const value = decoded(); mutate(value);
    assert.throws(() => assertVerification(value));
  }
});
test('console plan fixtures preserve legacy minimum decoding and hash the original identity fields', () => {
  const source = { entrypoint: 'source.mp4', graph_digest: 'b'.repeat(64), files: [] };
  const single = { kind: 'execution-plan', schema_version: 3, plan_id: 'a'.repeat(64), source,
    source_format: 'mp4', intent: { output: 'result.mp4', format: 'mp4', target: 'clear' }, required_depth: 'native' };
  const bundle = { kind: 'execution-plan', schema_version: 4, plan_id: 'c'.repeat(64), source,
    source_format: 'mp4', output_root: 'result', outputs: [{ id: 'dash', format: 'dash', target: 'clear', drms: [], conformance: 'none' }],
    required_depth: 'decoded' };
  const selection = { requested: 'auto', resolved: 'stitch', boundaries: 'not-applicable', segment_duration_ms: null };
  const currentSingle = { ...single, schema_version: 5, processing: selection };
  const currentBundle = { ...bundle, schema_version: 6, outputs: [{ ...bundle.outputs[0], processing: 'auto' }],
    processing: [{ id: 'dash', selection: { ...selection, boundaries: 'resegment', segment_duration_ms: 2000 } }] };
  for (const current of [single, bundle, currentSingle, currentBundle]) {
    const before = structuredClone(current), legacy = legacyPlan(current);
    assert.deepEqual(current, before);
    assert.equal(legacy.schema_version, current.schema_version % 2 === 1 ? 1 : 2);
    assert.equal(Object.hasOwn(legacy, 'required_depth'), false);
    assert.equal(Object.hasOwn(legacy, 'processing'), false);
    if (legacy.outputs) assert.ok(legacy.outputs.every(output => !Object.hasOwn(output, 'processing')));
    assert.equal(planMinimumDepth(legacy), 'decoded');
    assert.equal(planMinimumDepth(current), current.required_depth);
    const { plan_id: ignored, ...identity } = legacy;
    assert.equal(legacy.plan_id, createHash('sha256').update(JSON.stringify(identity)).digest('hex'));
    assert.notEqual(legacy.plan_id, current.plan_id);
  }
  for (const changed of [{ ...single, required_depth: null }, { ...single, required_depth: 'unknown' },
    { ...single, schema_version: 1 }, { ...bundle, schema_version: 2 }, { ...single, schema_version: 5 }]) {
    assert.throws(() => planMinimumDepth(changed));
  }
});

test('new saved options remain current while real older fixtures retain their original identity fields', () => {
  const selection = { requested: 'auto', resolved: 'stitch', boundaries: 'not-applicable', segment_duration_ms: null };
  const single = { kind: 'execution-plan', schema_version: 7, plan_id: 'a'.repeat(64), source: {}, source_format: 'mp4',
    intent: { format: 'mp4', target: 'clear', output: 'result.mp4' }, required_depth: 'native', processing: selection, output_options: {} };
  const bundle = { kind: 'execution-plan', schema_version: 8, plan_id: 'b'.repeat(64), source: {}, source_format: 'hls', output_root: 'result',
    outputs: [{ id: 'dash', format: 'dash', target: 'clear', drms: [], conformance: 'none', processing: 'auto', dash_addressing: 'base' }],
    required_depth: 'decoded', processing: [{ id: 'dash', selection: { ...selection, boundaries: 'resegment', segment_duration_ms: 6000 } }],
    output_options: [{ id: 'dash', options: { dash_addressing: 'base' } }] };
  for (const current of [single, bundle, { ...single, schema_version: 9 }, { ...bundle, schema_version: 10 }]) {
    const before = structuredClone(current), single = current.schema_version % 2 === 1;
    assert.equal(planMinimumDepth(current), current.required_depth);
    for (const schema of single ? [1, 3, 5] : [2, 4, 6]) {
      const old = compatibilityPlan(current, schema);
      assert.equal(old.schema_version, schema);
      assert.equal(Object.hasOwn(old, 'output_options'), false);
      if (schema >= 5) assert.deepEqual(old.processing, current.processing);
      else assert.equal(Object.hasOwn(old, 'processing'), false);
      if (old.outputs) assert.ok(old.outputs.every(output => !Object.hasOwn(output, 'dash_addressing')));
      const { plan_id: ignored, ...identity } = old;
      assert.equal(old.plan_id, createHash('sha256').update(JSON.stringify(identity)).digest('hex'));
    }
    assert.deepEqual(current, before);
    const missing = { ...current }; delete missing.output_options;
    assert.throws(() => planMinimumDepth(missing));
  }
});
