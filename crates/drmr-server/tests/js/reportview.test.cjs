'use strict';
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const test = require('node:test');
const vm = require('node:vm');

class Element {
  constructor(tagName) { this.tagName = tagName; this.nodeType = 1; this.children = []; this.attributes = {}; this.dataset = {}; this.textContent = ''; this.classList = { toggle() {} }; }
  get options() { return this.children; }
  appendChild(node) { this.children.push(node); return node; }
  replaceChildren() { this.children = []; }
  setAttribute(name, value) { this.attributes[name] = value; }
  addEventListener() {}
}
function descendants(node) { return [node, ...node.children.flatMap(descendants)]; }
function render(kind, readiness) {
  const sandbox = {
    window: { App: { I18n: { t: key => key, has: () => false, getLanguage: () => 'en' } } },
    document: { createElement: tag => new Element(tag), createTextNode: text => ({ nodeType: 3, textContent: text, children: [] }) },
    URL, location: { origin: 'http://127.0.0.1' },
  };
  for (const file of ['ui.js', 'reportview.js']) vm.runInNewContext(fs.readFileSync(path.join(__dirname, '../../static/js', file), 'utf8'), sandbox);
  const container = new Element('div');
  sandbox.window.App.ReportView.render(container, {
    all_pass: true, full_enumeration: true, round_trip_verified: true,
    dash_url: '/output/asset/manifest.mpd', hls_url: '/output/asset/master.m3u8',
    output_readiness: readiness, tracks: [], checks: [{ id: 'retained-check', status: 'PASS' }],
  }, { kind, id: 'asset' }, { pageLink: true });
  return descendants(container);
}

test('diagnostic catalog reports retain checks and report links without advertising media authority', () => {
  for (const readiness of [undefined, { state: 'unverified' }, { state: 'unknown' }]) {
    const nodes = render('catalog', readiness);
    assert.ok(nodes.some(node => node.textContent === 'reportDiagnostic'));
    assert.ok(nodes.some(node => node.textContent === 'retained-check'));
    assert.ok(nodes.some(node => node.textContent === 'outputUnverified'));
    const links = nodes.filter(node => node.tagName === 'a').map(node => node.attributes.href);
    assert.deepEqual(links, ['/reports/catalog/asset']);
  }
});

test('qualified catalog reports and unchanged HLS reports retain their published media links', () => {
  for (const [kind, readiness] of [['catalog', { state: 'verified' }], ['hls', undefined]]) {
    const nodes = render(kind, readiness);
    assert.equal(nodes.some(node => node.textContent === 'reportDiagnostic'), false);
    const links = nodes.filter(node => node.tagName === 'a').map(node => node.attributes.href);
    assert.ok(links.includes('/output/asset/manifest.mpd'));
    assert.ok(links.includes('/output/asset/master.m3u8'));
    assert.ok(nodes.some(node => node.textContent === 'checksPass'));
  }
});


test('comparison playback drops both old contexts when fresh catalog authority is missing', () => {
  const elements = new Map();
  const byId = id => { if (!elements.has(id)) elements.set(id, new Element('div')); return elements.get(id); };
  let stopped = 0;
  const sandbox = {
    window: { App: {
      I18n: { t: key => key, has: () => false },
      createShakaPlayer: () => ({ onState() {}, getState: () => ({ status: 'idle' }), stop() { stopped += 1; } }),
    } },
    document: { createElement: tag => new Element(tag), createTextNode: text => ({ nodeType: 3, textContent: text, children: [] }) },
    URL, location: { origin: 'http://127.0.0.1' },
  };
  for (const file of ['ui.js', 'playback.js']) vm.runInNewContext(fs.readFileSync(path.join(__dirname, '../../static/js', file), 'utf8'), sandbox);
  const playback = sandbox.window.App.Playback.create(byId);
  playback.prepareCatalog('asset', { old_url: '/source/asset/source.mpd', new_url: '/output/asset/output.mpd', output_readiness: { state: 'verified' } });
  assert.equal(playback.isVisible(), true);
  assert.equal(byId('play-new-btn').disabled, false);
  playback.prepareCatalog('asset', { old_url: '/source/asset/source.mpd', new_url: '/output/asset/output.mpd' });
  assert.equal(playback.isVisible(), false);
  assert.equal(byId('play-new-btn').disabled, true);
  assert.equal(byId('play-old-btn').disabled, true);
  assert.equal(stopped, 2);
});
