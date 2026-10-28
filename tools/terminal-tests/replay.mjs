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
const dashboard = /^(?:drmr convert\b|Source\b|Overall\b|Renditions\b|[SO] #\d)/;

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
      buffer: terminal.buffer.active.type };
  };
  // Observe complete paint transactions, including a delimiter split across reads.
  const ends = new Set([...raw.toString('latin1').matchAll(/\x1b\[\?2026l/g)]
    .map(match => match.index + match[0].length));
  const frames = [];
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
        const boundaries = [...ends].filter(offset => offset > consumed && offset <= end).sort((a, b) => a - b);
        if (!boundaries.includes(end)) boundaries.push(end);
        for (const boundary of boundaries) {
          await write(raw.subarray(consumed, boundary));
          consumed = boundary;
          if (ends.has(boundary) && terminal.buffer.active.type === 'alternate') {
            frames.push({ offset: boundary, elapsed_seconds: event.elapsed_seconds, ...frame() });
          }
        }
      } else throw new Error('capture-event-kind');
    }
    if (consumed !== raw.length) throw new Error('capture-byte-total');
    const final = frame();
    const normal = allNormalRows();
    const summary = capture.outcome === 'success' ? /^Finished convert in / :
      capture.outcome === 'cancel' ? /^Cancelled convert in / : /^Failed convert in /;
    const assertions = {
      completed_dashboard_frames: frames.length >= 3,
      no_duplicate_headers: frames.every(value => value.header_count === 1),
      parents_visible: frames.every(value => value.source_count === 1 && value.overall_count === 1),
      fixed_bar_column: frames.every(value => value.columns < 80 || value.bar_columns.every(column => column === 13)),
      observed_ascii_bars: frames.some(value => value.bar_columns.length) && frames.every(value => value.ascii_bars),
      final_dashboard_clear: final.dashboard_rows === 0,
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
