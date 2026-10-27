'use strict';
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const test = require('node:test');
const vm = require('node:vm');
function harness(imported = false, localized = false) {
  let document, language = 'en', view;
  const translations = localized && Object.fromEntries(['en', 'ja'].map(lang => {
    const box = { window: {}, localStorage: { getItem: () => lang } };
    vm.runInNewContext(fs.readFileSync(path.join(__dirname, '../../static/js/i18n.js'), 'utf8'), box);
    return [lang, box.window.App.I18n];
  }));
  class Element {
    constructor(tag) { this.tagName = tag; this.nodeType = 1; this.children = []; this.dataset = {}; this.attrs = new Map(); this.listeners = new Map(); this.textContent = ''; this.value = ''; this.disabled = false; this.hidden = false; }
    appendChild(node) { node.parentNode = this; this.children.push(node); return node; }
    replaceChildren() { this.children = []; }
    remove() { if (this.parentNode) this.parentNode.children = this.parentNode.children.filter(n => n !== this); }
    setAttribute(k, v) { this.attrs.set(k, v); }
    getAttribute(k) { return this.attrs.get(k) || null; }
    removeAttribute(k) { this.attrs.delete(k); }
    addEventListener(k, v) { this.listeners.set(k, v); }
    dispatch(k) { if (!(k === 'click' && this.disabled)) this.listeners.get(k)?.({ target: this, preventDefault() {} }); }
    showModal() { this.open = true; }
    close() { this.open = false; }
    focus() { document.activeElement = this; }
  }
  const descendants = node => [node, ...node.children.flatMap(descendants)];
  const host = new Element('article'), nav = new Element('ul'), global = new Element('div');
  document = { activeElement: null, createElement: tag => new Element(tag), createTextNode: text => ({ nodeType: 3, textContent: text, children: [] }) };
  const session = { session_id: 'session_1', source: { kind: imported ? 'saved-plan' : 'local', label: 'input.mp4' }, requests_used: 0, plan_refs: [] };
  const plan = imported ? { plan_ref: 'plan_1', plan_id: 'b'.repeat(64), plan_schema_version: 1, outputs: [{ id: null, format: 'mp4', target: 'clear' }], destination_state: 'present', export_url: '/api/common/plans/plan_1/export' } : null;
  const record = { session, plan, plans: new Map(plan ? [['plan_1', plan]] : []), reports: new Map(), analysis: { analysis_ref: 'analysis_1' }, error: null, lastJob: null, outcome: null };
  const data = { serverId: 'server_1', capabilities: { server_id: 'server_1', roots: [], worker_policy: { maximum: 8 }, execution: { availability: 'idle' } }, sessions: new Map([['session_1', record]]), selectedId: 'session_1', owner: null, creation: null, error: null };
  const sent = [], opened = [];
  const model = { snapshot: () => data, selected: () => data.sessions.get(data.selectedId), canStart: () => !data.owner,
    select: async sid => { data.selectedId = sid; }, submit: async (operation, runtime) => { sent.push({ operation, runtime }); },
    createSession: source => { opened.push(source); return new Promise(() => {}); }, recover() {}, recoverCreation() {}, stop() {}, close() {},
    usePlan() {}, invalidatePlan() { record.plan = null; } };
  const sandbox = { document, URL, Map, window: { App: { I18n: { t: (key, values) => translations ? translations[language].t(key, values) : language + ':' + key + (values ? JSON.stringify(values) : ''), getLanguage: () => language }, CommonApi: { files: async () => {} } } } };
  for (const file of ['ui.js', 'common-files.js', 'common-report.js', 'common-view.js']) vm.runInNewContext(fs.readFileSync(path.join(__dirname, '../../static/js', file), 'utf8'), sandbox);
  view = sandbox.window.App.CommonView.create(host, nav, global, model);
  view.update();
  const find = id => descendants(host).find(node => node.getAttribute && node.getAttribute('id') === id || node.id === id);
  return { view, data, record, sent, opened, document, host, nav, global, find, descendants, setLanguage: value => { language = value; } };
}
test('polling and locale changes retain the exact editing nodes, draft values and focus', () => {
  const h = harness();
  const input = h.find('common-output-1-id'); assert.ok(input);
  input.value = 'edited-output'; input.focus(); input.dispatch('input');
  h.data.owner = { sessionId: 'session_1', kind: 'plan', phase: 'running', stop: 'none', jobId: 'job_1' };
  h.view.update();
  assert.equal(h.find('common-output-1-id'), input); assert.equal(input.value, 'edited-output');
  assert.equal(h.document.activeElement, input);
  h.setLanguage('ja'); h.view.translate();
  assert.equal(h.find('common-output-1-id'), input); assert.equal(input.value, 'edited-output');
  assert.equal(h.document.activeElement, input);
});
test('the decode choice defaults off and survives polling before media submission', () => {
  const h = harness(), decode = h.find('common-decode');
  assert.ok(decode);
  assert.equal(decode.checked, false);
  assert.equal(decode.getAttribute('aria-describedby'), 'common-decode-help');
  decode.checked = true; decode.focus(); decode.dispatch('change');
  h.view.update(); h.setLanguage('ja'); h.view.translate();
  assert.equal(h.find('common-decode'), decode);
  assert.equal(decode.checked, true);
  assert.equal(h.document.activeElement, decode);
  h.find('common-analyze').dispatch('click');
  assert.equal(h.sent.length, 1);
  assert.equal(h.sent[0].runtime.decode, true);
});

test('each output processing selector survives polling, translates and invalidates the saved plan', () => {
  const h = harness(false, true), select = h.find('common-output-1-processing');
  assert.ok(select); assert.equal(select.tagName, 'select'); assert.equal(select.value, 'auto');
  assert.deepEqual(select.children.map(option => option.value), ['auto', 'segments', 'stitch']);
  select.value = 'segments'; select.focus(); select.dispatch('change');
  h.view.update(); h.setLanguage('ja'); h.view.translate();
  assert.equal(h.find('common-output-1-processing'), select);
  assert.equal(select.value, 'segments'); assert.equal(h.document.activeElement, select);
  h.find('common-add-output').dispatch('click');
  assert.equal(h.find('common-output-1-processing').value, 'segments');
  assert.equal(h.find('common-output-2-processing').value, 'auto');
  assert.ok(select.getAttribute('aria-label').includes('1'));
  assert.ok(select.getAttribute('aria-describedby'));
  h.record.plan = { plan_ref: 'old' };
  select.value = 'stitch'; select.dispatch('change');
  assert.equal(h.record.plan, null);
});

test('a saved processing choice is displayed without adding conversion overrides', () => {
  const h = harness(true, true);
  h.record.plan.plan_schema_version = 6;
  h.record.plan.required_depth = 'native';
  h.record.plan.destination_state = 'absent';
  h.record.plan.outputs = [{ id: 'hls', format: 'hls-ts', target: 'clear', drms: [], conformance: 'none', processing: 'segments' }];
  h.record.plan.processing = [{ id: 'hls', selection: { requested: 'segments', resolved: 'segments', boundaries: 'preserve', segment_duration_ms: null } }];
  h.view.update();
  const text = h.descendants(h.host).map(node => node.textContent).join(' ');
  assert.ok(text.includes('Segments'));
  assert.ok(text.includes('Original segment boundaries are preserved'));
  h.find('common-convert').dispatch('click');
  assert.deepEqual(JSON.parse(JSON.stringify(h.sent[0].operation)), { kind: 'convert', plan_ref: 'plan_1' });
});
test('an imported v1 plan supports later verification without analyze or reconversion', () => {
  const h = harness(true);
  assert.equal(h.find('common-analyze').disabled, true);
  assert.equal(h.find('common-convert').disabled, true);
  assert.equal(h.find('common-verify').disabled, false);
  h.find('common-verify').dispatch('click');
  assert.equal(h.sent.length, 1); assert.equal(h.sent[0].operation.kind, 'verify'); assert.equal(h.sent[0].operation.plan_ref, 'plan_1');
  assert.ok(h.descendants(h.host).some(node => node.textContent === 'en:commonSingleOutput'));
});
test('named output rows are capped at eight and keep editing values when another row is added', () => {
  const h = harness(), first = h.find('common-output-1-id');
  first.value = 'keep-me'; first.dispatch('input');
  for (let i = 0; i < 12; i++) h.find('common-add-output').dispatch('click');
  assert.equal(h.find('common-add-output').disabled, true);
  assert.equal(h.descendants(h.host).filter(node => node.dataset && node.dataset.outputRow).length, 8);
  assert.equal(first.value, 'keep-me'); assert.equal(h.find('common-output-1-id'), first);
});
test('HLS, DASH and MP4 URLs use one source selection and clear before registration completes', () => {
  for (const file of ['list.m3u8', 'manifest.mpd', 'source.mp4']) {
    const h = harness();
    h.data.selectedId = null; h.view.newInput();
    const kind = h.find('common-source-kind'); kind.value = 'url'; kind.dispatch('change');
    assert.ok(kind.children.some(node => node.value === 'url'));
    const url = h.find('common-url'); url.value = 'https://example.invalid/' + file + '?secret=sentinel'; url.dispatch('input');
    h.find('common-open').dispatch('click');
    assert.equal(h.opened.length, 1); assert.equal(h.opened[0].kind, 'url');
    assert.equal(h.opened[0].url, 'https://example.invalid/' + file + '?secret=sentinel');
    assert.equal(url.value, '');
    assert.equal(h.descendants(h.host).some(node => String(node.textContent).includes('sentinel')), false);
  }
});

test('URL session navigation and ownership use a localized label without any source path', () => {
  const h = harness();
  h.record.session.source = { kind: 'url', label: 'Source URL', display_url: 'https://source.example.invalid/PRIVATE_PATH/master.m3u8' };
  h.data.owner = { sessionId: 'session_1', kind: 'analyze', phase: 'running', stop: 'none', jobId: 'job_1' };
  h.view.update();
  assert.equal(h.descendants(h.host).some(node => String(node.textContent).includes('PRIVATE_PATH')), false);
  assert.match(h.global.children[0].textContent, /en:commonUrl/);
  assert.doesNotMatch(h.global.children[0].textContent, /PRIVATE_PATH/);
  h.setLanguage('ja'); h.view.translate();
  assert.match(h.global.children[0].textContent, /ja:commonUrl/);
});

test('generic local input labels translate in the selected source, navigation and owner status', () => {
  const h = harness(false, true);
  h.record.session.source = { kind: 'local', label: 'Local input', input_ref: 'PRIVATE_REF', display_url: 'https://source.invalid/PRIVATE_SOURCE' };
  h.data.owner = { sessionId: 'session_1', kind: 'analyze', phase: 'running', stop: 'none', jobId: 'job_1' };
  for (const [language, expected] of [['en', 'Local input'], ['ja', 'ローカル入力']]) {
    h.setLanguage(language); h.view.translate();
    assert.equal(h.descendants(h.host).find(node => node.className === 'common-source-label').textContent, expected);
    assert.equal(h.descendants(h.nav).find(node => node.className === 'row-id').textContent, expected);
    assert.ok(h.global.children[0].textContent.includes(expected));
    assert.ok(h.find('common-selected-status').textContent.includes(expected));
    const nodes = [h.host, h.nav, h.global].flatMap(h.descendants);
    assert.equal(nodes.some(node => /PRIVATE_/.test(node.textContent)), false);
    assert.equal(nodes.some(node => [...(node.attrs || [])].some(([, value]) => /PRIVATE_/.test(value))), false);
  }
});

test('local label translation preserves custom labels and the fixed private URL label', () => {
  const h = harness(false, true);
  const sourceLabel = () => h.descendants(h.host).find(node => node.className === 'common-source-label').textContent;
  for (const language of ['en', 'ja']) {
    h.setLanguage(language);
    for (const source of [{ kind: 'local', label: 'custom input.mp4' }, { kind: 'local', label: 'Local input custom' }, { kind: 'saved-plan', label: 'Local input' }]) {
      h.record.session.source = { ...source, input_ref: 'PRIVATE_REF', display_url: 'https://source.invalid/PRIVATE_SOURCE' };
      h.view.translate();
      assert.equal(sourceLabel(), source.label);
    }
    for (const kind of ['url']) {
      h.record.session.source = { kind, label: 'PRIVATE_LABEL', display_url: 'https://source.invalid/PRIVATE_SOURCE' };
      h.view.translate();
      assert.equal(sourceLabel(), language === 'ja' ? '入力 URL' : 'Source URL');
      const nodes = [h.host, h.nav, h.global].flatMap(h.descendants);
      assert.equal(nodes.some(node => /PRIVATE_/.test(node.textContent)), false);
    }
  }
});

test('an unknown Stop response is not labeled as an acknowledged stop', () => {
  const h = harness();
  h.data.owner = { sessionId: 'session_1', kind: 'convert', phase: 'running', stop: 'unknown', jobId: 'job_1' };
  h.view.update();
  assert.match(h.global.children[0].textContent, /stopOutcomeUnknown/);
  const stop = h.descendants(h.global).find(node => node.getAttribute && node.getAttribute('id') === 'common-global-stop');
  assert.equal(stop.hidden, true, 'only the GET status action remains');
});

test('selected status binds the selected last job, including a negative verify outcome', () => {
  const h = harness(true), planId = 'c'.repeat(64);
  h.record.lastJob = { job_id: 'job_negative', request_id: 'request_negative', operation: 'verify', plan_id: planId, plan_ref: 'plan_actual', state: 'failed' };
  h.record.outcome = 'failed'; h.view.update();
  const status = h.find('common-selected-status'); assert.ok(status);
  const expected = { outcome: 'failed', 'job-id': 'job_negative', 'request-id': 'request_negative', operation: 'verify', 'plan-id': planId, 'plan-ref': 'plan_actual', 'job-state': 'failed' };
  Object.entries(expected).forEach(([key, value]) => assert.equal(status.getAttribute('data-common-' + key), value, key));
  assert.match(status.textContent, /commonFailed/);
  assert.equal(status.getAttribute('role'), 'status'); assert.equal(status.getAttribute('aria-live'), 'polite');
  h.data.owner = { sessionId: 'another_session', kind: 'convert', phase: 'running', jobId: 'another_job', requestId: 'another_request' };
  h.setLanguage('ja'); h.view.translate();
  assert.equal(h.find('common-selected-status'), status);
  Object.entries(expected).forEach(([key, value]) => assert.equal(status.getAttribute('data-common-' + key), value, key));
  h.record.lastJob = null; h.record.outcome = null; h.view.update();
  Object.keys(expected).forEach(key => assert.equal(status.getAttribute('data-common-' + key), null, key));
});

test('report selection, removal and no-selection states clear stale envelope bindings', () => {
  const h = harness();
  const envelope = { job_id: 'job_analysis', request_id: 'request_analysis', operation: 'analyze', report_kind: 'analysis', plan_id: null, plan_ref: null, report: { input_admitted: true, eligible_formats: [] } };
  h.record.reports.set('analyze', envelope); h.view.update();
  const report = h.find('common-analysis-report'); assert.ok(report);
  assert.equal(report.getAttribute('data-common-job-id'), 'job_analysis');
  h.record.reports.delete('analyze'); h.view.update();
  assert.equal(report.getAttribute('data-common-job-id'), null);
  h.record.reports.set('analyze', envelope); h.view.update();
  assert.equal(report.getAttribute('data-common-job-id'), 'job_analysis');
  h.record.lastJob = { job_id: 'job_analysis', request_id: 'request_analysis', operation: 'analyze', state: 'succeeded', plan_id: null, plan_ref: null };
  h.record.outcome = 'succeeded'; h.view.update();
  const second = { ...h.record, session: { ...h.record.session, session_id: 'session_2' }, reports: new Map([['analyze', { ...envelope, job_id: 'job_second', request_id: 'request_second', report: { ...envelope.report, input_admitted: false } }]]), lastJob: { ...h.record.lastJob, job_id: 'job_second', request_id: 'request_second', state: 'failed' }, outcome: 'failed' };
  h.data.sessions.set('session_2', second); h.data.selectedId = 'session_2'; h.view.update();
  assert.equal(report.getAttribute('data-common-job-id'), 'job_second');
  assert.equal(h.find('common-selected-status').getAttribute('data-common-job-id'), 'job_second');
  assert.equal(h.find('common-selected-status').getAttribute('data-common-outcome'), 'failed');
  assert.equal(h.descendants(report).find(node => node.getAttribute?.('data-common-check') === 'input_admitted').getAttribute('data-common-state'), 'fail');
  h.data.selectedId = null; h.view.update();
  assert.equal(h.find('common-analysis-report'), report);
  for (const id of ['common-analysis-report', 'common-conversion-report', 'common-verification-report', 'common-selected-status']) {
    const node = h.find(id); assert.ok(node, id);
    for (const attr of ['job-id', 'request-id', 'operation', 'report-kind', 'plan-id', 'plan-ref', 'outcome', 'job-state']) assert.equal(node.getAttribute('data-common-' + attr), null, id + ':' + attr);
  }
  assert.equal(h.descendants(report).some(node => node.getAttribute?.('data-common-check')), false);
});

test('a report for a formerly selected plan cannot retain current-report bindings', () => {
  const h = harness(true);
  h.record.reports.set('verify', { job_id: 'job_verify', request_id: 'request_verify', operation: 'verify', report_kind: 'verification', plan_id: h.record.plan.plan_id, plan_ref: h.record.plan.plan_ref, report: { source_unchanged: false, output_matches_plan: false, verification: { media_verified: false, license_playback_verified: false } } });
  h.view.update();
  const report = h.find('common-verification-report'); assert.ok(report);
  assert.equal(report.getAttribute('data-common-job-id'), 'job_verify');
  h.record.plan = null; h.view.update();
  for (const attr of ['job-id', 'request-id', 'operation', 'report-kind', 'plan-id', 'plan-ref']) assert.equal(report.getAttribute('data-common-' + attr), null, attr);
  assert.equal(h.descendants(report).some(node => node.getAttribute?.('data-common-check')), false);
});


test('DASH addressing is a per-output choice with a nominal default and no layout selector', () => {
  const h = harness(false, true), format = h.find('common-output-1-format');
  const select = h.find('common-output-1-dash-addressing');
  assert.ok(select); assert.equal(select.value, 'template');
  assert.deepEqual(select.children.map(option => option.value), ['template', 'template-timeline', 'list', 'base']);
  assert.equal(select.hidden, true); assert.equal(select.disabled, true);
  format.value = 'dash'; format.dispatch('change');
  assert.equal(select.hidden, false); assert.equal(select.disabled, false);
  assert.ok(select.getAttribute('aria-describedby'));
  select.value = 'base'; select.focus(); select.dispatch('change');
  h.view.update(); h.setLanguage('ja'); h.view.translate();
  assert.equal(h.find('common-output-1-dash-addressing'), select);
  assert.equal(select.value, 'base'); assert.equal(h.document.activeElement, select);
  assert.ok(select.getAttribute('aria-label').includes('1'));
  assert.ok(select.children.some(option => option.textContent === '索引付き MP4 ファイル'));
  h.record.plan = { plan_ref: 'saved-choice' };
  select.value = 'list'; select.dispatch('change');
  assert.equal(h.record.plan, null);
  format.value = 'hls'; format.dispatch('change');
  assert.equal(select.hidden, true); assert.equal(select.disabled, true);
  format.value = 'dash'; format.dispatch('change');
  assert.equal(select.value, 'list');
  assert.equal(h.descendants(h.host).some(node => node.dataset?.commonField === 'layout'), false);
});

test('saved source layout and DASH addressing are read-only and never sent as execution overrides', () => {
  for (const schema of [7, 8, 9, 10]) {
    const h = harness(true, true), single = schema === 7 || schema === 9;
    const selection = { requested: 'stitch', resolved: 'stitch', boundaries: 'resegment', segment_duration_ms: 6000 };
    const options = { layout: 'source', dash_addressing: 'list' };
    Object.assign(h.record.plan, { plan_schema_version: schema, required_depth: 'native', destination_state: 'absent',
      outputs: [{ id: single ? null : 'dash', format: 'dash', target: 'cbcs', drms: ['widevine', 'playready'], conformance: 'none', processing: 'stitch', dash_addressing: 'list' }],
      processing: single ? selection : [{ id: 'dash', selection }],
      output_options: single ? options : [{ id: 'dash', options }] });
    h.view.update();
    let text = h.descendants(h.host).map(node => node.textContent).join(' ');
    assert.ok(text.includes('Source directories')); assert.ok(text.includes('Explicit segment list'));
    assert.ok(h.descendants(h.host).some(node => node.getAttribute?.('data-common-plan-processing') === 'stitch'));
    h.find('common-convert').dispatch('click');
    assert.deepEqual(JSON.parse(JSON.stringify(h.sent[0].operation)), { kind: 'convert', plan_ref: 'plan_1' });
    h.record.plan.destination_state = 'present'; h.setLanguage('ja'); h.view.translate();
    text = h.descendants(h.host).map(node => node.textContent).join(' ');
    assert.ok(text.includes('元のディレクトリ')); assert.ok(text.includes('セグメントの一覧'));
    h.find('common-verify').dispatch('click');
    assert.deepEqual(JSON.parse(JSON.stringify(h.sent[1].operation)), { kind: 'verify', plan_ref: 'plan_1' });
  }
});
