import assert from 'node:assert/strict';
import { matrixRows } from './matrix.mjs';

// Derive links from the independently reviewed support facts, not the builder.
const formats = ['mp4', 'dash', 'hls', 'hls-ts'];
const targets = { Clear: 'clear', cenc: 'cenc', cbcs: 'cbcs', 'AES-128': 'hls-aes128',
  'SAMPLE-AES': 'hls-sample-aes' };
export const matrixRecipes = matrixRows.flatMap(row => row.output.flatMap((labels, column) =>
  labels.map(label => ({ row: row.id, column, label, format: formats[column], target: targets[label],
    id: `recipe-${row.id}-${formats[column]}-${targets[label]}` }))));

const inputProfiles = {
  mp4: ['Clear', 'cenc', 'cbcs'], dash: ['Clear', 'cenc', 'cbcs'],
  'hls-fmp4': ['Clear', 'cenc', 'cbcs', 'AES-128'],
  'hls-ts-video': ['Clear', 'AES-128', 'SAMPLE-AES'],
  'hls-ts-audio': ['Clear', 'AES-128', 'SAMPLE-AES'],
};

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
  const extension = expected.row === 'mp4' ? 'mp4' : expected.row === 'dash' ? 'mpd' : 'm3u8';
  const profiles = inputProfiles[expected.row];
  assert.ok(profiles, 'Recipe input must have an independently supported protection profile');
  // Preserve trailing spaces: a continuation backslash must immediately precede the newline.
  const lines = value.code.split(/\r?\n/).filter(line => line.trim());
  assert.equal(lines.length, profiles.length * 7,
    'Every supported input protection needs one comment and four commands with two wrapped lines');
  for (const [index, profile] of profiles.entries()) {
    assert.equal(lines[index * 7], `# ${profile} input`, 'Input protection groups must be complete and ordered');
    const physical = lines.slice(index * 7 + 1, index * 7 + 7);
    for (const start of [0, 2]) assert.match(physical[start], / \\$/,
      'Analyze and plan must use literal shell continuations for their second line');
    assert.match(physical[1], /^  --download-dir /);
    assert.match(physical[3], /^  --output /);
    const steps = [physical[0].slice(0, -1) + physical[1].trimStart(),
      physical[2].slice(0, -1) + physical[3].trimStart(), physical[4], physical[5]];
    for (const step of steps.slice(0, 3)) assert.match(step, / &&$/, 'Each operation must stop the group on failure');
    assert.doesNotMatch(steps[3], / &&$/, 'Verification must finish its input group');
    const commands = steps.map(step => step.replace(/ &&$/, '').split(/\s+/));
    assert.deepEqual(commands.map(command => command.slice(0, 2)),
      ['analyze', 'plan', 'convert', 'verify'].map(operation => ['drmr', operation]),
      'Every input protection must include the four complete CLI steps');
    const name = { Clear: 'clear', 'AES-128': 'aes128', 'SAMPLE-AES': 'sample-aes' }[profile] ?? profile;
    const inputKeys = profile === 'Clear' ? [] : ['--keys-file', 'keys.json'];
    const runtimeKeys = profile === 'Clear' && expected.target === 'clear' ? [] : ['--keys-file', 'keys.json'];
    assert.deepEqual(commands[0], ['drmr', 'analyze', `"https://example.com/${name}.${extension}"`,
      '--download-dir', 'src', ...inputKeys],
    'Every input must use a quoted URL for native acquisition with only its source keys');
    assert.deepEqual(commands[1], ['drmr', 'plan', `src/input.${extension}`, '--to', expected.target,
      '--format', expected.format, '--output', expected.format === 'mp4' ? 'result.mp4' : 'result',
      '--save', 'plan.json', ...inputKeys],
    'Planning must use the actual retained entrypoint, selected output and only source keys');
    for (const [offset, operation] of ['convert', 'verify'].entries()) {
      assert.deepEqual(commands[offset + 2], ['drmr', operation, '--plan', 'plan.json', ...runtimeKeys],
        'Conversion and verification must use the saved plan and the required runtime keys');
    }
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
