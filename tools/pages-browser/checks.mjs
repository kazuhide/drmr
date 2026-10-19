import assert from 'node:assert/strict';
import { AssertionError } from 'node:assert';

export function requireLayout(value) {
  assert.ok(value.scrollWidth <= value.clientWidth, 'Page must not overflow horizontally');
  assert.deepEqual(value.clipped, [], 'Normal text and layout boxes must not be clipped');
}

export function requireFocus(value) {
  assert.equal(value.matchesTarget, true, 'Keyboard focus must stay on the expected target');
  assert.ok(value.width > 0 && value.height > 0 && value.inViewport, 'Keyboard focus must be visible');
  assert.ok(value.outlineWidth >= 2 && value.outlineStyle !== 'none', 'Keyboard focus needs an outline');
}

export function requireImages(images) {
  assert.ok(images.length > 0 && images.every(image => image.complete && image.width > 0), 'Images must load');
}

// These functions execute inside CI Chromium, without changing production styles.
export function layoutObservation() {
  const width = document.documentElement.clientWidth;
  const clipped = [];
  for (const element of document.querySelectorAll('h1,h2,h3,p,li,dt,dd,figcaption,a,img,pre,figure,nav')) {
    const rect = element.getBoundingClientRect();
    if (rect.width && (rect.left < 0 || rect.right > width)) clipped.push(element.tagName + ':' + element.textContent.trim().slice(0, 60));
  }
  return { clientWidth: width, scrollWidth: document.documentElement.scrollWidth, clipped };
}

export function focusObservation(expected) {
  const element = document.activeElement;
  const rect = element.getBoundingClientRect();
  const style = getComputedStyle(element);
  const identify = node => ({ tag: node.tagName, id: node.id,
    label: node.getAttribute('aria-label'), href: node.getAttribute('href'),
    text: node.textContent.trim().replace(/\s+/g, ' ').slice(0, 160) });
  return { target: identify(expected || element), active: identify(element),
    matchesTarget: !expected || element === expected, at: performance.now(),
    rect: { top: rect.top, bottom: rect.bottom, left: rect.left, right: rect.right,
      x: rect.x, y: rect.y, width: rect.width, height: rect.height },
    scroll: { x: scrollX, y: scrollY }, viewport: { width: innerWidth, height: innerHeight },
    width: rect.width, height: rect.height,
    inViewport: rect.top >= 0 && rect.bottom <= innerHeight && rect.left >= 0 && rect.right <= innerWidth,
    outlineWidth: Number.parseFloat(style.outlineWidth), outlineStyle: style.outlineStyle };
}


// Only this owned polling boundary constructs the accepted timeout kind.
export class FocusConditionTimeout extends Error {
  constructor(samples, elapsedMs) {
    super('Complete keyboard focus condition did not pass within five seconds');
    this.name = 'FocusConditionTimeout';
    this.samples = samples;
    this.elapsedMs = elapsedMs;
  }
}

export async function pollFocus(measure, samples, {
  now = () => performance.now(),
  pause = milliseconds => new Promise(resolve => setTimeout(resolve, milliseconds)),
} = {}) {
  const start = now();
  for (;;) {
    // Transport/evaluation errors propagate; they are never condition timeouts.
    const sample = await measure();
    samples.push(sample);
    const elapsed = now() - start;
    let complete = false;
    try {
      requireFocus(sample);
      complete = true;
    } catch (error) {
      if (!(error instanceof AssertionError)) throw error;
    }
    // A late successful measurement cannot pass the deadline either.
    if (elapsed >= 5000) throw new FocusConditionTimeout(samples, elapsed);
    if (complete) return sample;
    await pause(Math.min(100, 5000 - elapsed));
  }
}

export function requireClippedFocusTimeout(failure, check) {
  // Unexpected evaluation, transport, polling or persistence failures stay fatal.
  if (!(failure instanceof FocusConditionTimeout)) throw failure || new Error('Expected a condition timeout');
  assert.equal(failure.samples, check.samples);
  assert.ok(Number.isFinite(failure.elapsedMs) && failure.elapsedMs >= 5000);
  assert.equal(check.passed, false);
  assert.ok(check.samples.length > 1);
  for (const sample of check.samples) {
    assert.equal(sample.matchesTarget, true);
    assert.equal(sample.inViewport, false);
    assert.equal(sample.outlineWidth, 3);
    assert.equal(sample.outlineStyle, 'solid');
    const { rect, viewport } = sample;
    assert.ok([rect.top, rect.bottom, rect.left, rect.right, rect.width, rect.height,
      viewport.width, viewport.height, sample.width, sample.height].every(Number.isFinite));
    assert.ok(viewport.width > 0 && viewport.height > 0 && sample.width > 0 && sample.height > 0);
    assert.equal(rect.width, sample.width);
    assert.equal(rect.height, sample.height);
    assert.ok(rect.width > 0 && rect.height > 0 && rect.right > rect.left);
    assert.ok(0 <= rect.top && rect.top < viewport.height && viewport.height < rect.bottom);
    assert.ok(0 <= rect.left && rect.right <= viewport.width);
  }
}
