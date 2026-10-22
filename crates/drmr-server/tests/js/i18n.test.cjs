'use strict';
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const test = require('node:test');
const vm = require('node:vm');
function translations(language) {
  const stored = [], events = [], select = {};
  const sandbox = { window: {}, Event: class { constructor(type) { this.type = type; } },
    localStorage: { getItem: () => language, setItem: (key, value) => stored.push([key, value]) },
    document: { documentElement: {}, querySelectorAll: () => [], getElementById: () => select, dispatchEvent: event => events.push(event.type) } };
  vm.runInNewContext(fs.readFileSync(path.join(__dirname, '../../static/js/i18n.js'), 'utf8'), sandbox);
  return { i18n: sandbox.window.App.I18n, stored, events, document: sandbox.document, select };
}
test('both languages cover the standalone shell and current status labels', () => {
  for (const language of ['en', 'ja']) {
    const h = translations(language); h.i18n.apply();
    assert.equal(h.document.documentElement.lang, language);
    assert.equal(h.select.value, language);
    for (const key of ['brandName', 'language', 'skipToContent', 'leaseIdle', 'stopOutcomeUnknown', 'close']) assert.notEqual(h.i18n.t(key), key);
    assert.equal(h.i18n.t('close'), language === 'ja' ? '閉じる' : 'Close');
    assert.equal(h.i18n.has('commonLegacy'), false);
  }
});
test('language changes persist only the preference and notify the current common view', () => {
  const h = translations('en'); h.i18n.setLanguage('ja');
  assert.deepEqual(h.stored, [['drmr-language', 'ja']]);
  assert.deepEqual(h.events, ['workbench-language']);
  assert.equal(h.document.documentElement.lang, 'ja');
  h.i18n.setLanguage('unsupported');
  assert.equal(h.i18n.getLanguage(), 'en');
  assert.deepEqual(h.stored.at(-1), ['drmr-language', 'en']);
});
