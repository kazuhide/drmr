import test from 'node:test';
import assert from 'node:assert/strict';
import { replayCapture } from './replay.mjs';

const paint = '\x1b[?2026h\x1b[H\x1b[2Jdrmr convert\r\nSource Ready\r\nOverall      [----------]\x1b[?2026l';
function capture(body) {
  const raw = Buffer.from(body);
  return { columns: 120, rows: 24, outcome: 'success', raw: raw.toString('base64'),
    events: [{ kind: 'write', offset: 0, length: raw.length, elapsed_seconds: 0 }] };
}
const healthy = '\r\n\x1b[?1049h' + paint.repeat(3) + '\x1b[?1049lFinished convert in 1.0s\r\n';

test('complete alternate-screen paints preserve history and restore the terminal', async () => {
  assert.equal((await replayCapture(capture(healthy))).passed, true);
});
test('inline rows left above the result are refused', async () => {
  const result = await replayCapture(capture('drmr convert\r\nSource Ready\r\nOverall [----------]\r\nFinished convert in 1.0s'));
  assert.equal(result.assertions.final_dashboard_clear, false);
});
test('overwriting normal-screen history is refused', async () => {
  const result = await replayCapture(capture('\x1b[H\x1b[2J' + healthy));
  assert.equal(result.assertions.original_history_preserved, false);
});
test('a hidden cursor at completion is refused', async () => {
  const result = await replayCapture(capture(healthy + '\x1b[?25l'));
  assert.equal(result.assertions.final_cursor_visible, false);
});
test('duplicate headers in a completed paint are refused', async () => {
  const duplicate = paint.replace('Source Ready', 'drmr convert\r\nSource Ready');
  const result = await replayCapture(capture(healthy.replace(paint, duplicate)));
  assert.equal(result.assertions.no_duplicate_headers, false);
});
test('resize within a split paint is evaluated after its final delimiter', async () => {
  const value = capture(healthy);
  value.require_mid_draw = true;
  const offset = healthy.indexOf('drmr convert') + 6;
  value.events = [
    { kind: 'write', offset: 0, length: offset, elapsed_seconds: 0 },
    { kind: 'resize', offset, columns: 80, rows: 12, elapsed_seconds: 1 },
    { kind: 'write', offset, length: Buffer.from(healthy).length - offset, elapsed_seconds: 2 },
  ];
  const result = await replayCapture(value);
  assert.equal(result.passed, true);
  assert.equal(result.frames.length, 3);
});
test('non-ASCII progress bar contents are refused', async () => {
  const result = await replayCapture(capture(healthy.replaceAll('[----------]', '[████------]')));
  assert.equal(result.assertions.observed_ascii_bars, false);
});
test('a resize outside a paint cannot satisfy the controlled-race requirement', async () => {
  const value = capture(healthy);
  value.require_mid_draw = true;
  value.events.unshift({ kind: 'resize', offset: 0, columns: 80, rows: 12, elapsed_seconds: 0 });
  const result = await replayCapture(value);
  assert.equal(result.assertions.resize_during_synchronized_paint, false);
});
