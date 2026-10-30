import test from 'node:test';
import assert from 'node:assert/strict';
import { replayCapture } from './replay.mjs';

const paint = '\x1b[?2026h\rdrmr convert elapsed 1.0s\r\nSource Ready\r\nOverall      [----------]\r\n1 DASH fMP4  [----------] Waiting\r\n2 HLS fMP4   [----------] Waiting\r\n\x1b[?2026l';
const up = rows => `\x1b[${rows}F`;
const clear = rows => '\x1b[?2026h' + up(rows) + '\r\x1b[2K\r\n'.repeat(rows) + up(rows) + '\x1b[?2026l';
const paints = (value, rows = 5) => value + up(rows) + value + up(rows) + value + clear(rows);
function capture(body) {
  const raw = Buffer.from(body);
  return { columns: 120, rows: 24, outcome: 'success', raw: raw.toString('base64'),
    events: [{ kind: 'write', offset: 0, length: raw.length, elapsed_seconds: 0 }] };
}
const healthy = paints(paint) + 'Finished convert in 1.0s\r\n';

test('inline paints remain below the command and preserve primary-screen history', async () => {
  const result = await replayCapture(capture(healthy));
  assert.equal(result.passed, true);
  assert.equal(result.assertions.dashboard_below_command, true);
  assert.equal(result.assertions.no_alternate_screen, true);
});
test('stable geometry refuses earlier paints left above a cleared final paint', async () => {
  const body = paint.repeat(3) + clear(5) + 'Finished convert in 1.0s\r\n';
  assert.equal((await replayCapture(capture(body))).passed, false);
});
test('a no-op resize cannot authorize earlier paints left in history', async () => {
  const body = paint.repeat(3) + clear(5) + 'Finished convert in 1.0s\r\n';
  const value = capture(body);
  value.events = [
    { kind: 'write', offset: 0, length: Buffer.byteLength(paint), elapsed_seconds: 0 },
    { kind: 'resize', offset: Buffer.byteLength(paint), columns: 120, rows: 24, elapsed_seconds: 1 },
    { kind: 'write', offset: Buffer.byteLength(paint), length: Buffer.byteLength(body) - Buffer.byteLength(paint), elapsed_seconds: 2 },
  ];
  assert.equal((await replayCapture(value)).passed, false);
});
test('one actual resize does not authorize later stable-geometry residue', async () => {
  const body = paint.repeat(3) + clear(5) + 'Finished convert in 1.0s\r\n';
  const value = capture(body);
  value.events = [
    { kind: 'write', offset: 0, length: Buffer.byteLength(paint), elapsed_seconds: 0 },
    { kind: 'resize', offset: Buffer.byteLength(paint), columns: 80, rows: 24, elapsed_seconds: 1 },
    { kind: 'write', offset: Buffer.byteLength(paint), length: Buffer.byteLength(body) - Buffer.byteLength(paint), elapsed_seconds: 2 },
  ];
  assert.equal((await replayCapture(value)).passed, false);
});
test('incomplete unframed rows left above the result are refused', async () => {
  const result = await replayCapture(capture('drmr convert\r\nSource Ready\r\nOverall [----------]\r\nFinished convert in 1.0s'));
  assert.equal(result.assertions.final_dashboard_cleanup_safe, false);
});
test('overwriting normal-screen history is refused', async () => {
  const result = await replayCapture(capture('\x1b[H\x1b[2J' + healthy));
  assert.equal(result.assertions.original_history_preserved, false);
});
for (const control of ['\x1b[?1049h', '\x1b[?47h', '\x1b[?1047h', '\x1b[2J', '\x1b[3J', '\x1b[H', '\x1b[1;1f']) {
  test(`screen switching or absolute screen reset is refused: ${JSON.stringify(control)}`, async () => {
    assert.equal((await replayCapture(capture(control + healthy))).passed, false);
  });
}
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
test('a width change preserves the previous paint and restarts below it', async () => {
  const narrow = '\x1b[?2026h\rdrmr convert elapsed 1.0s\r\nSource Ready\r\nOverall work\r\n1 DASH fMP4 Waiting\r\n2 HLS fMP4 Waiting\r\n\x1b[?2026l';
  const body = paint + '\r\n' + narrow + up(5) + narrow + clear(5) + 'Finished convert in 1.0s\r\n';
  const value = capture(body);
  value.events = [
    { kind: 'write', offset: 0, length: Buffer.byteLength(paint), elapsed_seconds: 0 },
    { kind: 'resize', offset: Buffer.byteLength(paint), columns: 40, rows: 8, elapsed_seconds: 1 },
    { kind: 'write', offset: Buffer.byteLength(paint), length: Buffer.byteLength(body) - Buffer.byteLength(paint), elapsed_seconds: 2 },
  ];
  const result = await replayCapture(value);
  assert.equal(result.passed, true);
  assert.equal(result.assertions.original_history_preserved, true);
  assert.equal(result.frames[1].header_row > result.frames[0].header_row, true);
});
test('leaving the last live frame above the footer is refused', async () => {
  const body = paint + up(5) + paint + up(5) + paint + '\x1b[?2026h\x1b[?2026lFinished convert in 1.0s\r\n';
  assert.equal((await replayCapture(capture(body))).assertions.final_dashboard_cleanup_safe, false);
});
test('a resize inside the final paint retains uncertain rows without erasing history', async () => {
  const body = paint + up(5) + paint + up(5) + paint + '\x1b[?2026h\x1b[?2026l\r\nFinished convert in 1.0s\r\n';
  const value = capture(body);
  value.require_mid_draw = true;
  const offset = body.lastIndexOf('drmr convert') + 6;
  value.events = [
    { kind: 'write', offset: 0, length: offset, elapsed_seconds: 0 },
    { kind: 'resize', offset, columns: 80, rows: 12, elapsed_seconds: 1 },
    { kind: 'write', offset, length: Buffer.byteLength(body) - offset, elapsed_seconds: 2 },
  ];
  const result = await replayCapture(value);
  assert.equal(result.passed, true);
  assert.equal(result.assertions.original_history_preserved, true);
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

for (const residue of ['1 DASH fMP4 Stopped', '  1 DASH fMP4 Stopped', '2 HLS fMP4 Waiting',
  'S Video 1 Active', '1 Audio 2 Active', 'S #1 Active', '2 #2 Waiting', 'unexpected residue']) {
  test(`primary screen refuses residue: ${residue.trim()}`, async () => {
    assert.equal((await replayCapture(capture(healthy + residue + '\r\n'))).passed, false);
  });
}
test('primary residue erased later is still refused', async () => {
  const body = healthy.replace('Finished convert', '1 DASH fMP4 Stopped\r\x1b[2KFinished convert');
  assert.equal((await replayCapture(capture(body))).passed, false);
});
for (const [name, replacement] of [
  ['missing both output parents', paint.replace(/\r\n[12] (?:DASH|HLS)[^\x1b]*/g, '')],
  ['reordered output parents', paint.replace('1 DASH fMP4  [----------] Waiting', 'SWAP')
    .replace('2 HLS fMP4   [----------] Waiting', '1 DASH fMP4  [----------] Waiting')
    .replace('SWAP', '2 HLS fMP4   [----------] Waiting')],
  ['wrong output format', paint.replace('1 DASH fMP4  ', '1 HLS TS     ')],
  ['source checking during output processing', paint.replace('Source Ready', 'Source Checking')],
]) {
  test(`completed paint refuses ${name}`, async () => {
    assert.equal((await replayCapture(capture(healthy.replaceAll(paint, replacement)))).passed, false);
  });
}

const blockedPaint = paint.replace('2 HLS fMP4', '2 Output  ');
const stopped = 'Source       Checked\r\n1 DASH fMP4  Stopped\r\n2 Output     Not started\r\n';
function stoppedCapture(tail = stopped, outcome = 'cancel') {
  const footer = outcome === 'cancel' ? 'Cancelled convert in 4.0s | cancelled' :
    'Failed convert in 6.0s | media-tool-unavailable-or-timeout';
  const value = capture(paints(blockedPaint) + footer + '\r\n' + tail);
  value.outcome = outcome;
  value.error_code = outcome === 'cancel' ? 'cancelled' : 'media-tool-unavailable-or-timeout';
  return value;
}
for (const outcome of ['cancel', 'failure']) {
  test(`exact ${outcome} output-phase summary is allowed`, async () => {
    assert.equal((await replayCapture(stoppedCapture(stopped, outcome))).passed, true);
  });
}
for (const [name, tail] of [
  ['missing output', stopped.replace('2 Output     Not started\r\n', '')],
  ['duplicated source', stopped + 'Source       Checked\r\n'],
  ['wrong source status', stopped.replace('Source       Checked', 'Source       Stopped')],
  ['wrong output status', stopped.replace('1 DASH fMP4  Stopped', '1 DASH fMP4  Checked')],
  ['wrong format', stopped.replace('1 DASH fMP4 ', '1 Output    ')],
  ['indented summary', ' ' + stopped],
  ['live child', stopped + 'S #1 Active\r\n'],
  ['bar', stopped + 'Overall      [----------]\r\n'],
  ['header', stopped + 'drmr convert elapsed 4.0s\r\n'],
  ['reordered outputs', stopped.replace('1 Output', '3 Output').replace('2 Output', '1 Output').replace('3 Output', '2 Output')],
]) {
  test(`stopped summary refuses ${name}`, async () => {
    assert.equal((await replayCapture(stoppedCapture(tail))).passed, false);
  });
}
test('a source-phase stop allows only Source, before output parents are introduced', async () => {
  const value = stoppedCapture('Source       Stopped\r\n');
  const sourcePaint = '\x1b[?2026h\rdrmr convert elapsed 1.0s\r\nSource Preparing source media\r\nOverall      [----------]\r\n\x1b[?2026l';
  const body = paints(sourcePaint, 3) +
    'Cancelled convert in 4.0s | cancelled\r\nSource       Stopped\r\n';
  Object.assign(value, capture(body), { outcome: 'cancel', stop_phase: 'source', error_code: 'cancelled' });
  assert.equal((await replayCapture(value)).passed, true);
  const extra = Buffer.from(body + '1 Output     Not started\r\n');
  value.raw = extra.toString('base64'); value.events[0].length = extra.length;
  assert.equal((await replayCapture(value)).passed, false);
});
test('a source-only summary cannot hide already introduced output parents', async () => {
  const value = stoppedCapture('Source       Stopped\r\n');
  value.stop_phase = 'source';
  assert.equal((await replayCapture(value)).passed, false);
});
