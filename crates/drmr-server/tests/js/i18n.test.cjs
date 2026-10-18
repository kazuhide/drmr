'use strict';

const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const test = require('node:test');
const vm = require('node:vm');

function translations(language) {
  const sandbox = {
    window: {},
    localStorage: { getItem: () => language },
  };
  vm.runInNewContext(fs.readFileSync(path.join(__dirname, '../../static/js/i18n.js'), 'utf8'), sandbox);
  return sandbox.window.App.I18n;
}

for (const language of ['en', 'ja']) {
  test(language + ': known source access errors give fixed recovery guidance', () => {
    const i18n = translations(language);
    for (const status of [422, 400]) {
      for (const code of ['source-profile-invalid', 'source-not-allowed']) {
        const message = i18n.error({ status, body: {
          error: code,
          message: 'untrusted-response-secret',
          url: 'https://source.invalid/?secret=untrusted-response-secret',
        } });
        assert.match(message, /HTTPS/);
        assert.equal(message, i18n.t('sourceAccessError'));
        assert.ok(!message.includes('untrusted-response-secret'));
      }
    }
  });

  test(language + ': untrusted classifications and existing status classes keep safe fallbacks', () => {
    const i18n = translations(language);
    for (const code of ['private-sentinel', '__proto__', 'constructor', 'toString', null, {
      toString() { throw new Error('untrusted conversion must not execute'); },
    }]) {
      for (const status of [400, 415, 422]) {
        assert.equal(i18n.error({ status, body: { error: code, message: 'private-sentinel' } }), i18n.t('invalidError'));
      }
    }
    for (const [status, key] of [[403, 'forbiddenError'], [404, 'notFoundError'], [409, 'conflictError'], [410, 'expiredError'], [500, 'requestError']]) {
      assert.equal(i18n.error({ status, body: { error: 'source-profile-invalid' } }), i18n.t(key));
    }
    assert.equal(i18n.error(null), i18n.t('networkError'));
    assert.equal(i18n.error({ status: 409, body: { error: 'job-busy' } }), i18n.t('busyError'));
  });
}
