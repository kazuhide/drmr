import assert from 'node:assert/strict';
import { matrixRows } from './matrix.mjs';

// Derive links from the independently reviewed support facts, not the builder.
const formats = ['mp4', 'dash', 'hls', 'hls-ts'];
const targets = { Clear: 'clear', cenc: 'cenc', cbcs: 'cbcs', 'AES-128': 'hls-aes128',
  'SAMPLE-AES': 'hls-sample-aes' };
export const matrixRecipes = matrixRows.flatMap(row => row.output.flatMap((labels, column) =>
  labels.map(label => ({ row: row.id, column, label, format: formats[column], target: targets[label],
    id: `recipe-${row.id}-${formats[column]}-${targets[label]}` }))));

// Non-matrix targets are the independently inventoried navigation and code links.
export const landingFocusTargetCounts = { en: 37 + matrixRecipes.length, ja: 36 + matrixRecipes.length };
export const matrixRecipeBatches = Array.from({ length: Math.ceil(matrixRecipes.length / 15) }, (_, index) => {
  const offset = index * 15, end = Math.min(offset + 15, matrixRecipes.length);
  return { offset, end, recipes: matrixRecipes.slice(offset, end) };
});

const inputKeyModes = {
  mp4: ['cenc-cbcs'], dash: ['cenc-cbcs'],
  'hls-fmp4': ['cenc-cbcs', 'hls-aes128'],
  'hls-ts-video': ['hls-aes128', 'hls-sample-aes'],
  'hls-ts-audio': ['hls-aes128', 'hls-sample-aes'],
};
const keyModeLabels = { 'cenc-cbcs': 'cenc / cbcs', 'hls-aes128': 'AES-128', 'hls-sample-aes': 'SAMPLE-AES' };

// Each recipe retains one shell figure; complete JSON files have stable links.
export const matrixRecipeExampleCount = matrixRecipes.length;

function requireCommands(value, expected) {
  assert.equal(value.tag, 'FIGURE');
  assert.equal(value.figureClass, true);
  assert.equal(value.language, 'sh');
  assert.equal(value.preCount, 1);
  assert.equal(value.codeCount, 1);
  assert.equal(value.tabindex, '0');
  assert.ok(value.code.trim(), 'Every recipe example must be complete and nonempty');
  assert.equal(value.code.trim().replace(/\s+/g, ' '), value.rendered.trim().replace(/\s+/g, ' '),
    'The complete recipe code must be rendered');
  const extension = expected.row === 'mp4' ? 'mp4' : expected.row === 'dash' ? 'mpd' : 'm3u8';
  const command = keys => `drmr convert "https://example.com/input.${extension}" \\\n  --download-dir src --outputs outputs.json --output result${keys ? ' --keys-file keys.json' : ''}\n`;
  assert.equal(value.code.replace(/\r\n/g, '\n'), expected.target === 'clear'
    ? '# Clear input\n' + command(false) + '\n# Encrypted input\n' + command(true) : command(true),
    'Identical keyed conversions share one command; clear output retains both input key roles');

}

export function requireRecipe(value, expected) {
  assert.equal(value.id, expected.id, 'The matrix link must reach its exact recipe');
  assert.equal(value.tag, 'H3');
  assert.equal(value.tabindex, '-1', 'Recipe fragments must accept native focus');
  assert.equal(value.chapter, 'url-input');
  assert.ok(value.heading && value.presented, 'The recipe heading must be rendered');
  assert.equal(value.setupTag, 'P');
  assert.deepEqual(value.setupHrefs, ['#recipe-setup'], 'Each recipe must directly link to its setup');
  assert.deepEqual(value.blocks.map(block => block.tag), ['P', 'P', 'P', 'FIGURE', 'P'],
    'Each recipe must retain complete settings links, both input choices, commands and the result entrypoint');
  for (const paragraph of value.blocks.filter(block => block.tag === 'P')) assert.ok(paragraph.text.trim());
  const root = 'https://github.com/kazuhide/drmr/blob/main/docs/examples/';
  assert.equal(value.blocks[0].text, 'outputs.json');
  assert.deepEqual(value.blocks[0].links, [{ text: 'outputs.json',
    href: root + `outputs/${expected.format}-${expected.target}.json` }]);
  const clear = value.blocks[1];
  const encrypted = expected.target !== 'clear';
  assert.ok((encrypted ? ['Clear input: keys.json.', 'Clear 入力: keys.json。']
    : ['Clear input: no keys.json.', 'Clear 入力: keys.json は不要です。']).includes(clear.text),
  'The clear-input explanation must agree with its actual key-file role');
  assert.match(clear.text, /keys\.json/);
  assert.deepEqual(clear.links, encrypted ? [{ text: 'keys.json',
    href: root + `keys/${expected.format}-${expected.target}-from-clear.json` }] : [],
  'Only encrypted targets need an output key file for clear input');
  const modes = inputKeyModes[expected.row];
  assert.ok(modes, 'Recipe input must have independently admitted encrypted profiles');
  const encryptedChoice = value.blocks[2];
  assert.match(encryptedChoice.text, /keys\.json/);
  assert.deepEqual(encryptedChoice.links, modes.map(mode => ({ text: keyModeLabels[mode],
    href: root + `keys/${expected.format}-${expected.target}-from-${mode}.json` })),
  'Every admitted encrypted profile needs a complete matching key-file link');
  requireCommands(value.blocks[3], expected);
  const entrypoint = `result/${expected.format}/` + (expected.format === 'mp4' ? 'media.mp4'
    : expected.format === 'dash' ? 'manifest.mpd' : 'master.m3u8');
  assert.ok(value.blocks[4].text.includes(entrypoint), 'The result must name its actual named-output entrypoint');
  assert.deepEqual(value.blocks[4].links, []);
}

// This readback does not scroll or focus the fragment on behalf of the browser.
export function recipeObservation(element) {
  const setup = element.nextElementSibling;
  const blocks = [];
  for (let node = setup?.nextElementSibling; node && !['H2', 'H3'].includes(node.tagName); node = node.nextElementSibling) {
    if (node.tagName === 'FIGURE') {
      const code = node.querySelector('pre code');
      blocks.push({ tag: node.tagName, figureClass: node.classList.contains('guide-example'),
        language: node.getAttribute('data-code-language'), preCount: node.querySelectorAll('pre').length,
        codeCount: node.querySelectorAll('pre code').length, tabindex: node.querySelector('pre')?.getAttribute('tabindex'),
        code: code?.textContent ?? '', rendered: code?.innerText ?? '' });
    } else {
      blocks.push({ tag: node.tagName, text: node.textContent.trim(),
        links: [...node.querySelectorAll('a')].map(link => ({ text: link.textContent.trim(), href: link.getAttribute('href') })) });
    }
  }
  const box = element.getBoundingClientRect();
  const style = getComputedStyle(element);
  return { id: element.id, tag: element.tagName, tabindex: element.getAttribute('tabindex'),
    url: location.href, pageLanguage: document.documentElement.lang,
    readyState: document.readyState, fontsStatus: document.fonts.status,
    chapter: element.closest('[data-guide-chapter]')?.id, heading: element.textContent.trim(),
    presented: box.width > 0 && box.height > 0 && style.display !== 'none' && style.visibility === 'visible',
    setupTag: setup?.tagName, setupHrefs: [...(setup?.querySelectorAll('a') ?? [])].map(link => link.getAttribute('href')),
    blocks, rect: { left: box.left, right: box.right, top: box.top, bottom: box.bottom },
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
