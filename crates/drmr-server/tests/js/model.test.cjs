'use strict';

const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const test = require('node:test');
const vm = require('node:vm');

function model() {
  const sandbox = { window: {} };
  vm.runInNewContext(fs.readFileSync(path.join(__dirname, '../../static/js/model.js'), 'utf8'), sandbox);
  return sandbox.window.App.Model;
}

test('content IDs that match object properties remain independent entries', () => {
  const ids = ['constructor', 'toString', '__proto__', 'hasOwnProperty'];
  const jobs = ids.map((asset_id, index) => ({ asset_id, kind: 'analysis', state: 'succeeded', analysis_id: `analysis-${index}` }));
  const contents = model().compose([], jobs, null);
  assert.equal(contents.length, ids.length);
  for (const id of ids) {
    const content = contents.find(row => row.asset_id === id);
    assert.equal(content.jobs.length, 1);
    assert.equal(content.state, 'analyzed');
  }
});

test('catalog entries retain jobs whose IDs match inherited properties', () => {
  const contents = model().compose([{ asset_id: 'constructor', label: 'Sample' }], [{ asset_id: 'constructor', kind: 'convert', state: 'succeeded' }], null);
  assert.equal(contents.length, 1);
  assert.equal(contents[0].jobs.length, 1);
  assert.equal(contents[0].label, 'Sample');
});

test('report check IDs cannot collide with object properties', () => {
  const result = model().checkStatuses({ checks: [{ id: '__proto__', status: 'FAIL' }, { id: 'constructor', status: 'PASS' }] }, 'catalog');
  assert.equal(result.__proto__, 'FAIL');
  assert.equal(result.constructor, 'PASS');
});
