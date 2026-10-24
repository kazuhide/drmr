import { test } from 'node:test';
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { assertVerification, decodeBadgeState, legacyPlan, planMinimumDepth } from './console-evidence.mjs';

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
  for (const current of [single, bundle]) {
    const before = structuredClone(current), legacy = legacyPlan(current);
    assert.deepEqual(current, before);
    assert.equal(legacy.schema_version, current.schema_version - 2);
    assert.equal(Object.hasOwn(legacy, 'required_depth'), false);
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
