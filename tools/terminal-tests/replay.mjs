import { createHash } from 'node:crypto';
import { readFileSync } from 'node:fs';
import { createRequire } from 'node:module';
import { pathToFileURL } from 'node:url';

const require = createRequire(import.meta.url);
const { Terminal } = require('@xterm/headless');
const engine = require('@xterm/headless/package.json');
if (engine.version !== '6.0.0') throw new Error('terminal-engine-version');
const history = ['Output before conversion', 'user@host $ drmr convert'];
const sha256 = bytes => createHash('sha256').update(bytes).digest('hex');
const dashboard = /^\s*(?:drmr convert\b|Source\b|Overall\b|Renditions\b|(?:S|[1-9]\d*) (?:#\d+|(?:Video|Audio|Text) \d+|Output\b|DASH\b|HLS\b|MP4\b))/;
const expectedFormats = ['DASH fMP4', 'HLS fMP4'];
const outputRow = /^([12]) (Output|DASH fMP4|HLS fMP4) +(.+)$/;
const childRow = /^(?:S|[12]) (?:#[12]|(?:Video|Audio|Text) [12]) +(?:Waiting|Active|Done \(step\)|Stopped)(?: +.*)?$/;
const hiddenRow = /^\d+ hidden: \d+ active, \d+ waiting$/;
const sameRows = (left, right) => left.length === right.length && left.every((line, index) => line === right[index]);

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

function primaryGrammar(normal, snapshots, base, capture) {
  const label = { success: 'Finished', cancel: 'Cancelled', failure: 'Failed' }[capture.outcome];
  const code = capture.outcome === 'success' ? '' : capture.error_code;
  if (!label || (capture.outcome !== 'success' && !/^[a-z0-9-]+$/.test(code || ''))) return false;
  const footer = normal.filter(line => new RegExp('^' + label + ' convert in \\d+\\.\\ds' +
    (code ? ' \\| ' + code : '') + '$').test(line));
  if (footer.length !== 1) return false;
  if (!base || history.some((line, index) => base[index] !== line)) return false;
  // Width changes can leave earlier complete paints in scrollback. They must
  // still be dashboard rows, rather than unowned output hidden by later erases.
  const earlier = base.slice(history.length);
  for (let start = 0; start < earlier.length;) {
    if (!/^drmr convert\b/.test(earlier[start])) return false;
    let end = start + 1;
    while (end < earlier.length && !/^drmr convert\b/.test(earlier[end])) end++;
    if (!paintLayout(earlier.slice(start, end)).valid_layout) return false;
    start = end;
  }
  const expected = [...base, footer[0]];
  if (capture.outcome !== 'success') {
    if (capture.stop_phase === 'source') expected.push('Source       Stopped');
    else expected.push('Source       Checked', '1 DASH fMP4  Stopped', '2 Output     Not started');
  }
  if (normal.length !== expected.length || normal.some((line, index) => line !== expected[index])) return false;
  // After the last live frame is cleared, only the final transcript may appear.
  // A stopped row written and erased before the footer is still rejected.
  return snapshots.every(lines => lines.length <= expected.length && lines.every((line, index) =>
    index === lines.length - 1 ? expected[index].startsWith(line) : expected[index] === line));
}

export async function replayCapture(capture) {
  const raw = Buffer.from(capture.raw, 'base64');
  if (raw.length > 262144 || capture.events.length > 8192) throw new Error('capture-limit');
  const terminal = new Terminal({ cols: capture.columns, rows: capture.rows,
    allowProposedApi: true, scrollback: 1000, convertEol: false, logLevel: 'off' });
  let cursorVisible = true, alternateUsed = false, synchronized = false;
  for (const final of ['h', 'l']) terminal.parser.registerCsiHandler({ prefix: '?', final }, parameters => {
    if (parameters.includes(25)) cursorVisible = final === 'h';
    if ([47, 1047, 1049].some(mode => parameters.includes(mode))) alternateUsed = true;
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
    const lines = [];
    for (let index = 0; index < buffer.length; index++) {
      const line = buffer.getLine(index);
      const text = line?.translateToString(false) ?? '';
      if (line?.isWrapped && lines.length) lines[lines.length - 1] += text;
      else lines.push(text);
    }
    return lines.map(line => line.trimEnd()).filter(line => line.trim());
  };
  const frame = (writtenHeaders = null) => {
    const normal = allNormalRows();
    const header = normal.findLastIndex(line => /^drmr convert\b/.test(line));
    const text = header < 0 ? [] : normal.slice(header);
    const bars = text.flatMap(line => [...line.matchAll(/\[[^\]\r\n]*\]/g)]
      .map(match => ({ column: match.index, ascii: /^[\x20-\x7e]+$/.test(match[0]) })));
    return { columns: terminal.cols, rows: terminal.rows,
      header_count: writtenHeaders ?? text.filter(line => /^drmr convert\b/.test(line)).length,
      header_row: header,
      source_count: text.filter(line => /^Source\b/.test(line)).length,
      overall_count: text.filter(line => /^Overall\b/.test(line)).length,
      dashboard_rows: text.filter(line => dashboard.test(line)).length,
      bar_columns: bars.map(bar => bar.column), ascii_bars: bars.every(bar => bar.ascii),
      buffer: terminal.buffer.active.type, ...paintLayout(text) };
  };
  // Observe complete paint transactions, including a delimiter split across reads.
  const ends = new Set([...raw.toString('latin1').matchAll(/\x1b\[\?2026l/g)]
    .map(match => match.index + match[0].length));
  const starts = new Map([...raw.toString('latin1').matchAll(/\x1b\[\?2026h/g)]
    .map(match => [match.index + match[0].length, match.index]));
  const controls = [...raw.toString('latin1').matchAll(/\x1b\[[0-?]*[ -/]*[@-~]|[\r\n\b\t]/g)];
  const cuts = new Set([...ends, ...controls.flatMap(match => [match.index, match.index + match[0].length])]);
  const frames = [];
  const primarySnapshots = [];
  const resultSnapshots = [];
  let consumed = 0, resizedDuringPaint = false, transaction = null, closedBase = null;
  let lastFrameClosedSafely = false, resizedInTransaction = false;
  let geometry = 0, previousPaint = null, priorFramesReplaced = true;
  try {
    await write(history.join('\r\n') + '\r\n');
    for (const event of capture.events) {
      if (event.offset !== consumed) throw new Error('capture-byte-order');
      if (event.kind === 'resize') {
        if (event.columns !== terminal.cols || event.rows !== terminal.rows) {
          geometry++;
          if (synchronized) {
            resizedDuringPaint = true;
            resizedInTransaction = true;
          }
        }
        terminal.resize(event.columns, event.rows);
      } else if (event.kind === 'write') {
        const end = consumed + event.length;
        if (end > raw.length) throw new Error('capture-byte-length');
        const boundaries = [...cuts].filter(offset => offset > consumed && offset <= end).sort((a, b) => a - b);
        if (!boundaries.includes(end)) boundaries.push(end);
        for (const boundary of boundaries) {
          await write(raw.subarray(consumed, boundary));
          consumed = boundary;
          if (starts.has(boundary)) {
            transaction = starts.get(boundary);
            resizedInTransaction = false;
          }
          if (terminal.buffer.active.type === 'normal') {
            const normal = allNormalRows();
            primarySnapshots.push(normal);
            if (closedBase) resultSnapshots.push(normal);
            if (primarySnapshots.length > 1024) throw new Error('primary-observation-limit');
          }
          if (ends.has(boundary) && transaction !== null) {
            const headers = [...raw.subarray(transaction, boundary).toString('latin1').matchAll(/drmr convert\b/g)].length;
            if (headers) {
              const current = frame(headers);
              const normal = allNormalRows();
              const prefix = normal.slice(0, current.header_row);
              const mayRetain = previousPaint && (geometry !== previousPaint.geometry || previousPaint.uncertain);
              // Ordinary redraws replace the previous owned frame. Its rows may
              // join history only at a recorded geometry-change boundary.
              priorFramesReplaced &&= sameRows(prefix, previousPaint?.prefix ?? history) ||
                Boolean(mayRetain && sameRows(prefix, previousPaint.normal));
              previousPaint = { prefix, normal, geometry, uncertain: resizedInTransaction };
              frames.push({ offset: boundary, elapsed_seconds: event.elapsed_seconds, ...current });
            } else if (frames.length && !closedBase) {
              closedBase = allNormalRows();
              resultSnapshots.push(closedBase);
              // A resize inside the last paint makes its row footprint uncertain.
              // Keeping that complete paint is safer than erasing prior history.
              const mayRetain = geometry !== previousPaint.geometry || previousPaint.uncertain;
              lastFrameClosedSafely = sameRows(closedBase, previousPaint.prefix) ||
                (mayRetain && sameRows(closedBase, previousPaint.normal));
            }
            transaction = null;
          }
        }
      } else throw new Error('capture-event-kind');
    }
    if (consumed !== raw.length) throw new Error('capture-byte-total');
    const final = frame();
    const normal = allNormalRows();
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
    const transcriptValid = priorFramesReplaced && primaryGrammar(normal, resultSnapshots, closedBase, capture);
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
      final_dashboard_cleanup_safe: lastFrameClosedSafely && transcriptValid,
      prior_frames_replaced_without_resize: priorFramesReplaced,
      primary_transcript_valid: transcriptValid,
      original_history_preserved: primarySnapshots.every(lines => history.every((line, index) => lines[index] === line)),
      dashboard_below_command: frames.every(value => value.header_row >= history.length),
      no_alternate_screen: !alternateUsed,
      no_screen_clear: !/\x1b\[[0-9;]*J/.test(raw.toString('latin1')),
      no_absolute_cursor_position: !/\x1b\[[0-9;]*[Hf]/.test(raw.toString('latin1')),
      final_summary_present: rows().some(line => summary.test(line)),
      final_normal_buffer: final.buffer === 'normal' && !alternateUsed,
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
