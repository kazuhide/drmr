'use strict';

const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const test = require('node:test');
const vm = require('node:vm');

test('stop uses the selected encoded job ID and a JSON object with no process parameters', async () => {
  const calls = [];
  const sandbox = { window: {}, fetch: (url, options) => {
    calls.push({ url, options });
    return Promise.resolve({ ok: true, text: () => Promise.resolve('{"state":"running","stop_requested":true}') });
  } };
  vm.runInNewContext(fs.readFileSync(path.join(__dirname, '../../static/js/api.js'), 'utf8'), sandbox);
  for (const id of ['job', '__proto__', 'a/b?x#']) {
    assert.equal((await sandbox.window.App.Api.stopJob(id)).state, 'running');
    const { url, options } = calls.at(-1);
    assert.equal(url, '/api/jobs/' + encodeURIComponent(id) + '/stop');
    assert.equal(options.method, 'POST');
    assert.equal(options.headers['Content-Type'], 'application/json');
    assert.equal(options.body, '{}');
  }
});
