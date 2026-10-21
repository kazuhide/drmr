'use strict';
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const test = require('node:test');
const vm = require('node:vm');
const flush = () => new Promise(setImmediate);
function harness() {
  const events = new Map(), buttons = new Map(), timers = new Map();
  const calls = [], owner = { phase: 'unknown', jobId: null };
  const data = { owner, creation: null, capabilities: { execution: { availability: 'idle' } } };
  let onChange;
  const model = { snapshot: () => data, refresh: async () => { calls.push('GET refresh'); }, recover: async () => { calls.push('GET recovery'); }, recoverCreation: async () => { calls.push('GET creation'); } };
  const view = { update: () => calls.push('render'), translate: () => calls.push('translate'), clearSecrets: () => calls.push('clear URL'), hide() {}, show() {}, newInput: () => calls.push('new input') };
  function button(id) { if (!buttons.has(id)) buttons.set(id, { addEventListener: (name, fn) => events.set(id + ':' + name, fn) }); return buttons.get(id); }
  const box = { console, setTimeout: fn => { timers.set(1, fn); return 1; }, clearTimeout: id => timers.delete(id),
    document: { getElementById: button, addEventListener: (name, fn) => events.set(name, fn) },
    window: { addEventListener: (name, fn) => events.set(name, fn), App: {
      ConsoleBridge: { ownsExecution: () => false, showCommon() {}, commonChanged() {} },
      CommonState: { create: options => { onChange = options.onChange; return model; } },
      CommonView: { create: () => view, ownerText: () => 'common owner' }
    } } };
  vm.runInNewContext(fs.readFileSync(path.join(__dirname, '../../static/js/common-console.js'), 'utf8'), box);
  events.get('DOMContentLoaded')();
  return { calls, data, owner, timers, events, onChange: () => onChange(), controller: box.window.App.CommonConsole };
}
test('observation uses GET recovery and never replaces an uncertain owner', async () => {
  const h = harness(); await flush();
  assert.deepEqual(h.calls.filter(v => v.startsWith('GET')), ['GET refresh', 'GET recovery']);
  assert.equal(h.data.owner, h.owner); assert.equal(h.controller.isBusy(), true);
});
test('a locale event updates the existing view without changing ownership or starting operations', async () => {
  const h = harness(); await flush(); h.calls.length = 0;
  h.events.get('workbench-language')();
  assert.deepEqual(h.calls, ['translate']); assert.equal(h.data.owner, h.owner);
});
test('pagehide clears temporary URLs and polling without posting Stop or closing a session', async () => {
  const h = harness(); await flush(); h.calls.length = 0;
  h.events.get('pagehide')();
  assert.deepEqual(h.calls, ['clear URL']); assert.equal(h.timers.size, 0); assert.equal(h.data.owner, h.owner);
  h.onChange(); assert.deepEqual(h.calls, ['clear URL']);
});
