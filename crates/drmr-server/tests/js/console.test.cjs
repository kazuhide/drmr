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
  const catalog = ids.map(asset_id => ({ asset_id, label: asset_id }));
  const queued = [];
  const sandbox = {
    window: { addEventListener() {}, App: {
      I18n: { t: (key, values) => key + (values?.n == null ? '' : ':' + values.n), has: () => false, getLanguage: () => 'en', error: () => 'fixed-error' },
      Api: {
        postQueue: assetIds => { queued.push(Array.from(assetIds)); return Promise.resolve(); },
        getCatalog: () => Promise.resolve(catalog),
        getHlsJobs: () => Promise.resolve({ jobs: options.jobs || [] }),
        stopJob: options.stopJob || (() => Promise.resolve({ stop_requested: true })),
        getQueue: () => Promise.resolve({ running: null, pending: [], recent: [] }),
      },
    } },
    document: { getElementById: element, createElement: tag => new Element(tag),
      createTextNode: text => ({ nodeType: 3, textContent: text, children: [] }),
      addEventListener() {}, activeElement: null },
    URL, console, setTimeout: () => 0, clearTimeout() {},
  };
  for (const file of ['ui.js', 'model.js', 'console.js']) {
    let source = fs.readFileSync(path.join(__dirname, '../../static/js', file), 'utf8');
    if (file === 'console.js') {
      // Expose closure state only in this VM; the shipped controller has no testing API.
      const boot = "  document.addEventListener('DOMContentLoaded', boot);";
      assert.ok(source.includes(boot));
      source = source.replace(boot, `window.controller = {
        state, cacheElements, bind, renderList, newLogView, applyRecord, settleProgress,
        renderLogMeta, suggestedName, discoverRunning, renderRuns, requestStop,
        onRestore: function (callback) { restoreRun = callback; }
      };`);
    }
    vm.runInNewContext(source, sandbox, { filename: file });
  }
  const controller = sandbox.window.controller;
  controller.cacheElements();
  controller.bind();
  controller.state.contents = sandbox.window.App.Model.compose(catalog, [], null);
  controller.renderList();
  const checkbox = id => descendants(element('content-list')).find(node =>
    node.tagName === 'input' && node.getAttribute('aria-label') === id);
  const toggle = (id, checked) => { const input = checkbox(id); input.checked = checked; input.dispatch('change'); };
  const unchecked = () => {
    for (const id of ids) assert.equal(checkbox(id).checked, false, id + ' starts unchecked');
    assert.equal(element('bulk-bar').hidden, true);
  };
  return { controller, element, checkbox, toggle, unchecked, queued };
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
