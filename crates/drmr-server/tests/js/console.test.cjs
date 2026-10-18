'use strict';

const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const test = require('node:test');
const vm = require('node:vm');

const ids = ['constructor', 'toString', '__proto__', 'hasOwnProperty'];
const flush = () => new Promise(setImmediate);

class Element {
  constructor(tagName) {
    this.tagName = tagName;
    this.nodeType = 1;
    this.children = [];
    this.dataset = {};
    this.attributes = new Map();
    this.listeners = new Map();
    this.checked = false;
    this.value = '';
    this.textContent = '';
  }
  appendChild(node) { node.parentNode = this; this.children.push(node); return node; }
  replaceChildren() { this.children = []; }
  setAttribute(name, value) { this.attributes.set(name, value); }
  getAttribute(name) { return this.attributes.get(name) || null; }
  removeAttribute(name) { this.attributes.delete(name); }
  addEventListener(name, handler) { this.listeners.set(name, handler); }
  dispatch(name) { this.listeners.get(name)?.({ target: this }); }
}

function descendants(node) {
  return [node, ...node.children.flatMap(descendants)];
}

function harness(options = {}) {
  const elements = new Map();
  const element = id => {
    if (!elements.has(id)) elements.set(id, new Element('div'));
    return elements.get(id);
  };
  const catalog = options.catalog || ids.map(asset_id => ({ asset_id, label: asset_id }));
  const queued = [];
  const sandbox = {
    window: { addEventListener() {}, App: {
      I18n: { t: (key, values) => key + (values?.n == null ? '' : ':' + values.n), has: () => false, getLanguage: () => 'en', error: () => 'fixed-error' },
      Api: {
        postQueue: assetIds => { queued.push(Array.from(assetIds)); return Promise.resolve(); },
        getCatalog: options.getCatalog || (() => Promise.resolve(catalog)),
        getHlsJobs: () => Promise.resolve({ jobs: options.jobs || [] }),
        postConvert: options.postConvert || (() => Promise.reject({ status: 409 })),
        getAssetUrls: options.getAssetUrls || (() => Promise.resolve({})),
        getCatalogReport: options.getCatalogReport || (() => Promise.reject({ status: 404 })),
        revokePlaybackSession: () => Promise.resolve(),
        stopJob: options.stopJob || (() => Promise.resolve({ stop_requested: true })),
        getQueue: () => Promise.resolve({ running: null, pending: [], recent: [] }),
      },
    } },
    document: { getElementById: element, createElement: tag => new Element(tag),
      createTextNode: text => ({ nodeType: 3, textContent: text, children: [] }),
      addEventListener() {}, activeElement: null },
    URL, location: { origin: 'http://127.0.0.1' }, console, setTimeout: () => 0, clearTimeout() {}, setInterval: () => 0, clearInterval() {},
  };
  if (options.language) {
    sandbox.localStorage = { getItem: () => options.language };
    vm.runInNewContext(fs.readFileSync(path.join(__dirname, '../../static/js/i18n.js'), 'utf8'), sandbox);
  }
  for (const file of ['ui.js', 'model.js', 'console.js']) {
    let source = fs.readFileSync(path.join(__dirname, '../../static/js', file), 'utf8');
    if (file === 'console.js') {
      // Expose closure state only in this VM; the shipped controller has no testing API.
      const boot = "  document.addEventListener('DOMContentLoaded', boot);";
      assert.ok(source.includes(boot));
      source = source.replace(boot, `window.controller = {
        state, cacheElements, bind, renderList, newLogView, applyRecord, settleProgress, finishRun,
        renderLogMeta, suggestedName, discoverRunning, renderRuns, requestStop,
        renderPlaybackControls, preparePreview, recompose, renderReport, loadCatalogReport, startConvert, renderInput, renderHead,
        onPlayback: function (value) { playback = value; },
        onRestore: function (callback) { restoreRun = callback; }
      };`);
    }
    vm.runInNewContext(source, sandbox, { filename: file });
  }
  const controller = sandbox.window.controller;
  controller.cacheElements();
  controller.bind();
  controller.state.catalog = catalog;
  controller.state.contents = sandbox.window.App.Model.compose(catalog, [], null);
  controller.renderList();
  const checkbox = id => descendants(element('content-list')).find(node =>
    node.tagName === 'input' && node.getAttribute('aria-label') === id);
  const toggle = (id, checked) => { const input = checkbox(id); input.checked = checked; input.dispatch('change'); };
  const unchecked = () => {
    for (const id of ids) assert.equal(checkbox(id).checked, false, id + ' starts unchecked');
    assert.equal(element('bulk-bar').hidden, true);
  };
  return { controller, element, checkbox, toggle, unchecked, queued, i18n: sandbox.window.App.I18n };
}

test('special catalog IDs render unchecked and toggle independently', () => {
  const h = harness();
  h.unchecked();
  for (const id of ids) h.toggle(id, true);
  h.controller.renderList();
  for (const id of ids) assert.equal(h.checkbox(id).checked, true, id);
  assert.equal(h.element('bulk-count').textContent, 'bulkSelected:4');
  h.toggle('__proto__', false);
  h.controller.renderList();
  assert.equal(h.checkbox('__proto__').checked, false);
  assert.equal(h.checkbox('constructor').checked, true);
  assert.equal(h.element('bulk-count').textContent, 'bulkSelected:3');
});

test('clear selection resets all IDs and allows selecting them again', () => {
  const h = harness();
  for (const id of ids) h.toggle(id, true);
  h.element('bulk-clear-btn').dispatch('click');
  h.unchecked();
  h.toggle('__proto__', true);
  assert.deepEqual(Object.keys(h.controller.state.checked), ['__proto__']);
  assert.equal(h.element('bulk-count').textContent, 'bulkSelected:1');
});

test('bulk enqueue sends every selected ID and its reset supports another selection', async () => {
  const h = harness();
  for (const id of ids) h.toggle(id, true);
  h.element('bulk-queue-btn').dispatch('click');
  await flush();
  assert.deepEqual(h.queued, [ids]);
  h.unchecked();
  h.toggle('__proto__', true);
  h.element('bulk-queue-btn').dispatch('click');
  await flush();
  assert.deepEqual(h.queued, [ids, ['__proto__']]);
});

test('progress retains special rendition and track IDs through settlement and rendering', () => {
  const h = harness();
  const view = h.controller.newLogView('job', 'asset', 'convert', false);
  for (const id of ids) {
    h.controller.applyRecord(view, { stage: 'merge', rendition_id: id });
    h.controller.applyRecord(view, { stage: 'hls-source', track_id: id, index: 1, total: 2 });
  }
  h.controller.settleProgress(view, false);
  h.controller.renderLogMeta(view);
  const strips = descendants(h.element('log-strips')).filter(node => node.className === 'strip');
  assert.deepEqual(strips.map(node => node.dataset.trackId), [...ids, ...ids]);
  assert.ok(strips.every(node => node.dataset.status === 'failed'));
});

test('suggested names check only existing asset IDs', () => {
  const h = harness();
  h.controller.state.contents = [];
  for (const id of ['constructor', 'toString', 'hasOwnProperty']) {
    const url = 'https://media.example/' + id + '/master.m3u8';
    assert.equal(h.controller.suggestedName(url), id);
    h.controller.state.contents = [{ asset_id: id }];
    assert.equal(h.controller.suggestedName(url), id + '-2');
    h.controller.state.contents = [];
  }
});

test('running persisted job IDs are adopted until explicitly marked finished', () => {
  const h = harness();
  const adopted = [];
  h.controller.onRestore(job => adopted.push(job.job_id));
  for (const id of ['constructor', 'toString', 'hasOwnProperty']) {
    h.controller.state.jobs = [{ job_id: id, state: 'running' }];
    h.controller.discoverRunning();
    h.controller.state.finishedJobs[id] = true;
    h.controller.discoverRunning();
  }
  assert.deepEqual(adopted, ['constructor', 'toString', 'hasOwnProperty']);
});


test('a stop request targets one exact job, remains idempotent and does not invent completion', async () => {
  let accept;
  const calls = [];
  const job = { job_id: '__proto__', asset_id: 'asset', kind: 'convert', state: 'running' };
  const h = harness({ jobs: [job], stopJob: id => { calls.push(id); return new Promise(resolve => { accept = resolve; }); } });
  const run = { jobId: job.job_id, assetId: job.asset_id, kind: job.kind, startedAt: Date.now(), view: h.controller.newLogView(job.job_id, job.asset_id, job.kind, true) };
  h.controller.state.run = run;
  const stopping = h.controller.requestStop(job);
  await h.controller.requestStop(job);
  assert.deepEqual(calls, [job.job_id]);
  accept({ state: 'running', stop_requested: true });
  await stopping;
  assert.equal(h.controller.state.run, run);
  assert.equal(run.view.live, true);
  assert.equal(run.view.outcome, null);
  assert.equal(h.controller.state.stopRequests[job.job_id], 'requested');
  h.controller.renderRuns({ jobs: [job] });
  const button = descendants(h.element('runs-list')).find(node => node.dataset?.focusKey === 'stop:' + job.job_id);
  assert.equal(button.textContent, 'stopPending');
  assert.equal(button.getAttribute('aria-disabled'), 'true');
  await h.controller.requestStop({ ...job, state: 'interrupted' });
  assert.deepEqual(calls, [job.job_id]);
});

test('a rejected stop keeps the live run and permits a deliberate retry', async () => {
  const calls = [];
  const job = { job_id: 'job', asset_id: 'asset', kind: 'analysis', state: 'running' };
  const h = harness({ stopJob: id => { calls.push(id); return Promise.reject({ status: 503, body: { error: 'cleanup-unconfirmed', message: 'private' } }); } });
  const run = { jobId: job.job_id };
  h.controller.state.run = run;
  await h.controller.requestStop(job);
  assert.equal(h.controller.state.run, run);
  assert.equal(h.controller.state.stopRequests[job.job_id], undefined);
  await h.controller.requestStop(job);
  assert.deepEqual(calls, ['job', 'job']);
});


test('unverified catalog output cannot prepare comparison playback', async () => {
  let requested = 0;
  const h = harness({ catalog: [{ asset_id: 'asset', converted: true }], getAssetUrls: () => { requested += 1; return Promise.resolve({}); } });
  const c = h.controller.state.contents[0];
  h.controller.state.selectedId = 'asset';
  h.controller.renderPlaybackControls(c);
  assert.equal(h.element('preview-btn').disabled, true);
  h.controller.preparePreview(false);
  await flush();
  assert.equal(requested, 0);
});

test('fresh asset authority must still permit preview after an older verified listing', async () => {
  let prepared = 0;
  const h = harness({ catalog: [{ asset_id: 'asset', converted: true, output_readiness: { state: 'verified', job_id: 'old' } }],
    getAssetUrls: () => Promise.resolve({ new_url: null, output_readiness: { state: 'unverified' } }) });
  h.controller.state.selectedId = 'asset';
  h.controller.onPlayback({ hide() {}, prepareCatalog() { prepared += 1; } });
  h.controller.preparePreview(false);
  await flush();
  assert.equal(prepared, 0);
  assert.equal(h.element('preview-btn').disabled, true);
  assert.equal(h.controller.state.contents[0].state, 'unverified');
});

test('a new catalog generation clears previously displayed success and playback', () => {
  let hidden = 0;
  const h = harness({ catalog: [{ asset_id: 'asset', converted: true, output_readiness: { state: 'verified', job_id: 'old' } }] });
  h.controller.state.selectedId = 'asset';
  h.controller.state.detail.report = { all_pass: true };
  h.controller.state.detail.catalogInfo = { new_url: '/output/asset/old.mpd' };
  h.controller.onPlayback({ hide() { hidden += 1; } });
  h.controller.state.catalog = [{ asset_id: 'asset', converted: true, output_readiness: { state: 'unverified', job_id: 'new' } }];
  h.controller.recompose();
  assert.equal(h.controller.state.detail.report, null);
  assert.equal(h.controller.state.detail.catalogInfo, null);
  assert.equal(hidden, 1);
  assert.equal(h.element('preview-btn').disabled, true);
});


test('verified catalog comparison still requires and uses fresh qualified asset URLs', async () => {
  let prepared = 0;
  const info = { new_url: '/output/asset/new.mpd', output_readiness: { state: 'verified', job_id: 'current' } };
  const h = harness({ catalog: [{ asset_id: 'asset', converted: true, output_readiness: info.output_readiness }], getAssetUrls: () => Promise.resolve(info) });
  h.controller.state.selectedId = 'asset';
  h.controller.onPlayback({ hide() {}, prepareCatalog(id, value) { assert.equal(id, 'asset'); assert.equal(value, info); prepared += 1; } });
  h.controller.renderPlaybackControls(h.controller.state.contents[0]);
  assert.equal(h.element('preview-btn').disabled, false);
  h.controller.preparePreview(false);
  await flush();
  assert.equal(prepared, 1);
  assert.equal(h.element('preview-btn').disabled, false);
  assert.ok(descendants(h.element('preview-state')).some(node => node.textContent === 'previewReady'));
});

test('an old report response cannot restore success after a new generation is observed', async () => {
  let deliver;
  let requests = 0;
  const h = harness({ catalog: [{ asset_id: 'asset', converted: true, output_readiness: { state: 'verified', job_id: 'old' } }],
    getCatalogReport: () => ++requests === 1 ? new Promise(resolve => { deliver = resolve; }) : Promise.reject({ status: 410 }) });
  h.controller.state.selectedId = 'asset';
  h.controller.onPlayback({ hide() {} });
  h.controller.loadCatalogReport(h.controller.state.contents[0], h.controller.state.selectToken);
  h.controller.state.catalog = [{ asset_id: 'asset', converted: true, output_readiness: { state: 'unverified', job_id: 'new' } }];
  h.controller.recompose();
  deliver({ all_pass: true, output_readiness: { state: 'verified', job_id: 'old' } });
  await flush();
  assert.equal(h.controller.state.detail.report, null);
  assert.equal(h.element('preview-btn').disabled, true);
});


test('a refused launch restores an untouched generation only from refreshed server authority', async () => {
  const row = () => ({ asset_id: 'asset', converted: true, output_readiness: { state: 'verified', job_id: 'old' } });
  const h = harness({ catalog: [row()], getCatalog: () => Promise.resolve([row()]) });
  h.controller.state.selectedId = 'asset';
  h.controller.onPlayback({ hide() {} });
  h.controller.startConvert(h.controller.state.contents[0]);
  assert.equal(h.element('preview-btn').disabled, true);
  await flush();
  assert.equal(h.controller.state.contents[0].state, 'packaged');
  assert.equal(h.element('preview-btn').disabled, false);
});


test('a retired preview cannot leave the final new generation disabled without another poll', async () => {
  let deliverOld;
  let requests = 0;
  const info = id => ({ new_url: '/output/asset/output.mpd', output_readiness: { state: 'verified', job_id: id } });
  const h = harness({ catalog: [{ asset_id: 'asset', converted: true, output_readiness: info('old').output_readiness }],
    getAssetUrls: () => ++requests === 1 ? new Promise(resolve => { deliverOld = resolve; }) : Promise.resolve(info('new')) });
  h.controller.state.selectedId = 'asset';
  h.controller.onPlayback({ hide() {}, prepareCatalog() { assert.fail('the retired preview cannot install its old URLs'); } });
  h.controller.preparePreview(false);
  h.controller.state.catalog = [{ asset_id: 'asset', converted: true, output_readiness: info('new').output_readiness }];
  h.controller.recompose();
  assert.equal(h.element('preview-btn').disabled, false, 'the final generation does not wait on a retired request');
  deliverOld(info('old'));
  await flush();
  assert.equal(h.controller.state.previewBusy, false);
  assert.equal(h.element('preview-btn').disabled, false);
});

test('an old preview completion cannot release a newer active preview request', async () => {
  let deliverOld, deliverNew;
  let requests = 0, prepared = 0;
  const info = id => ({ new_url: '/output/asset/output.mpd', output_readiness: { state: 'verified', job_id: id } });
  const h = harness({ catalog: [{ asset_id: 'asset', converted: true, output_readiness: info('old').output_readiness }],
    getAssetUrls: () => {
      requests += 1;
      if (requests === 1) return new Promise(resolve => { deliverOld = resolve; });
      if (requests === 3) return new Promise(resolve => { deliverNew = resolve; });
      return Promise.resolve(info('new'));
    } });
  h.controller.state.selectedId = 'asset';
  h.controller.onPlayback({ hide() {}, prepareCatalog(id, value) { assert.equal(value.output_readiness.job_id, 'new'); prepared += 1; } });
  h.controller.preparePreview(false);
  h.controller.state.catalog = [{ asset_id: 'asset', converted: true, output_readiness: info('new').output_readiness }];
  h.controller.recompose();
  await flush();
  h.controller.preparePreview(false);
  deliverOld(info('old'));
  await flush();
  assert.equal(h.controller.state.previewBusy, true);
  assert.equal(h.element('preview-btn').disabled, true);
  assert.equal(prepared, 0);
  deliverNew(info('new'));
  await flush();
  assert.equal(prepared, 1);
  assert.equal(h.controller.state.previewBusy, false);
  assert.equal(h.element('preview-btn').disabled, false);
});


test('stopped analysis uses interrupted list/detail chips and input recovery copy', () => {
  const h = harness({ catalog: [] });
  h.controller.state.jobs = [{ asset_id: 'asset', kind: 'analysis', job_id: 'stopped', state: 'interrupted' }];
  h.controller.recompose();
  const c = h.controller.state.contents[0];
  h.controller.renderHead(c);
  h.controller.renderInput(c);
  for (const id of ['content-list', 'content-chips']) {
    const chip = descendants(h.element(id)).find(node => node.textContent === 'cstate_interrupted');
    assert.ok(chip, id);
    assert.ok(chip.className.includes('chip-warn'));
    assert.equal(descendants(h.element(id)).some(node => node.textContent === 'cstate_failed'), false);
  }
  assert.ok(descendants(h.element('input-state')).some(node => node.textContent === 'finding_analysis-interrupted'));
  assert.equal(descendants(h.element('input-state')).some(node => node.textContent === 'analysisJobFailed'), false);
});


test('a newly running analysis replaces the previous asset failure notice', () => {
  const h = harness({ catalog: [] });
  h.controller.state.jobs = [{ asset_id: 'old', kind: 'analysis', job_id: 'failed', state: 'failed' }];
  h.controller.recompose();
  h.controller.renderInput(h.controller.state.contents[0]);
  assert.ok(descendants(h.element('input-state')).some(node => node.textContent === 'analysisJobFailed'));
  h.controller.state.jobs = [{ asset_id: 'new', kind: 'analysis', job_id: 'active', state: 'running' }];
  h.controller.recompose();
  h.controller.state.run = { kind: 'analysis', assetId: 'new' };
  h.controller.renderInput(h.controller.state.contents[0]);
  assert.ok(descendants(h.element('input-state')).some(node => node.textContent === 'analyzing'));
  assert.equal(descendants(h.element('input-state')).some(node => node.textContent === 'analysisJobFailed'), false);
  assert.equal(descendants(h.element('input-state')).some(node => node.textContent === 'finding_analysis-interrupted'), false);
});


for (const language of ['en', 'ja']) {
  test(language + ': interrupted catalog conversion warns without rewriting its exit or completed work', async () => {
    const h = harness({ language, catalog: [{ asset_id: 'asset', label: 'Asset' }] });
    h.controller.state.selectedId = 'asset';
    const view = h.controller.newLogView('job', 'asset', 'convert', true);
    h.controller.applyRecord(view, { stage: 'merge', rendition_id: 'pending' });
    h.controller.applyRecord(view, { stage: 'report', rendition_id: 'complete' });
    h.controller.applyRecord(view, { stage: 'encrypt', rendition_id: 'failed', level: 'error' });
    h.controller.applyRecord(view, { stage: 'hls-source', track_id: 'audio', index: 1, total: 5 });
    const run = { jobId: 'job', assetId: 'asset', kind: 'convert', startedAt: Date.now(), view };
    h.controller.state.run = run;
    h.controller.finishRun(run, { state: 'interrupted', exit_code: -9 });
    await flush();
    const notice = h.element('content-notice').children[0];
    assert.match(notice.className, /notice-warn/);
    assert.equal(descendants(notice).map(node => node.textContent).join(''), h.i18n.t('convertInterrupted', { elapsed: h.i18n.t('unitSec', { s: Math.round(run.elapsedMs / 1000) }) }));
    assert.equal(run.exitCode, -9);
    assert.equal(view.outcome, 'interrupted');
    assert.equal(view.progress.renditions.pending, 'interrupted');
    assert.equal(view.progress.renditions.complete, 'done');
    assert.equal(view.progress.renditions.failed, 'failed');
    assert.equal(view.progress.tracks.audio.status, 'interrupted');
    h.controller.renderLogMeta(view);
    const rows = descendants(h.element('log-strips')).filter(node => node.className === 'strip');
    for (const id of ['pending', 'audio']) {
      const row = rows.find(node => node.dataset.trackId === id);
      assert.equal(row.dataset.status, 'interrupted');
      assert.ok(descendants(row).some(node => node.textContent.includes(h.i18n.t('strip_interrupted'))));
    }
  });
}

test('terminal failures remain failures and lost logs remain unknown', async () => {
  const h = harness({ catalog: [{ asset_id: 'asset' }] });
  h.controller.state.selectedId = 'asset';
  const view = h.controller.newLogView('job', 'asset', 'convert', true);
  h.controller.applyRecord(view, { stage: 'merge', rendition_id: 'pending' });
  const run = { jobId: 'job', assetId: 'asset', kind: 'convert', startedAt: Date.now(), view };
  h.controller.state.run = run;
  h.controller.finishRun(run, { state: 'failed', exit_code: 7 });
  await flush();
  assert.match(h.element('content-notice').children[0].className, /notice-fail/);
  assert.equal(descendants(h.element('content-notice')).map(node => node.textContent).join(''), 'convertFailed');
  assert.equal(run.exitCode, 7);
  assert.equal(view.progress.renditions.pending, 'failed');
  const lost = h.controller.newLogView('lost', 'asset', 'convert', false);
  lost.interrupted = true;
  lost.outcome = 'failed';
  h.controller.applyRecord(lost, { stage: 'merge', rendition_id: 'pending' });
  h.controller.settleProgress(lost, false);
  assert.equal(lost.progress.renditions.pending, 'unknown');
});

for (const kind of ['analysis', 'package']) {
  test('interrupted ' + kind + ' uses a warning rather than a failure notice', async () => {
    const h = harness({ catalog: [{ asset_id: 'asset' }] });
    h.controller.state.selectedId = 'asset';
    const view = h.controller.newLogView('job', 'asset', kind, true);
    const run = { jobId: 'job', assetId: 'asset', kind, startedAt: Date.now(), view };
    h.controller.state.run = run;
    h.controller.finishRun(run, { state: 'interrupted', exit_code: -9 });
    await flush();
    const notice = h.element('content-notice').children[0];
    assert.match(notice.className, /notice-warn/);
    assert.match(descendants(notice).map(node => node.textContent).join(''), kind === 'analysis' ? /finding_analysis-interrupted/ : /packageInterrupted/);
    assert.equal(run.exitCode, -9);
  });
}
