import { createHash } from 'node:crypto';
import { readFileSync } from 'node:fs';
import { createRequire } from 'node:module';
import { pathToFileURL } from 'node:url';

const require = createRequire(import.meta.url);
const { Terminal } = require('@xterm/headless');
const engine = require('@xterm/headless/package.json');
if (engine.version !== '6.0.0') throw new Error('terminal-engine-version');
const history = 'Output before conversion';
const sha256 = bytes => createHash('sha256').update(bytes).digest('hex');
const dashboard = /^\s*(?:drmr convert\b|Source\b|Overall\b|Renditions\b|(?:S|[1-9]\d*) (?:#\d+|(?:Video|Audio|Text) \d+|Output\b|DASH\b|HLS\b|MP4\b))/;
const expectedFormats = ['DASH fMP4', 'HLS fMP4'];
const outputRow = /^([12]) (Output|DASH fMP4|HLS fMP4) +(.+)$/;
const childRow = /^(?:S|[12]) (?:#[12]|(?:Video|Audio|Text) [12]) +(?:Waiting|Active|Done \(step\)|Stopped)(?: +.*)?$/;
const hiddenRow = /^\d+ hidden: \d+ active, \d+ waiting$/;

function paintLayout(text) {
  const lines = text.filter(line => line.trim());
  const parents = lines.slice(3, 5).map(line => line.match(outputRow));
  const introduced = parents.some(Boolean);
  const ordered = introduced && parents.every((match, index) => match && Number(match[1]) === index + 1 &&
    (match[2] === 'Output' || match[2] === expectedFormats[index]));
  const tail = lines.slice(introduced ? 5 : 3);
  return { output_parents: ordered ? parents.map(match => match[2]) : [],
    source_ready: /^Source +Ready\b/.test(lines[1] || ''),
    valid_layout: /^drmr convert\b/.test(lines[0] || '') && /^Source +\S/.test(lines[1] || '') &&
      /^Overall +\S/.test(lines[2] || '') && (!introduced || ordered) &&
      tail.every(line => line === 'Renditions' || childRow.test(line) || hiddenRow.test(line)) };
}

function primaryGrammar(normal, snapshots, capture) {
  const label = { success: 'Finished', cancel: 'Cancelled', failure: 'Failed' }[capture.outcome];
  const code = capture.outcome === 'success' ? '' : capture.error_code;
  if (!label || (capture.outcome !== 'success' && !/^[a-z0-9-]+$/.test(code || ''))) return false;
  const footer = normal.filter(line => new RegExp('^' + label + ' convert in \\d+\\.\\ds' +
    (code ? ' \\| ' + code : '') + '$').test(line));
  if (footer.length !== 1) return false;
  const expected = [history, footer[0]];
  if (capture.outcome !== 'success') {
    if (capture.stop_phase === 'source') expected.push('Source       Stopped');
    else expected.push('Source       Checked', '1 DASH fMP4  Stopped', '2 Output     Not started');
  }
  if (normal.length !== expected.length || normal.some((line, index) => line !== expected[index])) return false;
  // Every visible primary-screen prefix must belong to the final transcript. This
  // also rejects stale rows which a later cursor move or erase happens to hide.
  return snapshots.every(lines => lines[0] === history && lines.length <= expected.length && lines.every((line, index) =>
    index === lines.length - 1 ? expected[index].startsWith(line) : expected[index] === line));
}

export async function replayCapture(capture) {
  const raw = Buffer.from(capture.raw, 'base64');
  if (raw.length > 262144 || capture.events.length > 8192) throw new Error('capture-limit');
  const terminal = new Terminal({ cols: capture.columns, rows: capture.rows,
    allowProposedApi: true, scrollback: 1000, convertEol: false, logLevel: 'off' });
  let cursorVisible = true, alternateEntered = false, alternateLeft = false, synchronized = false;
  for (const final of ['h', 'l']) terminal.parser.registerCsiHandler({ prefix: '?', final }, parameters => {
    if (parameters.includes(25)) cursorVisible = final === 'h';
    if (parameters.includes(1049)) {
      if (final === 'h') alternateEntered = true;
      else alternateLeft = true;
    }
    if (parameters.includes(2026)) synchronized = final === 'h';
    return false;
  });
  const write = bytes => new Promise(resolve => terminal.write(bytes, resolve));
  const rows = () => {
    const buffer = terminal.buffer.active;
    return Array.from({ length: terminal.rows }, (_, index) =>
      buffer.getLine(buffer.viewportY + index)?.translateToString(true) ?? '');
  };
  const allNormalRows = () => {
    const buffer = terminal.buffer.normal;
    return Array.from({ length: buffer.length }, (_, index) =>
      buffer.getLine(index)?.translateToString(true) ?? '');
  };
  const frame = () => {
    const text = rows();
    const bars = text.flatMap(line => [...line.matchAll(/\[[^\]\r\n]*\]/g)]
      .map(match => ({ column: match.index, ascii: /^[\x20-\x7e]+$/.test(match[0]) })));
    return { columns: terminal.cols, rows: terminal.rows,
      header_count: text.filter(line => /^drmr convert\b/.test(line)).length,
      source_count: text.filter(line => /^Source\b/.test(line)).length,
      overall_count: text.filter(line => /^Overall\b/.test(line)).length,
      dashboard_rows: text.filter(line => dashboard.test(line)).length,
      bar_columns: bars.map(bar => bar.column), ascii_bars: bars.every(bar => bar.ascii),
      buffer: terminal.buffer.active.type, ...paintLayout(text) };
  };
  // Observe complete paint transactions, including a delimiter split across reads.
  const ends = new Set([...raw.toString('latin1').matchAll(/\x1b\[\?2026l/g)]
    .map(match => match.index + match[0].length));
  const controls = [...raw.toString('latin1').matchAll(/\x1b\[[0-?]*[ -/]*[@-~]|[\r\n\b\t]/g)];
  const cuts = new Set([...ends, ...controls.flatMap(match => [match.index, match.index + match[0].length])]);
  const frames = [];
  const primarySnapshots = [];
  let consumed = 0, resizedDuringPaint = false;
  try {
    await write(history + '\r\n');
    for (const event of capture.events) {
      if (event.offset !== consumed) throw new Error('capture-byte-order');
      if (event.kind === 'resize') {
        if (synchronized && terminal.buffer.active.type === 'alternate') resizedDuringPaint = true;
        terminal.resize(event.columns, event.rows);
      } else if (event.kind === 'write') {
        const end = consumed + event.length;
        if (end > raw.length) throw new Error('capture-byte-length');
        const boundaries = [...cuts].filter(offset => offset > consumed && offset <= end).sort((a, b) => a - b);
        if (!boundaries.includes(end)) boundaries.push(end);
        for (const boundary of boundaries) {
          await write(raw.subarray(consumed, boundary));
          consumed = boundary;
          if (terminal.buffer.active.type === 'normal') {
            primarySnapshots.push(allNormalRows().filter(line => line.trim()));
            if (primarySnapshots.length > 1024) throw new Error('primary-observation-limit');
          }
          if (ends.has(boundary) && terminal.buffer.active.type === 'alternate') {
            frames.push({ offset: boundary, elapsed_seconds: event.elapsed_seconds, ...frame() });
          }
        }
      } else throw new Error('capture-event-kind');
    }
    if (consumed !== raw.length) throw new Error('capture-byte-total');
    const final = frame();
    const normal = allNormalRows().filter(line => line.trim());
    const summary = capture.outcome === 'success' ? /^Finished convert in / :
      capture.outcome === 'cancel' ? /^Cancelled convert in / : /^Failed convert in /;
    let introduced = false;
    const formats = [null, null];
    const orderedParents = frames.every(value => {
      if (value.output_parents.length === 0) return !introduced;
      introduced = true;
      return value.output_parents.every((format, index) => {
        if (formats[index] && format !== formats[index]) return false;
        if (format !== 'Output') formats[index] = format;
        return true;
      }) && (value.output_parents.every(format => format === 'Output') || value.source_ready);
    });
    const transcriptValid = primaryGrammar(normal, primarySnapshots, capture);
    const sourceStop = capture.outcome !== 'success' && capture.stop_phase === 'source';
    const assertions = {
      completed_dashboard_frames: frames.length >= 3,
      no_duplicate_headers: frames.every(value => value.header_count === 1),
      parents_visible: frames.every(value => value.source_count === 1 && value.overall_count === 1),
      fixture_output_parents: (sourceStop ? !introduced && frames.every(value => !value.source_ready) : introduced) &&
        orderedParents && frames.every(value => value.valid_layout) &&
        (capture.outcome !== 'success' || formats.every((format, index) => format === expectedFormats[index])),
      fixed_bar_column: frames.every(value => value.columns < 80 || value.bar_columns.every(column => column === 13)),
      observed_ascii_bars: frames.some(value => value.bar_columns.length) && frames.every(value => value.ascii_bars),
      final_dashboard_clear: transcriptValid,
      primary_transcript_valid: transcriptValid,
      original_history_preserved: normal.filter(line => line === history).length === 1,
      final_summary_present: rows().some(line => summary.test(line)),
      final_normal_buffer: final.buffer === 'normal' && alternateEntered && alternateLeft,
      final_cursor_visible: cursorVisible,
      synchronized_output_closed: !synchronized,
      resize_during_synchronized_paint: !capture.require_mid_draw || resizedDuringPaint,
      no_color_respected: !capture.no_color || !/\x1b\[[0-9;]*m/.test(raw.toString('latin1')),
    };
    return { passed: Object.values(assertions).every(Boolean), assertions, frames, final,
      engine: { name: engine.name, version: engine.version, node: process.version,
        parser_sha256: sha256(readFileSync(require.resolve('@xterm/headless'))) },
      raw_sha256: sha256(raw), bytes: raw.length, events: capture.events };
  } finally { terminal.dispose(); }
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  try {
    const chunks = []; let count = 0;
    for await (const chunk of process.stdin) {
      count += chunk.length;
      if (count > 1048576) throw new Error('capture-input-limit');
      chunks.push(chunk);
    }
    console.log(JSON.stringify(await replayCapture(JSON.parse(Buffer.concat(chunks)))));
  } catch {
    console.log(JSON.stringify({ passed: false, error: 'terminal-replay-invalid' }));
    process.exitCode = 1;
  }
}
