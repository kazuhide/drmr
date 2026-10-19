import assert from 'node:assert/strict';

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
