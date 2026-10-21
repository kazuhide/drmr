'use strict';
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const test = require('node:test');
const vm = require('node:vm');
class Element {
  constructor(tag) { this.tagName = tag; this.nodeType = 1; this.children = []; this.dataset = {}; this.attrs = new Map(); this.listeners = new Map(); this.textContent = ''; this.value = ''; }
  appendChild(node) { node.parentNode = this; this.children.push(node); return node; }
  replaceChildren() { this.children = []; }
  setAttribute(k, v) { this.attrs.set(k, v); }
  getAttribute(k) { return this.attrs.get(k) || null; }
  removeAttribute(k) { this.attrs.delete(k); }
  addEventListener(k, v) { this.listeners.set(k, v); }
  dispatch(k) { this.listeners.get(k)?.({ target: this, preventDefault() {} }); }
  showModal() { this.open = true; }
  close() { this.open = false; this.dispatch('close'); }
  focus() { this.focused = true; }
}
const descendants = node => [node, ...node.children.flatMap(descendants)];
function harness(files) {
  const document = { createElement: tag => new Element(tag), createTextNode: text => ({ nodeType: 3, textContent: text, children: [] }), activeElement: null };
  const box = { document, window: { App: { I18n: { t: key => key }, CommonApi: { files } } } };
  for (const file of ['ui.js', 'common-files.js']) vm.runInNewContext(fs.readFileSync(path.join(__dirname, '../../static/js', file), 'utf8'), box);
  const host = new Element('div'), picker = box.window.App.CommonFiles.create(host);
  picker.update({ server_id: 'server_1', roots: [{ root_ref: 'root_1', label: 'Allowed', roles: ['output-parent', 'keys'] }] });
  return { host, picker };
}
test('the root folder returns only the issued output-parent ref, never the root or directory ref', async () => {
  let selected;
  const h = harness(async () => ({ server_id: 'server_1', output_parent_ref: 'parent_1', entries: [], next_cursor: null }));
  await h.picker.open('output-parent', value => { selected = value; });
  descendants(h.host).find(node => node.textContent === 'commonChooseFolder').dispatch('click');
  assert.equal(selected.ref, 'parent_1'); assert.notEqual(selected.ref, 'root_1');
});
test('key selection stores a role-bound reference without requesting contents', async () => {
  let selected; const requests = [];
  const h = harness(async query => { requests.push(query); return { server_id: 'server_1', output_parent_ref: null, entries: [{ kind: 'file', file_ref: 'key_ref', label: 'keys.json' }], next_cursor: null }; });
  await h.picker.open('keys', value => { selected = value; });
  descendants(h.host).find(node => node.textContent === 'keys.json').dispatch('click');
  assert.equal(selected.ref, 'key_ref'); assert.equal(selected.role, 'keys');
  assert.equal(requests.length, 1); assert.equal(requests[0].role, 'keys');
});
test('an earlier folder response cannot replace a newly opened role picker', async () => {
  let deliver;
  const h = harness(query => query.role === 'keys' ? new Promise(resolve => { deliver = resolve; }) : Promise.resolve({ server_id: 'server_1', output_parent_ref: 'parent_1', entries: [], next_cursor: null }));
  const old = h.picker.open('keys', () => assert.fail('retired selection'));
  await h.picker.open('output-parent', () => {});
  deliver({ server_id: 'server_1', output_parent_ref: null, entries: [{ kind: 'file', file_ref: 'old', label: 'old-key.json' }], next_cursor: null });
  await old;
  assert.equal(descendants(h.host).some(node => node.textContent === 'old-key.json'), false);
});

test('loading a second page leaves first-page file buttons usable in the same folder', async () => {
  let selected;
  const h = harness(async query => ({ server_id: 'server_1', output_parent_ref: null,
    entries: [{ kind: 'file', file_ref: query.cursor ? 'second_ref' : 'first_ref', label: query.cursor ? 'second.json' : 'first.json' }],
    next_cursor: query.cursor ? null : 'next_page' }));
  await h.picker.open('keys', value => { selected = value; });
  descendants(h.host).find(node => node.textContent === 'commonMore').dispatch('click');
  await new Promise(setImmediate);
  descendants(h.host).find(node => node.textContent === 'first.json').dispatch('click');
  assert.equal(selected?.ref, 'first_ref');
});
