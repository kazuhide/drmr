import assert from 'node:assert/strict';

// Reviewed public profile facts, independent of the rendered HTML.
const standard = [['Clear', 'cenc', 'cbcs'], ['Clear', 'cenc', 'cbcs'], ['Clear', 'cbcs', 'AES-128'], []];
export const matrixColumns = ['MP4', 'DASH', 'HLS fMP4', 'HLS MPEG-TS'];
export const matrixRows = [
  { id: 'mp4', format: 'MP4', input: ['Clear', 'cenc', 'cbcs'], output: standard },
  { id: 'dash', format: 'DASH', input: ['Clear', 'cenc', 'cbcs'], output: standard },
  { id: 'hls-fmp4', format: 'HLS', input: ['Clear', 'cbcs', 'AES-128'], output: standard },
  { id: 'hls-ts-video', format: 'HLS / MPEG-TS', input: ['Clear', 'AES-128', 'SAMPLE-AES'],
    output: [[], ['Clear', 'cenc', 'cbcs'], ['Clear', 'cbcs', 'AES-128'], ['Clear', 'AES-128', 'SAMPLE-AES']] },
  { id: 'hls-ts-audio', format: 'HLS / MPEG-TS', input: ['Clear', 'AES-128', 'SAMPLE-AES'],
    output: [[], ['Clear', 'cenc', 'cbcs'], ['Clear', 'cbcs', 'AES-128'], ['Clear', 'AES-128', 'SAMPLE-AES']] },
];

export function requireMatrixLayout(value) {
  assert.equal(value.overflowX, 'auto', 'Matrix needs its own native horizontal scroll container');
  assert.ok(value.clientWidth > 0 && value.clientHeight > 0);
  assert.ok(value.rect.left >= 0 && value.rect.right <= value.documentWidth, 'Matrix viewport must stay inside the page');
  assert.equal(value.documentScrollWidth, value.documentWidth, 'The page must not replace matrix scrolling');
  assert.deepEqual(value.hiddenText, [], 'Rendered matrix text must remain visible and inside its cell');
  assert.ok(value.renderedTextCount > 0, 'Rendered matrix text needs actual text rectangles');
  assert.deepEqual(value.clippedCells, [], 'Matrix cells must retain their complete content');
  assert.equal(value.rowHeaders.length, 5);
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
  const hiddenText = [];
  let renderedTextCount = 0;
  for (const cell of wrapper.querySelectorAll('th,td')) {
    const cellBox = cell.getBoundingClientRect();
    const walker = document.createTreeWalker(cell, NodeFilter.SHOW_TEXT);
    for (let node = walker.nextNode(); node; node = walker.nextNode()) {
      if (!node.textContent.trim()) continue;
      let hidden = false;
      // Opacity is not inherited; inspect the rendered leaf and its owners.
      for (let owner = node.parentElement; owner; owner = owner.parentElement) {
        const style = getComputedStyle(owner);
        if (style.display === 'none' || style.visibility !== 'visible' ||
            Number(style.opacity) === 0 || style.contentVisibility === 'hidden') hidden = true;
        if (owner === wrapper) break;
      }
      const range = document.createRange();
      range.selectNodeContents(node);
      const rectangles = [...range.getClientRects()].filter(box => box.width > 0 && box.height > 0);
      const clipped = rectangles.some(box => box.left < cellBox.left || box.right > cellBox.right ||
        box.top < cellBox.top || box.bottom > cellBox.bottom);
      if (hidden || !rectangles.length || clipped) {
        hiddenText.push({ text: node.textContent.trim(), hidden, rectangles: rectangles.length, clipped });
      } else {
        renderedTextCount++;
      }
    }
  }
  const lastCells = [...wrapper.querySelectorAll('tbody td:last-child')].map(bounds);
  const firstCells = [...wrapper.querySelectorAll('tbody td:first-of-type')].map(bounds);
  return { rect: bounds(wrapper), contentLeft, contentRight,
    clientWidth: wrapper.clientWidth, clientHeight: wrapper.clientHeight,
    scrollWidth: wrapper.scrollWidth, scrollLeft: wrapper.scrollLeft,
    overflowX: getComputedStyle(wrapper).overflowX,
    focused: document.activeElement === wrapper, rowHeaders, clippedCells, hiddenText, renderedTextCount, firstCells, lastCells,
    documentWidth: document.documentElement.clientWidth, documentScrollWidth: document.documentElement.scrollWidth,
    pageX: scrollX, pageY: scrollY };
}


// A first animation delta is not completion of the preceding native key.
export class MatrixKeyDeadline extends Error {
  constructor(operation) {
    super(`Native matrix ${operation} exceeded its five-second traversal deadline`);
    this.name = 'MatrixKeyDeadline';
    this.operation = operation;
  }
}

export async function completeMatrixKey({ arm, press, wait, observe, before, direction, deadline,
  validateMovement = requireMatrixMovement,
  now = () => performance.now(), schedule = (callback, delay) => {
    const timer = setTimeout(callback, delay);
    return () => clearTimeout(timer);
  } }) {
  const expired = Symbol('matrix deadline');
  let cancel = () => {};
  const alarm = new Promise(resolve => {
    const check = () => {
      const left = deadline - now();
      if (left > 0) { cancel = schedule(check, Math.ceil(left)); return; }
      resolve(expired);
    };
    cancel = schedule(check, Math.max(0, Math.ceil(deadline - now())));
  });
  const run = async (operation, action, cleanup = false) => {
    const remaining = deadline - now();
    if (!cleanup && remaining <= 0) throw new MatrixKeyDeadline(operation);
    const task = Promise.resolve().then(() => {
      if (!cleanup && now() >= deadline) throw new MatrixKeyDeadline(operation);
      return action(Math.max(0, deadline - now()));
    }).then(value => ({ value }));
    const result = await Promise.race([task, alarm]);
    if (result === expired || now() >= deadline) throw new MatrixKeyDeadline(operation);
    return result.value;
  };
  let signal;
  let after;
  let failure;
  try {
    signal = await run('listener setup', arm);
    await run('keypress', press);
    await run('scrollend observation', remaining => wait(signal, remaining));
    after = await run('geometry observation', observe);
    validateMovement(before, after, direction);
  } catch (error) {
    failure = error;
  } finally {
    try {
      if (signal) await run('listener cleanup', () => signal.dispose(), true);
    } catch (error) {
      failure ||= error;
    } finally {
      cancel();
    }
  }
  if (failure) throw failure;
  if (now() >= deadline) throw new MatrixKeyDeadline('completion');
  return after;
}
