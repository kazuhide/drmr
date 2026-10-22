import assert from 'node:assert/strict';

// Fixed guide routes and chapter identity; the committed builder owns the copy.
export const guideChapters = ['introduction', 'terminology', 'supported-profiles', 'installation',
  'key-configuration', 'inspect-plan', 'local-conversion', 'url-input', 'verification',
  'resources-stop', 'docker', 'windows', 'web-console',
  'desktop', 'results-errors', 'regression-tests', 'cli-reference', 'references'];
export const guideLocales = [
  { language: 'en', route: 'guide/', asset: 'guide/index.html', title: 'User guide',
    skip: 'Skip to content', toc: 'Guide chapters', switchName: '日本語', other: 'ja',
    switchRoute: 'ja/guide/' },
  { language: 'ja', route: 'ja/guide/', asset: 'ja/guide/index.html', title: 'ユーザーガイド',
    skip: '本文へ移動', toc: '目次', switchName: 'English', other: 'en',
    switchRoute: 'guide/' },
];

export function guideObservation() {
  const shown = element => {
    if (!element || !element.getClientRects().length) return false;
    for (let node = element; node; node = node.parentElement) {
      const style = getComputedStyle(node);
      if (style.display === 'none' || style.visibility !== 'visible' || Number(style.opacity) === 0) return false;
    }
    const rect = element.getBoundingClientRect();
    return rect.width > 0 && rect.height > 0;
  };
  const width = document.documentElement.clientWidth;
  return { clientWidth: width, scrollWidth: document.documentElement.scrollWidth,
    clipped: [...document.querySelectorAll('main,article,.guide-chapter,pre,.guide-table-scroll')]
      .filter(element => { const r = element.getBoundingClientRect(); return r.left < 0 || r.right > width; })
      .map(element => ({ tag: element.tagName, id: element.id })),
    chapters: [...document.querySelectorAll('[data-guide-chapter]')].map(element => ({
      id: element.id, key: element.getAttribute('data-guide-chapter'),
      title: element.querySelector('h2')?.textContent.trim(),
      presented: shown(element) && shown(element.querySelector('h2')),
    })) };
}

export function requireGuideLayout(value) {
  if (!(value.clientWidth > 0 && value.scrollWidth === value.clientWidth && value.clipped.length === 0 &&
    value.chapters.length === guideChapters.length && value.chapters.every((chapter, index) =>
      chapter.id === guideChapters[index] && chapter.key === chapter.id && chapter.title && chapter.presented === true))) {
    throw new Error('The guide must expose every chapter without document overflow or hidden headings');
  }
}

export function guideScrollObservation(element) {
  const r = element.getBoundingClientRect();
  const code = element.tagName === 'PRE' ? element.querySelector('code').getBoundingClientRect() : null;
  const style = getComputedStyle(element);
  return { focused: document.activeElement === element, tag: element.tagName,
    chapter: element.closest('[data-guide-chapter]').id,
    identity: element.getAttribute('aria-labelledby') || element.parentElement.getAttribute('aria-labelledby'),
    scrollLeft: element.scrollLeft, scrollWidth: element.scrollWidth, clientWidth: element.clientWidth,
    scrollTop: element.scrollTop, scrollHeight: element.scrollHeight, clientHeight: element.clientHeight,
    pageX: scrollX, pageY: scrollY, documentWidth: document.documentElement.clientWidth,
    documentHeight: document.documentElement.clientHeight,
    rect: { left: r.left, right: r.right, width: r.width, top: r.top, bottom: r.bottom, height: r.height },
    client: { top: r.top + element.clientTop, bottom: r.top + element.clientTop + element.clientHeight },
    code: code ? { top: code.top, bottom: code.bottom, height: code.height } : null,
    overflowX: style.overflowX, overflowY: style.overflowY };
}

export function requireGuideMovement(before, after, direction) {
  const horizontal = direction === 'right' || direction === 'left';
  const vertical = direction === 'down' || direction === 'up';
  const moved = horizontal ? after.scrollTop === before.scrollTop &&
    (direction === 'right' ? after.scrollLeft > before.scrollLeft : after.scrollLeft < before.scrollLeft) :
    vertical && after.tag === 'PRE' && after.scrollLeft === 0 && before.scrollLeft === 0 &&
    (direction === 'down' ? after.scrollTop > before.scrollTop : after.scrollTop < before.scrollTop);
  if (!(before.focused === true && after.focused === true && before.tag === after.tag && before.chapter === after.chapter &&
    before.identity === after.identity && after.identity && after.pageX === before.pageX && after.pageY === before.pageY &&
    after.clientWidth > 0 && after.scrollWidth === before.scrollWidth && after.clientWidth === before.clientWidth &&
    after.clientHeight > 0 && after.scrollHeight === before.scrollHeight && after.clientHeight === before.clientHeight &&
    after.documentWidth === before.documentWidth && after.documentHeight === before.documentHeight &&
    JSON.stringify(after.rect) === JSON.stringify(before.rect) &&
    after.overflowX === 'auto' && after.overflowY === 'auto' &&
    0 <= after.rect.left && after.rect.left < after.rect.right && after.rect.right <= after.documentWidth &&
    0 <= after.rect.top && after.rect.top < after.rect.bottom && after.rect.bottom <= after.documentHeight && moved)) {
    throw new Error('Native guide scrolling must retain its target and leave document scrolling unchanged');
  }
}

export function requireGuideCodeEdge(value, edge) {
  const { client, code } = value;
  // scrollTop is fractional; CSSOM scrollHeight and clientHeight are integers.
  const reached = edge === 'bottom' ? value.scrollTop > 0 &&
    Math.abs(value.scrollHeight - value.clientHeight - value.scrollTop) < 1 &&
    code?.bottom > client.top && code.bottom <= client.bottom :
    edge === 'top' && value.scrollTop === 0 && code?.top >= client.top && code.top < client.bottom;
  if (!(value.focused === true && value.tag === 'PRE' && value.scrollLeft === 0 &&
    value.scrollHeight > value.clientHeight && client.top < client.bottom && code?.height > 0 && reached)) {
    throw new Error('The native code viewport must expose its complete requested edge');
  }
}

export function requireGuidePrintCode(values, source) {
  assert.ok(source.length > 0);
  assert.equal(values.length, source.length, 'Print must retain every code example');
  const compact = text => text.trim().replace(/\s+/g, ' ');
  values.forEach((value, index) => {
    assert.ok(value.shown && source[index].trim(), 'Printed code must remain visible and nonempty');
    assert.equal(value.code, source[index], 'Print must retain the complete source code');
    assert.equal(compact(value.rendered), compact(source[index]), 'Print must render the complete code');
    assert.equal(value.maxBlockSize, 'none', 'Print must remove the code viewport height limit');
    assert.equal(value.overflowX, 'visible');
    assert.equal(value.overflowY, 'visible');
    assert.equal(value.whiteSpace, 'pre-wrap');
    assert.ok(value.clientWidth > 0 && value.scrollWidth <= value.clientWidth &&
      value.clientHeight > 0 && value.scrollHeight <= value.clientHeight, 'Printed code must not be clipped');
  });
}

// A JSON-quoted scalar is also valid YAML; preserve full code through ARIA snapshot escaping.
export function codeAriaTemplate(text) {
  const code = text.trim().replace(/\s+/g, ' ');
  if (!code) throw new Error('Guide code must not be empty');
  return '- code: ' + JSON.stringify(code);
}

// Read all figures once; the static page has no code that can replace them.
export function guideFiguresObservation() {
  return [...document.querySelectorAll('figure.guide-example')].map(node => {
    const captions = node.querySelectorAll('figcaption');
    const pres = node.querySelectorAll('pre');
    const codes = node.querySelectorAll('pre code');
    return { tag: node.tagName, role: node.getAttribute('role'),
      language: node.getAttribute('data-code-language'), labelledBy: node.getAttribute('aria-labelledby'),
      captionCount: captions.length, captionId: captions[0]?.id, captionText: captions[0]?.innerText,
      preCount: pres.length, codeCount: codes.length, tabindex: pres[0]?.getAttribute('tabindex'),
      code: codes[0]?.textContent, rendered: codes[0]?.innerText };
  });
}

// Playwright emits plain, JSON-quoted or YAML single-quoted scalars here.
// Reject an unfamiliar structure rather than silently dropping figure content.
function snapshotScalar(value) {
  if (value.startsWith('"')) return JSON.parse(value);
  if (value.startsWith("'")) {
    assert.match(value, /^'(?:[^']|'')*'$/);
    return value.slice(1, -1).replace(/''/g, "'");
  }
  assert.ok(value && !/[\r\n]/.test(value));
  assert.ok(!/^[\[\]{},&*!|>%@`]/.test(value));
  return value;
}

function figureSubtrees(snapshot) {
  const lines = snapshot.split('\n');
  const figures = [];
  for (let index = 0; index < lines.length; index++) {
    const start = /^( *)- ((?:figure\b|'figure\b|"figure\b).*)$/.exec(lines[index]);
    if (!start) continue;
    assert.ok(start[2].endsWith(':'), 'Every actual ARIA figure needs its complete subtree');
    const key = snapshotScalar(start[2].slice(0, -1));
    assert.match(key, /^figure ".*"$/, 'Each actual ARIA figure needs its accessible name');
    const label = JSON.parse(key.slice('figure '.length));
    const indent = start[1].length;
    let end = index + 1;
    while (end < lines.length && lines[end].trim() && /^ */.exec(lines[end])[0].length > indent) end++;
    const subtree = lines.slice(index, end).map(line => line.slice(indent));
    assert.equal(subtree.length, 3, 'Actual figure ARIA must retain its caption and complete code');
    const caption = /^  - text: (.+)$/.exec(subtree[1]);
    const code = /^  - code: (.+)$/.exec(subtree[2]);
    assert.ok(caption && code, 'Actual figure ARIA needs caption text and the code role');
    figures.push({ label, caption: snapshotScalar(caption[1]), code: snapshotScalar(code[1]),
      aria: subtree.join('\n') });
    index = end - 1;
  }
  return figures;
}

// Every DOM figure is paired with its own actual subtree from the complete
// accessibility snapshot, retaining full raw evidence and source order.
export function requireGuideFigures(facts, snapshot) {
  const figures = figureSubtrees(snapshot);
  assert.ok(facts.length > 0, 'Guide figures must not be empty');
  assert.equal(figures.length, facts.length, 'Every DOM figure needs exactly one actual ARIA figure');
  const labels = new Set();
  const captions = new Set();
  const compact = text => text.trim().replace(/\s+/g, ' ');
  return facts.map((fact, index) => {
    const actual = figures[index];
    assert.equal(fact.tag, 'FIGURE');
    assert.equal(fact.role, null);
    assert.match(fact.language, /^(sh|bash|powershell|json|text)$/);
    assert.equal(fact.captionCount, 1);
    assert.equal(fact.preCount, 1);
    assert.equal(fact.codeCount, 1);
    assert.equal(fact.tabindex, '0');
    assert.ok(fact.captionId && !captions.has(fact.captionId));
    captions.add(fact.captionId);
    assert.equal(fact.labelledBy, fact.captionId);
    const label = compact(fact.captionText);
    assert.ok(label && !labels.has(label), 'Guide figure accessible names must be unique');
    labels.add(label);
    assert.ok(fact.code.trim(), 'Guide code must not be empty');
    assert.equal(compact(fact.rendered), compact(fact.code), 'Complete guide code must remain rendered');
    assert.equal(actual.label, label, 'Actual figure role and accessible name must match its caption');
    assert.equal(actual.caption, label);
    assert.equal(actual.code, compact(fact.code), 'Actual code ARIA must retain the complete source text');
    return { label, language: fact.language, role: 'figure', code: fact.code,
      rendered: fact.rendered, aria: actual.aria };
  });
}

// Screenshots observe the same native owner after a completed traversal; they
// cannot substitute another target, scroll the page, or crop the focus outline.
export function requireGuideCheckpoint(record, expected) {
  for (const key of ['source_head', 'source_tree', 'language', 'page_asset', 'page_sha256']) {
    if (record[key] !== expected[key]) throw new Error('Guide checkpoint source identity changed');
  }
  if (JSON.stringify(record.viewport) !== JSON.stringify(expected.viewport)) throw new Error('Guide checkpoint viewport changed');
  const tag = { 'native-code-viewport': 'PRE', 'native-table-viewport': 'DIV', 'native-code-vertical-viewport': 'PRE' }[record.phase];
  const owner = record.selected;
  if (!tag || owner.tag !== tag || !guideChapters.includes(owner.chapter) || !owner.identity) {
    throw new Error('Guide checkpoint phase or owner changed');
  }
  if (record.phase === 'native-code-vertical-viewport' && !(owner.chapter === 'windows' &&
    /^[a-f0-9]{64}$/.test(owner.function_sha256) && /^[a-f0-9]{64}$/.test(owner.code_sha256))) {
    throw new Error('The vertical guide checkpoint needs its source-bound Windows code example');
  }
  for (const value of [record.before, record.after]) {
    const f = value.focus, r = f.rect, v = f.viewport, s = value.scroll;
    if (!(s.focused === true && s.tag === tag && s.chapter === owner.chapter && s.identity === owner.identity &&
      s.scrollLeft === 0 && s.scrollTop === 0 && v.width === expected.viewport.width && v.height === expected.viewport.height && f.matchesTarget === true && JSON.stringify(f.target) === JSON.stringify(f.active) &&
      f.target.tag === tag && f.inViewport === true && 0 <= r.left && r.left < r.right && r.right <= v.width &&
      0 <= r.top && r.top < r.bottom && r.bottom <= v.height && r.width > 0 && r.height > 0 &&
      f.outlineWidth >= 2 && f.outlineStyle !== 'none')) {
      throw new Error('Guide checkpoint must show its fully focused native owner');
    }
  }
  if (JSON.stringify(record.before.scroll) !== JSON.stringify(record.after.scroll) ||
      JSON.stringify(record.before.focus.rect) !== JSON.stringify(record.after.focus.rect) ||
      JSON.stringify(record.before.focus.scroll) !== JSON.stringify(record.after.focus.scroll) ||
      JSON.stringify(record.before.focus.target) !== JSON.stringify(record.after.focus.target)) {
    throw new Error('Guide checkpoint moved its owner or document');
  }
}
