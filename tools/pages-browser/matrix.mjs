import assert from 'node:assert/strict';

// Reviewed public profile facts, independent of the rendered HTML.
const standard = [['Clear', 'cenc', 'cbcs'], ['Clear', 'cenc', 'cbcs'], ['Clear', 'cbcs', 'AES-128'], []];
export const matrixColumns = ['MP4', 'DASH', 'HLS fMP4', 'HLS MPEG-TS'];
export const matrixRows = [
  { id: 'mp4-standard', format: 'MP4', input: ['Clear', 'cenc', 'cbcs'], output: standard },
  { id: 'mp4-audio-edit', format: 'MP4', input: ['Clear', 'cenc'], output: [['Clear'], [], [], []] },
  { id: 'dash', format: 'DASH', input: ['Clear', 'cenc', 'cbcs'], output: standard },
  { id: 'hls-fmp4', format: 'HLS', input: ['Clear', 'cbcs', 'AES-128'], output: standard },
  { id: 'hls-ts-video', format: 'HLS / MPEG-TS', input: ['Clear', 'AES-128', 'SAMPLE-AES'],
    output: [[], [], ['Clear', 'cbcs', 'AES-128'], ['Clear', 'AES-128', 'SAMPLE-AES']] },
  { id: 'hls-ts-audio', format: 'HLS / MPEG-TS', input: ['Clear', 'AES-128', 'SAMPLE-AES'],
    output: [[], [], [], ['Clear', 'AES-128', 'SAMPLE-AES']] },
];

export function requireMatrixLayout(value) {
  assert.equal(value.overflowX, 'auto', 'Matrix needs its own native horizontal scroll container');
  assert.ok(value.clientWidth > 0 && value.clientHeight > 0);
  assert.ok(value.rect.left >= 0 && value.rect.right <= value.documentWidth, 'Matrix viewport must stay inside the page');
  assert.equal(value.documentScrollWidth, value.documentWidth, 'The page must not replace matrix scrolling');
  assert.deepEqual(value.clippedCells, [], 'Matrix cells must retain their complete content');
  assert.equal(value.rowHeaders.length, 6);
  for (const header of value.rowHeaders) {
    assert.equal(header.position, 'sticky');
    assert.ok(header.left >= value.contentLeft && header.right <= value.contentRight,
      'Sticky input headers must stay inside the matrix viewport');
  }
}

export function requireMatrixMovement(before, after, direction) {
  requireMatrixLayout(before);
  requireMatrixLayout(after);
  assert.equal(after.focused, true, 'Native arrows must retain the matrix focus');
  assert.equal(after.pageX, before.pageX, 'Arrow keys must not move the document horizontally');
  assert.equal(after.pageY, before.pageY, 'Arrow keys must not move the document vertically');
  assert.ok(direction === 'right' ? after.scrollLeft > before.scrollLeft : after.scrollLeft < before.scrollLeft,
    'Native arrows must move the matrix in the requested direction');
  assert.deepEqual(after.rowHeaders.map(header => header.left), before.rowHeaders.map(header => header.left),
    'Input headers must remain anchored while output columns move');
}

// Executed read-only in Chromium. No CSS, focus or scroll position is changed.
export function matrixObservation(wrapper) {
  const rect = wrapper.getBoundingClientRect();
  const contentLeft = rect.left + wrapper.clientLeft;
  const contentRight = contentLeft + wrapper.clientWidth;
  const bounds = element => {
    const box = element.getBoundingClientRect();
    return { left: box.left, right: box.right, top: box.top, bottom: box.bottom, width: box.width, height: box.height };
  };
  const rowHeaders = [...wrapper.querySelectorAll('tbody th[scope="row"]')].map(element => ({
    ...bounds(element), position: getComputedStyle(element).position,
  }));
  const clippedCells = [...wrapper.querySelectorAll('th,td')].filter(element => {
    const style = getComputedStyle(element);
    return style.display === 'none' || style.visibility !== 'visible' ||
      element.scrollWidth > element.clientWidth || element.scrollHeight > element.clientHeight;
  }).map(element => element.textContent.trim());
  const lastCells = [...wrapper.querySelectorAll('tbody td:last-child')].map(bounds);
  const firstCells = [...wrapper.querySelectorAll('tbody td:first-of-type')].map(bounds);
  return { rect: bounds(wrapper), contentLeft, contentRight,
    clientWidth: wrapper.clientWidth, clientHeight: wrapper.clientHeight,
    scrollWidth: wrapper.scrollWidth, scrollLeft: wrapper.scrollLeft,
    overflowX: getComputedStyle(wrapper).overflowX,
    focused: document.activeElement === wrapper, rowHeaders, clippedCells, firstCells, lastCells,
    documentWidth: document.documentElement.clientWidth, documentScrollWidth: document.documentElement.scrollWidth,
    pageX: scrollX, pageY: scrollY };
}
