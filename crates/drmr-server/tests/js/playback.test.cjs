"use strict";

const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const test = require('node:test');
const vm = require('node:vm');
const flush = () => new Promise(setImmediate);
function deferred() { let resolve, reject; const promise = new Promise((a, b) => { resolve = a; reject = b; }); return { promise, resolve, reject }; }
class Element {
  constructor() { this.children = []; this.listeners = new Map(); this.value = ''; this.textContent = ''; this.hidden = false; this.classList = { toggle() {} }; }
  replaceChildren() { this.children = []; }
  appendChild(node) { this.children.push(node); return node; }
  setAttribute(key, value) { this[key] = value; }
  addEventListener(key, callback) { this.listeners.set(key, callback); }
  get options() { return this.children; }
  dispatch(key) { this.listeners.get(key)?.(); }
}
function harness() {
  const elements = new Map(), players = [], plays = [];
  const element = id => { if (!elements.has(id)) elements.set(id, new Element()); return elements.get(id); };
  for (const side of ['old', 'new']) element('video-' + side).play = () => { const call = deferred(); plays.push(call); return call.promise; };
  const sandbox = { window: { App: { I18n: { t: key => key }, codecFamily: () => 'avc1' } }, URL, location: { origin: 'http://127.0.0.1' },
    document: { createElement: () => new Element(), createTextNode: text => ({ textContent: text }) } };
  vm.runInNewContext(fs.readFileSync(path.join(__dirname, '../../static/js/ui.js'), 'utf8'), sandbox);
  sandbox.window.App.createShakaPlayer = () => {
    let state = { status: 'idle', keyStatus: 'n/a' }, observer = () => {};
    const player = { selected: [], onState: callback => { observer = callback; }, getState: () => state,
      load: (_asset, url) => { state = { status: 'loading', phase: 'ready', url, keyStatus: 'configured' }; observer(state); return Promise.resolve(); },
      stop: () => { state = { status: 'idle', keyStatus: 'n/a' }; observer(state); return Promise.resolve(); },
      selectTextRendition: () => true, selectRendition: id => { player.selected.push(id); return true; } };
    players.push(player); return player;
  };
  vm.runInNewContext(fs.readFileSync(path.join(__dirname, '../../static/js/playback.js'), 'utf8'), sandbox);
  const playback = sandbox.window.App.Playback.create(element);
  function prepare(asset, ids = ['ordinary']) { playback.prepareCatalog(asset, { output_readiness: { state: 'verified' }, old_url: '/' + asset + '.mpd', new_url: '/result.mpd',
    renditions: { old: ids.map(rep_id => ({ rep_id, content_type: 'video', codec: 'avc1' })), new: [] } }); }
  return { element, players, plays, playback, prepare };
}

test('valid prototype-shaped rendition IDs remain selectable and true duplicates are removed', async () => {
  const h = harness(); const ids = ['ordinary', '__proto__', 'constructor', 'toString', 'hasOwnProperty'];
  h.prepare('a', ids.concat('__proto__'));
  assert.deepEqual(Array.from(h.element('old-rendition-select').options, node => node.value), ['', ...ids]);
  for (const id of ids) { h.element('old-rendition-select').value = id; h.element('old-rendition-select').dispatch('change'); await flush(); }
  assert.deepEqual(h.players[0].selected, ids);
});
for (const transition of ['replacement', 'reset', 'same-content retry']) {
  test('a retired play rejection cannot change state after ' + transition, async () => {
    const h = harness(); h.prepare('a'); h.element('play-old-btn').dispatch('click'); await flush();
    if (transition === 'replacement') { h.playback.hide(); h.prepare('b'); }
    else if (transition === 'reset') h.playback.hide();
    else { h.element('play-old-btn').dispatch('click'); await flush(); }
    const before = h.element('old-state').textContent;
    h.plays[0].reject({ name: 'NotAllowedError' }); await flush();
    assert.equal(h.element('old-state').textContent, before);
  });
}
test('the current play rejection keeps actionable guidance', async () => {
  const h = harness(); h.prepare('a'); h.element('play-old-btn').dispatch('click'); await flush();
  h.plays[0].reject({ name: 'NotAllowedError' }); await flush();
  assert.match(h.element('old-state').textContent, /autoplayBlocked/);
});
test('a synchronous play failure is reported for its current attempt', async () => {
  const h = harness(); h.prepare('a'); h.element('video-old').play = () => { throw new Error('untrusted raw detail'); };
  h.element('play-old-btn').dispatch('click'); await flush();
  assert.match(h.element('old-state').textContent, /playFailed/);
  assert.doesNotMatch(h.element('old-state').textContent, /untrusted/);
  assert.equal(h.element('play-old-btn').disabled, false);
});
