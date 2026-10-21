import assert from 'node:assert/strict';
import { matrixRows } from './matrix.mjs';

// Derive links from the independently reviewed support facts, not the builder.
const formats = ['mp4', 'dash', 'hls', 'hls-ts'];
const targets = { Clear: 'clear', cenc: 'cenc', cbcs: 'cbcs', 'AES-128': 'hls-aes128',
  'SAMPLE-AES': 'hls-sample-aes' };
export const matrixRecipes = matrixRows.flatMap(row => row.output.flatMap((labels, column) =>
  labels.map(label => ({ row: row.id, column, label, format: formats[column], target: targets[label],
    id: `recipe-${row.id}-${formats[column]}-${targets[label]}` }))));

export function requireRecipe(value, expected) {
  assert.equal(value.id, expected.id, 'The matrix link must reach its exact recipe');
  assert.equal(value.tag, 'H3');
  assert.equal(value.tabindex, '-1', 'Recipe fragments must accept native focus');
  assert.equal(value.chapter, 'url-input');
  assert.ok(value.heading && value.presented, 'The recipe heading must be rendered');
  assert.equal(value.setupTag, 'P');
  assert.deepEqual(value.setupHrefs, ['#recipe-setup'], 'Each recipe must directly link to its setup');
  assert.equal(value.figureTag, 'FIGURE');
  assert.equal(value.figureClass, true);
  assert.equal(value.language, 'sh');
  assert.equal(value.code.trim().replace(/\s+/g, ' '), value.rendered.trim().replace(/\s+/g, ' '),
    'The complete recipe code must be rendered');
  const code = value.code.replace(/\\\r?\n\s*/g, ' ');
  const commands = [...code.matchAll(/^\s*drmr (analyze|plan|convert|verify)\b(.*)$/gm)];
  assert.deepEqual(commands.map(command => command[1]), ['analyze', 'plan', 'convert', 'verify'],
    'Every recipe must include the four complete CLI steps');
  assert.match(code, /^\s*\(\s*\n\s*set -eu\s*$/m, 'Recipes must isolate their working directory and stop on failure');
  const flag = (command, name) => {
    const found = command.match(new RegExp(`(?:^|\\s)--${name}\\s+(?:"([^"\\n]+)"|'([^'\\n]+)'|([^\\s]+))`));
    assert.ok(found, `Recipe command needs --${name}`);
    return found[1] ?? found[2] ?? found[3];
  };
  assert.equal(flag(commands[1][2], 'to'), expected.target);
  assert.equal(flag(commands[1][2], 'format'), expected.format);
  assert.ok(flag(commands[1][2], 'output'));
  const plan = flag(commands[1][2], 'save');
  for (const command of commands) assert.equal(flag(command[2], 'keys-file'), '../config/keys.json');
  for (const command of commands.slice(2)) {
    assert.equal(flag(command[2], 'plan'), plan, 'Conversion and verification must use the saved plan');
  }
  if (expected.row.startsWith('hls-')) {
    assert.match(commands[0][2], /https:\/\//, 'HLS recipes must acquire the public URL');
    assert.ok(flag(commands[0][2], 'download-dir'));
  } else {
    assert.match(code, /\bcurl\s/);
    assert.match(code, /https:\/\//, 'MP4 and DASH recipes must download their public input');
  }
}

// This readback does not scroll or focus the fragment on behalf of the browser.
export function recipeObservation(element) {
  const setup = element.nextElementSibling;
  const figure = setup?.nextElementSibling;
  const code = figure?.querySelector('pre code');
  const box = element.getBoundingClientRect();
  const style = getComputedStyle(element);
  return { id: element.id, tag: element.tagName, tabindex: element.getAttribute('tabindex'),
    url: location.href, pageLanguage: document.documentElement.lang,
    readyState: document.readyState, fontsStatus: document.fonts.status,
    chapter: element.closest('[data-guide-chapter]')?.id, heading: element.textContent.trim(),
    presented: box.width > 0 && box.height > 0 && style.display !== 'none' && style.visibility === 'visible',
    setupTag: setup?.tagName, setupHrefs: [...(setup?.querySelectorAll('a') ?? [])].map(link => link.getAttribute('href')),
    figureTag: figure?.tagName, figureClass: figure?.classList.contains('guide-example'),
    language: figure?.getAttribute('data-code-language'), code: code?.textContent ?? '',
    rendered: code?.innerText ?? '',
    rect: { left: box.left, right: box.right, top: box.top, bottom: box.bottom },
    viewport: { width: innerWidth, height: innerHeight }, focused: document.activeElement === element };
}

// Identity failures are terminal measurement errors, not a settling condition.
export function requireRecipeIdentity(value, recipe, source) {
  assert.equal(value.id, recipe.id);
  assert.equal(value.tag, 'H3');
  assert.equal(value.tabindex, '-1');
  assert.equal(value.chapter, 'url-input');
  assert.equal(value.url, source.url);
  assert.equal(value.pageLanguage, source.language);
}

// Pointer navigation need not draw a keyboard outline. Its actual focused
// heading must still fit completely inside the viewport, without any epsilon.
export function requireRecipePosition(value) {
  assert.equal(value.focused, true);
  const { rect, viewport } = value;
  assert.ok([rect.left, rect.right, rect.top, rect.bottom, viewport.width, viewport.height].every(Number.isFinite));
  assert.ok(viewport.width > 0 && viewport.height > 0 && rect.left < rect.right && rect.top < rect.bottom);
  assert.ok(0 <= rect.left && rect.right <= viewport.width && 0 <= rect.top && rect.bottom <= viewport.height);
}

export function matrixLinkExposure(element) {
  if (!element.matches('.conversion-matrix td a[href]')) return { matrixLink: false };
  const rectangles = [...element.getClientRects()].filter(rect => rect.width > 0 && rect.height > 0);
  const points = rectangles.flatMap(rect => {
    const inset = Math.min(2, rect.width / 4, rect.height / 4);
    return [[rect.left + inset, rect.top + inset], [rect.right - inset, rect.top + inset],
      [rect.left + inset, rect.bottom - inset], [rect.right - inset, rect.bottom - inset],
      [(rect.left + rect.right) / 2, (rect.top + rect.bottom) / 2]].map(([x, y]) => {
      const hit = document.elementFromPoint(x, y);
      return { x, y, exposed: Boolean(hit && (hit === element || element.contains(hit))),
        hit: hit ? { tag: hit.tagName, id: hit.id, class: hit.className } : null };
    });
  });
  const wrapper = element.closest('.matrix-scroll');
  const box = wrapper.getBoundingClientRect();
  return { matrixLink: true, points,
    rectangles: rectangles.map(rect => ({ left: rect.left, right: rect.right, top: rect.top, bottom: rect.bottom })),
    scroller: { left: box.left, right: box.right, top: box.top, bottom: box.bottom,
      scrollLeft: wrapper.scrollLeft, clientWidth: wrapper.clientWidth, scrollWidth: wrapper.scrollWidth } };
}

export function requireMatrixLinkExposure(value) {
  if (value.matrixLink) assert.ok(value.points.length >= 5 && value.points.every(point => point.exposed),
    'Native matrix link focus must remain visible beside sticky headers');
}
