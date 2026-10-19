// Fixed guide routes and chapter identity; the committed builder owns the copy.
export const guideChapters = ['introduction', 'terminology', 'supported-profiles', 'installation',
  'key-configuration', 'inspect-plan', 'local-conversion', 'url-input', 'verification',
  'saved-hls', 'prepared-dash-batch', 'resources-stop', 'docker', 'windows', 'web-console',
  'desktop', 'results-errors', 'regression-tests', 'cli-reference', 'references'];
export const guideLocales = [
  { language: 'en', route: 'guide/', asset: 'guide/index.html', title: 'User guide',
    skip: 'Skip to content', toc: 'Guide chapters', switchName: '日本語', other: 'ja',
    switchRoute: 'ja/guide/' },
  { language: 'ja', route: 'ja/guide/', asset: 'ja/guide/index.html', title: 'ユーザーガイド',
    skip: '本文へ移動', toc: 'ガイドの章', switchName: 'English', other: 'en',
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
  return { focused: document.activeElement === element, tag: element.tagName,
    chapter: element.closest('[data-guide-chapter]').id,
    identity: element.getAttribute('aria-labelledby') || element.parentElement.getAttribute('aria-labelledby'),
    scrollLeft: element.scrollLeft, scrollWidth: element.scrollWidth, clientWidth: element.clientWidth,
    pageX: scrollX, pageY: scrollY, documentWidth: document.documentElement.clientWidth,
    rect: { left: r.left, right: r.right, width: r.width }, overflowX: getComputedStyle(element).overflowX };
}

export function requireGuideMovement(before, after, direction) {
  if (!(after.focused === true && before.tag === after.tag && before.chapter === after.chapter &&
    before.identity === after.identity && after.identity && after.pageX === before.pageX && after.pageY === before.pageY &&
    after.clientWidth > 0 && after.scrollWidth === before.scrollWidth && after.clientWidth === before.clientWidth &&
    after.overflowX === 'auto' && 0 <= after.rect.left && after.rect.left < after.rect.right &&
    after.rect.right <= after.documentWidth &&
    (direction === 'right' ? after.scrollLeft > before.scrollLeft : after.scrollLeft < before.scrollLeft))) {
    throw new Error('Native guide scrolling must retain its target and leave document scrolling unchanged');
  }
}

// A JSON-quoted scalar is also valid YAML; preserve full code through ARIA snapshot escaping.
export function codeAriaTemplate(text) {
  const code = text.trim().replace(/\s+/g, ' ');
  if (!code) throw new Error('Guide code must not be empty');
  return '- code: ' + JSON.stringify(code);
}

// Screenshots observe the same native owner after a completed traversal; they
// cannot substitute another target, scroll the page, or crop the focus outline.
export function requireGuideCheckpoint(record, expected) {
  for (const key of ['source_head', 'source_tree', 'language', 'page_asset', 'page_sha256']) {
    if (record[key] !== expected[key]) throw new Error('Guide checkpoint source identity changed');
  }
  if (JSON.stringify(record.viewport) !== JSON.stringify(expected.viewport)) throw new Error('Guide checkpoint viewport changed');
  const tag = { 'native-code-viewport': 'PRE', 'native-table-viewport': 'DIV' }[record.phase];
  const owner = record.selected;
  if (!tag || owner.tag !== tag || !guideChapters.includes(owner.chapter) || !owner.identity) {
    throw new Error('Guide checkpoint phase or owner changed');
  }
  for (const value of [record.before, record.after]) {
    const f = value.focus, r = f.rect, v = f.viewport, s = value.scroll;
    if (!(s.focused === true && s.tag === tag && s.chapter === owner.chapter && s.identity === owner.identity &&
      s.scrollLeft === 0 && v.width === expected.viewport.width && v.height === expected.viewport.height && f.matchesTarget === true && JSON.stringify(f.target) === JSON.stringify(f.active) &&
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
