'use strict';
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const test = require('node:test');
const vm = require('node:vm');
function dictionary(lang) {
  const sandbox = { window: {}, localStorage: { getItem: () => lang } };
  vm.runInNewContext(fs.readFileSync(path.join(__dirname, '../../static/js/i18n.js'), 'utf8'), sandbox);
  return sandbox.window.App.I18n;
}
test('every common-workflow label has English and Japanese copy with matching interpolation', () => {
  const en = dictionary('en'), ja = dictionary('ja');
  const common = en.keys('en').filter(key => key.startsWith('common'));
  assert.deepEqual(Array.from(ja.keys('ja').filter(key => key.startsWith('common'))).sort(), Array.from(common).sort());
  for (const key of common) {
    assert.notEqual(en.t(key), key, key); assert.notEqual(ja.t(key), key, key);
    const vars = text => [...text.matchAll(/\{([A-Za-z]+)\}/g)].map(match => match[1]).sort();
    assert.deepEqual(vars(en.t(key)), vars(ja.t(key)), key);
    assert.doesNotMatch(ja.t(key), /出力世代|鍵コンテキスト/, key);
  }
});
test('all literal common-view and report labels resolve without exposing an untranslated key', () => {
  const en = dictionary('en');
  for (const file of ['common-view.js', 'common-report.js', 'common-files.js']) {
    const source = fs.readFileSync(path.join(__dirname, '../../static/js', file), 'utf8');
    const keys = new Set([...source.matchAll(/['"](common[A-Z][A-Za-z0-9_-]*)['"]/g)].map(match => match[1]).filter(key => !key.endsWith('_')));
    for (const key of keys) assert.ok(en.has(key), file + ': ' + key);
  }
});
