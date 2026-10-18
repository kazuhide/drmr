/* Browser console controller for content, jobs and local playback. */
(function () {
  'use strict';
  var App = window.App;
  var UI = App.UI, h = UI.h, Model = App.Model;
  function t(key, values) { return App.I18n.t(key, values); }

  var DRM_FIELDS = { kid: 'drm-kid', key: 'drm-key', iv: 'drm-iv', widevine_pssh: 'drm-widevine-pssh', playready_la_url: 'drm-playready-la-url', fairplay_uri: 'drm-fairplay-uri' };
  var CATALOG_STAGE_KEYS = { merge: 'stageMerge', decrypt: 'stageDecrypt', encrypt: 'stageEncrypt', report: 'stageVerify', manifest: 'stageManifest', raw: 'stageWorking' };
  var RENDITION_WORK_STAGES = { merge: true, decrypt: true, encrypt: true };
  var SOURCE_PROGRESS_KEYS = { 'analysis-started': 'srcAnalysisStarted', 'playlist-started': 'srcPlaylistStarted', 'segment-downloaded': 'srcSegmentDownloaded', 'probe-started': 'srcProbeStarted', 'segment-probed': 'srcSegmentProbed', 'playlist-complete': 'srcPlaylistComplete', 'analysis-complete': 'srcAnalysisComplete' };
  var POLL_MS = 3000;
  var MAX_JOB_PAGES = 10;

  var state = {
    catalog: [], jobs: [], queue: { running: null, pending: [], recent: [] }, contents: [],
    filter: '', checked: Object.create(null), listError: null, loaded: false, dismissedSkips: {}, seenSkips: {}, announcedSkips: {}, jobsTruncated: false, loadSeq: 0, loadAgain: false, loadAgainFull: false, finishedJobs: Object.create(null), logRaw: false, nameEdited: false, sampleRevealed: false,
    selectedId: null, selectToken: 0, openInfo: {}, // openInfo: the transformation's (i) panels open for this selection
    detail: { catalogInfo: null, analysis: null, analysisError: null, result: null, resultError: null, report: null, reportError: null },
    run: null, lastRun: null, logView: null, stopRequests: Object.create(null),
    sessionId: null, previewBusy: false, previewNeedsCredentials: false,
    addOpen: false, platform: null, pollTimer: null, loading: null,
  };
  var els = {};
  var playback = null;
  var elapsedTimer = null;

  function byId(id) { return document.getElementById(id); }
  function isJobRunning() { return state.run !== null; }
  function selected() { return state.contents.filter(function (c) { return c.asset_id === state.selectedId; })[0] || null; }
  function newLogView(jobId, assetId, kind, live) {
    return { jobId: jobId, assetId: assetId, kind: kind, live: live, lines: [], watcher: null, interrupted: false, outcome: null, notice: '',
      progress: { lastRec: null, renditions: Object.create(null), tracks: Object.create(null) } };
  }

  // ========================================================================================
  // Loading the listings and composing the contents
  // ========================================================================================
  function loadAllJobs() {
    var rows = [];
    function page(cursor, n) {
      return App.Api.getHlsJobs(cursor).then(function (data) {
        rows = rows.concat(data.jobs || []);
        if (data.next_cursor && n < MAX_JOB_PAGES) return page(data.next_cursor, n + 1);
        return { rows: rows, truncated: !!data.next_cursor }; // the cap is never silent
      });
    }
    return page(null, 1);
  }
  // light: jobs + queue only (a poll never rescans the catalog); full: the catalog too.
  function fetchListings(light) {
    var seq = ++state.loadSeq;
    return Promise.all([light ? Promise.resolve(null) : App.Api.getCatalog(), loadAllJobs(), App.Api.getQueue()]).then(function (parts) {
      if (seq !== state.loadSeq) return; // a newer fetch is the truth
      if (parts[0]) state.catalog = parts[0];
      var known = {};
      state.jobs.forEach(function (job) { known[job.job_id] = job.state; });
      state.jobs = parts[1].rows || [];
      state.jobs.forEach(function (job) { if (Model.isTerminal(job.state)) delete state.stopRequests[job.job_id]; });
      // A conversion that started and finished between two light polls changes `converted`
      // without this page observing it: fetch the catalog again before settling.
      if (light && state.jobs.some(function (job) { return job.kind === 'convert' && Model.isTerminal(job.state) && known[job.job_id] !== job.state; })) { state.loadAgain = true; state.loadAgainFull = true; }
      state.jobsTruncated = !!parts[1].truncated;
      state.queue = parts[2] || { running: null, pending: [], recent: [] };
      state.listError = null;
      state.loaded = true;
      recompose();
      discoverRunning();
    }, function (e) {
      if (seq !== state.loadSeq) return;
      state.listError = App.I18n.error(e);
      state.loaded = true;
      renderList();
    });
  }
  // A reload requested while one is in flight runs again afterwards: the caller always gets
  // listings fetched after its request (a completion is never answered by an older poll).
  // Whatever a render throws, the loading slot is released and polling continues.
  function loadAll(light) {
    if (state.loading) { state.loadAgain = true; if (!light) state.loadAgainFull = true; return state.loading; }
    function followUps() { if (!state.loadAgain) return null; var full = state.loadAgainFull; state.loadAgain = false; state.loadAgainFull = false; return fetchListings(!full).then(followUps); }
    function done(error) { state.loading = null; if (error) console.error('listing render failed', error); schedulePoll(); }
    state.loading = fetchListings(!!light).then(followUps).then(function () { done(null); }, done);
    return state.loading;
  }
  function recompose() {
    var previous = selected();
    state.contents = Model.compose(state.catalog, state.jobs, state.queue);
    // a check survives only on a row that still offers its checkbox (catalog, not orphaned, not queued or running)
    Object.keys(state.checked).forEach(function (id) { if (!state.contents.some(function (c) { return c.asset_id === id && c.kind === 'catalog' && !c.orphan && c.state !== 'queued' && c.state !== 'running'; })) delete state.checked[id]; });
    renderList();
    renderQueue();
    renderLease();
    var content = selected();
    if (content) {
      var changed = previous && content.kind === 'catalog' && Model.catalogGeneration(previous) !== Model.catalogGeneration(content);
      if (changed) {
        discardCatalogDetails();
      }
      renderHead(content); renderRuns(content); renderTransform(content); renderPlaybackControls(content);
      if (changed) {
        renderReport(content); renderInput(content);
        loadCatalogInfo(content, state.selectToken);
        if (Model.hasCatalogReport(content)) loadCatalogReport(content, state.selectToken);
      }
    }
    else if (state.selectedId && state.loaded) deselect(); // the selection vanished: nothing of it may linger
    announceSkips();
  }
  function discardCatalogDetails() {
    state.selectToken += 1;
    state.detail.report = null; state.detail.reportError = null; state.detail.catalogInfo = null;
    revokeSession(); if (playback) playback.hide();
    UI.replace(els.previewState);
  }
  // Everything derived from the selection is dropped, exactly as on a change of selection.
  function deselect() {
    state.selectToken += 1;
    state.selectedId = null;
    state.detail = { catalogInfo: null, analysis: null, analysisError: null, result: null, resultError: null, report: null, reportError: null };
    state.openInfo = {};
    state.previewNeedsCredentials = false;
    clearSecrets();
    revokeSession();
    playback.hide();
    if (state.logView && !state.logView.live) closeLogView();
    UI.replace(els.contentNotice); UI.replace(els.previewState); UI.replace(els.transformState);
    try { history.replaceState(null, '', '/'); } catch (_e) { /* not available */ }
    renderDetailVisibility();
    renderLogPanel();
  }
  function skipKey(item) { return item.asset_id + '@' + (item.at || ''); }
  function skipText(item) {
    var reason = UI.safeCode(item.reason) || '';
    return t('queueSkipped', { asset: item.asset_id, reason: App.I18n.has('qreason_' + reason) ? t('qreason_' + reason) : reason });
  }
  function visibleSkips() {
    // the latest ten undismissed skips (the server keeps fifty recent outcomes)
    return (state.queue.recent || []).filter(function (item) { return item && item.outcome === 'skipped' && typeof item.asset_id === 'string' && !state.dismissedSkips[skipKey(item)]; }).slice(0, 10);
  }
  // Each new skip is announced once through the queue's live line; a skip of the selected
  // content is also written on its page (never over a run in progress), once per page visit.
  function announceSkips() {
    visibleSkips().forEach(function (item) {
      var key = skipKey(item);
      if (!state.announcedSkips[key]) { state.announcedSkips[key] = true; UI.setText(els.queueNotice, skipText(item)); }
      if (item.asset_id !== state.selectedId || state.seenSkips[key]) return;
      state.seenSkips[key] = true;
      if (state.run && state.run.assetId === item.asset_id) return; // the run's own outcome owns the page
      UI.replace(els.contentNotice, UI.notice('fail', skipText(item)));
    });
  }
  function schedulePoll() {
    if (state.pollTimer) { clearTimeout(state.pollTimer); state.pollTimer = null; }
    var active = isJobRunning() || (state.queue.pending || []).length > 0 || (state.queue.running && !state.run);
    if (active) state.pollTimer = setTimeout(function () { state.pollTimer = null; loadAll(true); }, POLL_MS);
  }
  // A running job that this page did not launch (queue runner, another tab, a reload).
  function discoverRunning() {
    if (isJobRunning()) return;
    // a job this page already saw finish is never re-adopted from a stale listing
    var running = state.jobs.filter(function (job) { return job.state === 'running' && !state.finishedJobs[job.job_id]; })[0];
    if (running) restoreRun(running);
  }
  // Keyboard focus survives a re-render: controls carry a data-focus-key, and the focused
  // key is looked up again in the rebuilt container.
  function focusKeyOf(container) {
    var active = document.activeElement;
    if (!active || !active.dataset || !active.dataset.focusKey) return null;
    for (var node = active; node; node = node.parentNode) if (node === container) return active.dataset.focusKey;
    return null;
  }
  function findFocusKey(node, key) {
    var children = node.children || [];
    for (var i = 0; i < children.length; i++) {
      var child = children[i];
      if (!child || child.nodeType !== 1) continue;
      if (child.dataset && child.dataset.focusKey === key) return child;
      var deeper = findFocusKey(child, key);
      if (deeper) return deeper;
    }
    return null;
  }
  function restoreFocus(container, key) {
    if (!key) return;
    var target = findFocusKey(container, key);
    if (target && typeof target.focus === 'function') target.focus();
  }

  // ========================================================================================
  // Left pane: list, bulk selection, queue
  // ========================================================================================
  function visibleContents() {
    var q = state.filter.trim().toLowerCase();
    return state.contents.filter(function (c) { return !q || c.asset_id.toLowerCase().indexOf(q) >= 0 || String(c.label || '').toLowerCase().indexOf(q) >= 0; });
  }
  function renderList() {
    var focused = focusKeyOf(els.contentList);
    UI.setText(els.listCount, state.contents.length ? t(state.contents.length === 1 ? 'listCountOne' : 'listCount', { n: state.contents.length }) : '');
    els.listTruncated.hidden = !state.jobsTruncated;
    els.listError.hidden = !state.listError;
    UI.setText(els.listError, state.listError ? t('listLoadFailed') + ' ' + state.listError : '');
    var rows = visibleContents();
    if (!rows.length) {
      UI.replace(els.contentList, h('li', { className: 'list-empty', text: t(state.contents.length ? 'listNone' : (state.loaded ? 'listEmpty' : 'loading')) }));
      renderBulkBar();
      return;
    }
    // Two input kinds, side by side: existing DRM content (catalog) and clear HLS.
    var groups = [{ key: 'catalog', items: rows.filter(function (c) { return c.kind === 'catalog'; }) }, { key: 'hls', items: rows.filter(function (c) { return c.kind === 'hls'; }) }];
    UI.replace(els.contentList, groups.map(function (group) { return !group.items.length ? null : [h('li', { className: 'list-group', role: 'presentation', text: t('group_' + group.key) }), group.items.map(renderRow)]; }));
    restoreFocus(els.contentList, focused);
    renderBulkBar();
  }
  function renderRow(content) {
      var selectable = content.kind === 'catalog' && !content.orphan && content.state !== 'queued' && content.state !== 'running';
      var check = selectable
        ? h('input', { type: 'checkbox', className: 'row-check', checked: !!state.checked[content.asset_id], 'aria-label': content.asset_id, dataset: { focusKey: 'check:' + content.asset_id }, onchange: function (e) { if (e.target.checked) state.checked[content.asset_id] = true; else delete state.checked[content.asset_id]; renderBulkBar(); } })
        : h('span', { className: 'row-spacer' });
      var stateKind = stateChipKind(content.state);
      return h('li', { className: 'content-row', dataset: { assetId: content.asset_id, state: content.state }, 'aria-current': content.asset_id === state.selectedId ? 'true' : null },
        check,
        h('button', { type: 'button', className: 'row-main', dataset: { focusKey: 'row:' + content.asset_id }, onclick: function () { selectContent(content.asset_id); } },
          h('span', { className: 'row-id mono', text: content.asset_id }),
          h('span', { className: 'row-label', text: content.label || '' }),
          h('span', { className: 'row-state' }, UI.chip(stateKind, t('cstate_' + content.state))),
          h('span', { className: 'row-kind', text: t('kind_' + content.kind) })));
  }
  function renderBulkBar() {
    var n = Object.keys(state.checked).length;
    els.bulkBar.hidden = n === 0;
    UI.setText(els.bulkCount, t('bulkSelected', { n: n }));
  }
  function renderQueue() {
    var pending = state.queue.pending || [];
    var skipped = visibleSkips();
    var focused = focusKeyOf(els.queuePane);
    els.queuePane.hidden = !pending.length && !skipped.length;
    if (!pending.length) UI.setText(els.queueNotice, '');
    UI.replace(els.queueList, pending.map(function (item) {
      return h('li', null, h('span', { className: 'mono', text: item.asset_id }),
        h('button', { type: 'button', className: 'btn-link', text: t('queueRemove'), 'aria-label': t('queueRemoveLabel', { asset: item.asset_id }), dataset: { focusKey: 'qrm:' + item.asset_id }, onclick: function () { removeFromQueue(item.asset_id); } }));
    }));
    // Items the runner could not launch stay visible, with their reason, until dismissed.
    UI.replace(els.queueSkipped, skipped.length ? h('div', { className: 'queue-head', text: t('queueSkippedTitle') }) : null, skipped.map(function (item) {
      return h('li', { className: 'queue-skip' }, h('span', { text: skipText(item) }),
        h('button', { type: 'button', className: 'btn-link', text: t('dismiss'), 'aria-label': t('dismissLabel', { asset: item.asset_id }), dataset: { focusKey: 'qdis:' + skipKey(item) }, onclick: function () { state.dismissedSkips[skipKey(item)] = true; renderQueue(); } }));
    }));
    restoreFocus(els.queuePane, focused);
  }
  function enqueue(assetIds) {
    return App.Api.postQueue(assetIds).then(function () {
      UI.setText(els.queueNotice, t('queueAdded', { n: assetIds.length }));
      state.checked = Object.create(null);
      return loadAll();
    }, function (e) { UI.setText(els.queueNotice, t('queueAddFailed') + ' ' + App.I18n.error(e)); });
  }
  function removeFromQueue(assetId) {
    App.Api.deleteQueueItem(assetId).then(function () { UI.setText(els.queueNotice, ''); return loadAll(); }, function (e) { UI.setText(els.queueNotice, App.I18n.error(e)); });
  }

  // ========================================================================================
  // Selection and the detail page
  // ========================================================================================
  function renderDetailVisibility() {
    var content = selected();
    els.detailEmpty.hidden = !!content || state.addOpen;
    els.addSection.hidden = !state.addOpen;
    els.content.hidden = !content || state.addOpen;
    Array.prototype.forEach.call(els.contentList.children, function (row) {
      if (row.dataset && row.dataset.assetId) row.setAttribute('aria-current', row.dataset.assetId === state.selectedId ? 'true' : 'false');
    });
  }
  // Everything derived from the previous selection is dropped: analysis, result, report,
  // secrets, the playback session and both players. The log panel follows the selection.
  function selectContent(assetId) {
    if (assetId === state.selectedId && !state.addOpen) return;
    state.selectToken += 1;
    state.selectedId = assetId;
    state.addOpen = false;
    state.detail = { catalogInfo: null, analysis: null, analysisError: null, result: null, resultError: null, report: null, reportError: null };
    state.openInfo = {};
    state.previewNeedsCredentials = false;
    clearSecrets();
    revokeSession();
    playback.hide();
    UI.replace(els.contentNotice);
    UI.replace(els.previewState);
    UI.replace(els.transformState);
    if (state.logView && !state.logView.live) closeLogView();
    var content = selected();
    renderDetailVisibility();
    if (!content) return;
    try { history.replaceState(null, '', '/?asset=' + encodeURIComponent(assetId)); } catch (_e) { /* not available */ }
    renderDetail(content);
    announceSkips();
    if (typeof els.contentId.focus === 'function') els.contentId.focus();
    var token = state.selectToken;
    if (content.kind === 'catalog' && content.orphan) {
      // nothing to fetch: the entry is gone; runs and logs are the content
    } else if (content.kind === 'catalog') {
      loadCatalogInfo(content, token);
      if (Model.hasCatalogReport(content)) loadCatalogReport(content, token);
    } else {
      if (content.analysis_id) loadAnalysis(content, token);
      if (content.result_id) loadResult(content, content.result_id, token);
    }
    // the running job of this content: show its live log
    if (state.run && state.run.assetId === assetId) showLogView(state.run.view);
    else if (state.logView && state.logView.assetId !== assetId) renderLogPanel();
  }
  function loadCatalogInfo(content, token) {
    App.Api.getAssetUrls(content.asset_id).then(function (info) {
      if (token !== state.selectToken) return;
      state.detail.catalogInfo = info;
      renderInput(content);
    }, function (e) { if (token === state.selectToken) UI.replace(els.inputState, UI.notice('fail', t('inspectLoadFailed') + ' ' + App.I18n.error(e))); });
  }
  function loadCatalogReport(content, token) {
    UI.replace(els.reportState, h('p', { className: 'state-line', text: t('reportLoading') }));
    App.Api.getCatalogReport(content.asset_id).then(function (report) {
      if (token !== state.selectToken) return;
      state.detail.report = report; state.detail.reportError = null;
      renderReport(content); renderTransform(content);
    }, function (e) {
      if (token !== state.selectToken) return;
      state.detail.report = null;
      state.detail.reportError = e && e.status === 404 ? t('reportNotFound') : e && e.status === 410 ? t('reportUnavailable') : App.I18n.error(e);
      renderReport(content);
    });
  }
  function loadAnalysis(content, token) {
    UI.replace(els.inputState, h('p', { className: 'state-line', text: t('analysisLoading') }));
    App.Api.getAnalysis(content.analysis_id).then(function (analysis) {
      if (token !== state.selectToken) return;
      state.detail.analysis = analysis; state.detail.analysisError = null;
      renderInput(content); renderTransform(content);
    }, function (e) {
      if (token !== state.selectToken) return;
      state.detail.analysisError = e && e.status === 410 ? t('analysisUnavailable') : App.I18n.error(e);
      renderInput(content); renderTransform(content);
    });
  }
  function loadResult(content, resultId, token) {
    UI.replace(els.reportState, h('p', { className: 'state-line', text: t('resultLoading') }));
    return App.Api.getHlsResult(resultId).then(function (result) {
      if (token !== state.selectToken) return;
      state.detail.result = result; state.detail.resultError = null;
      if (!state.detail.analysis && !content.analysis_id && result.analysis_id) { content.analysis_id = result.analysis_id; loadAnalysis(content, token); }
      renderReport(content); renderTransform(content); renderPlaybackControls(content);
    }, function (e) {
      if (token !== state.selectToken) return;
      state.detail.resultError = e && e.status === 410 ? t('resultUnavailable') : App.I18n.error(e);
      renderReport(content); renderTransform(content);
    });
  }

  function renderDetail(content) {
    renderHead(content);
    renderInput(content);
    renderTransform(content);
    renderRuns(content);
    renderReport(content);
    renderPlaybackControls(content);
    renderLogPanel();
  }
  function stateChipKind(st) { return st === 'packaged' ? 'good' : st === 'running' ? 'run' : st === 'failed' ? 'fail' : st === 'queued' ? 'info' : (st === 'orphan' || st === 'unverified') ? 'warn' : null; }
  function reportLink(kind, id, asset) {
    return h('a', { className: 'btn btn-sm', href: '/reports/' + kind + '/' + encodeURIComponent(id), text: t('viewReport'), 'aria-label': t('viewReportLabel', { asset: asset }) });
  }
  function renderHead(content) {
    UI.setText(els.contentId, content.asset_id);
    UI.setText(els.contentLabel, content.label || '');
    UI.replace(els.contentChips, UI.chip(null, t('kind_' + content.kind)), UI.chip(stateChipKind(content.state), t('cstate_' + content.state)));
    var actions = [];
    var focused = focusKeyOf(els.contentActions);
    var busy = isJobRunning() || content.state === 'running' || content.state === 'queued';
    if (content.kind === 'catalog' && content.orphan) {
      // read-only: the entry is gone, nothing can be launched or linked
    } else if (content.kind === 'catalog') {
      actions.push(h('button', { type: 'button', className: 'btn btn-primary', id: 'convert-btn', disabled: busy, dataset: { focusKey: 'convert' }, text: t(content.converted ? 'convertAgain' : 'convert'), onclick: function () { startConvert(content); } }));
      if (content.state !== 'queued' && content.state !== 'running') actions.push(h('button', { type: 'button', className: 'btn', id: 'queue-btn', dataset: { focusKey: 'queue' }, text: t('addToQueue'), onclick: function () { enqueue([content.asset_id]); } }));
      if (Model.hasCatalogReport(content)) actions.push(reportLink('catalog', content.asset_id, content.label || content.asset_id));
    } else if (content.result_id) {
      actions.push(reportLink('hls', content.result_id, content.asset_id));
    }
    UI.replace(els.contentActions, actions);
    restoreFocus(els.contentActions, focused);
  }

  // --- input --------------------------------------------------------------------------------
  // The input as a plain table: renditions, codec, muxing, segments. No chart.
  function muxingLabel(x) {
    var c = String(x.container || '').toLowerCase();
    if (c === 'fmp4' || c === 'mp4' || c === 'cmaf') return 'fMP4';
    if (c === 'ts') return 'TS';
    if (c === 'webvtt' || c === 'vtt') return 'WebVTT';
    return x.container ? String(x.container) : (x.codec && /^(avc|hvc|hev|mp4a)/i.test(String(x.codec)) ? 'fMP4' : '—');
  }
  function inputTable(tracks) {
    var rows = (tracks || []).filter(function (x) { return x && (x.id != null || x.rep_id != null); });
    if (!rows.length) return h('p', { className: 'empty', text: t('noTracks') });
    function td(text, cls) { return h('td', { className: cls || null, text: text == null || text === '' ? '—' : String(text) }); }
    var order = { video: 0, audio: 1, text: 2, subtitle: 2 };
    rows.sort(function (a, b) { return (order[a.content_type] || 0) - (order[b.content_type] || 0) || ((b.bandwidth || 0) - (a.bandwidth || 0)); });
    return h('div', { className: 'table-wrap' }, h('table', { className: 'data input-table' },
      h('thead', null, h('tr', null, ['colType', 'colTrack', 'colDetail', 'colCodec', 'colMuxing', 'colBitrate', 'colSegments'].map(function (k) { return h('th', { scope: 'col', text: t(k) }); }))),
      h('tbody', null, rows.map(function (x) {
        var detail = [];
        if (x.width && x.height) detail.push(x.width + '×' + x.height);
        if (x.language && x.language !== 'und') detail.push(x.language);
        if (x.name) detail.push(String(x.name));
        if (x.muxed) detail.push(t('muxedAudio'));
        if (x.is_default) detail.push(t('defaultTrack'));
        var segments = typeof x.segment_count === 'number' ? String(x.segment_count) + (typeof x.duration === 'number' ? ' · ' + UI.fmtSeconds(x.duration) : '') : null;
        return h('tr', null, td(App.I18n.has('type_' + x.content_type) ? t('type_' + x.content_type) : x.content_type), td(x.id != null ? x.id : x.rep_id, 'mono'), td(detail.join(' · ')), td(UI.codecLabel(x), 'mono'), td(muxingLabel(x)), td(UI.fmtKbps(x.bandwidth), 'num'), td(segments, 'num'));
      }))));
  }
  function muxingSummary(tracks) {
    var seen = {};
    (tracks || []).forEach(function (x) { var m = muxingLabel(x); if (m !== '—') seen[m] = true; });
    return Object.keys(seen).join(' / ') || '—';
  }
  function findingRow(finding) {
    var severity = typeof finding === 'string' ? 'info' : (finding.severity || finding.level || 'info');
    var kind = severity === 'error' ? 'fail' : severity === 'warning' ? 'warn' : 'info';
    var code = UI.safeCode(typeof finding === 'string' ? finding : (finding.code || finding.classification || finding.token));
    var known = code && App.I18n.has('finding_' + code);
    return UI.notice(kind, known ? t('finding_' + code) : t('finding_' + kind), code);
  }
  function audioCandidates(a) { return a.audio_candidates || []; }
  function audioSelectionRequired() { var a = state.detail.analysis; return !!(a && a.requires_audio_selection); }
  function kvPairs(pairs) {
    return pairs.filter(Boolean).map(function (pair) { return [h('dt', { text: pair[0] }), h('dd', { className: pair[2] || null, text: pair[1] == null || pair[1] === '' ? '—' : pair[1] })]; });
  }
  function renderInput(content) {
    var runningHere = state.run && state.run.assetId === content.asset_id && state.run.kind === 'analysis';
    if (!runningHere) UI.replace(els.inputState);
    UI.replace(els.inputFindings); UI.replace(els.inputTable); UI.replace(els.inputKv);
    els.audioChoice.hidden = true;
    if (content.kind === 'catalog' && content.orphan) { UI.replace(els.inputState, UI.notice('warn', t('orphanNotice'))); return; }
    if (content.kind === 'catalog') {
      var info = state.detail.catalogInfo;
      var old = (info && info.old_summary) || {};
      var reps = (info && info.renditions && info.renditions.old || []).map(function (r) { return Object.assign({ id: r.rep_id }, r); });
      UI.replace(els.inputKv, kvPairs([
        [t('inputEntry'), content.entry ? '<catalog_root>/' + content.entry + '/' : null, 'mono'],
        [t('inputMpd'), content.paths && content.paths.mpd, 'mono'], [t('inputHlsMaster'), content.paths && content.paths.hls_master, 'mono'],
        info ? [t('drmState'), t('drmStateExisting', { scheme: old.protection_scheme || 'cenc', systems: (old.drm_systems || []).join(', ') || '—' })] : null,
        info ? ['KID', old.default_kid, 'mono'] : null, info ? [t('prVersion'), old.playready_version ? old.playready_version + ' / ' + (old.playready_algid || '') : null] : null,
        info ? [t('muxing'), muxingSummary(reps)] : null, [t('inputKeys'), t('inputKeysCatalog')]]));
      if (!info) { UI.replace(els.inputState, h('p', { className: 'state-line', text: t('loading') })); return; }
      UI.replace(els.inputTable, inputTable(reps));
      return;
    }
    var a = state.detail.analysis;
    UI.replace(els.inputKv, kvPairs([[t('inputHlsWhere'), t('inputHlsSnapshot', { id: content.asset_id }), 'mono'], a ? [t('drmState'), t('drmStateClear')] : null, a ? [t('muxing'), muxingSummary(a.tracks)] : null]));
    if (!a) {
      if (state.detail.analysisError) UI.replace(els.inputState, UI.notice('fail', state.detail.analysisError));
      else if (!runningHere && !content.analysis_id) UI.replace(els.inputState, UI.notice(content.state === 'failed' ? 'fail' : 'info', t(content.state === 'failed' ? 'analysisJobFailed' : 'analysisPending')));
      return;
    }
    UI.replace(els.inputState, UI.notice(a.admitted ? 'good' : 'fail', t(a.admitted ? 'analysisPass' : 'analysisRejected')));
    UI.replace(els.inputFindings, (a.findings || []).map(findingRow));
    UI.replace(els.inputTable, inputTable(a.tracks || []));
    var required = audioSelectionRequired();
    els.audioChoice.hidden = !required;
    var previous = els.audioSelect.value;
    UI.replace(els.audioSelect, h('option', { value: '', text: t('chooseAudio') }), audioCandidates(a).map(function (track) {
      return h('option', { value: String(track.id), text: String(track.label || track.id) + (track.language ? ' / ' + track.language : '') });
    }));
    if (Array.prototype.some.call(els.audioSelect.options, function (o) { return o.value === previous; })) els.audioSelect.value = previous;
  }

  // --- transformation ------------------------------------------------------------------------
  function drmReady() {
    var a = state.detail.analysis;
    if (!a || !a.admitted || state.detail.result) return false;
    var ok = true;
    Object.keys(DRM_FIELDS).forEach(function (key) { if (validateDrmValue(key, byId(DRM_FIELDS[key]).value.trim())) ok = false; });
    if (audioSelectionRequired() && !els.audioSelect.value) ok = false;
    return ok;
  }
  function renderTransform(content) {
    var isCatalog = content.kind === 'catalog';
    var report = isCatalog ? state.detail.report : state.detail.result;
    var statuses = Model.checkStatuses(report, isCatalog ? 'catalog' : 'hls');
    var done = isCatalog ? Model.hasVerifiedOutput(content) : !!content.result_id;
    var a = state.detail.analysis;
    var help = content.orphan ? 'orphanNotice' : isCatalog ? (done ? 'transformCatalogDone' : content.converted ? 'transformCatalogUnverified' : 'transformCatalogPending') : (done ? 'transformHlsDone' : (a && !a.admitted ? 'transformHlsBlocked' : 'transformHlsPending'));
    UI.setText(els.transformHelp, t(help));
    // The one line that matters: input kind -> the three outputs. Everything else is behind (i).
    var inputText = isCatalog ? t('inputExisting') : t('inputClear', { muxing: a ? muxingSummary(a.tracks) : 'HLS' });
    UI.replace(els.transformSummary, h('span', { className: 'transform-in', text: t('transformInput') + ' ' + inputText }), h('span', { className: 'transform-arrow', 'aria-hidden': 'true', text: '→' }), h('span', { className: 'transform-out', text: t('transformOutput') + ' ' + t('outputsList') }));
    // The listing poll re-renders this section every few seconds while any job runs: an open
    // (i) stays open and keyboard focus stays on its button, as in the list and the runs.
    var focused = focusKeyOf(els.transformRules);
    UI.replace(els.transformRules, Model.rules(content.kind).map(function (rule) {
      var panels = [];
      var chips = rule.checks.map(function (id) {
        var status = statuses[id] || null;
        var label = App.I18n.has('check_' + id) ? t('check_' + id) : id;
        // Each check carries what it verifies behind its own (i); the panel opens under the row.
        var info = App.I18n.has('help_' + id) ? UI.infoToggle(t('checkHelpLabel', { check: label }), t('help_' + id), { focusKey: 'info:' + id, open: !!state.openInfo[id],
          onToggle: function (open) { if (open) state.openInfo[id] = true; else delete state.openInfo[id]; } }) : null;
        if (info) panels.push(info.panel);
        return h('span', { className: 'rule-check', dataset: { status: status || 'none' }, title: label },
          h('span', { text: status ? t('check_' + status) : (done ? t('check_UNKNOWN') : t('checkPending')) }), h('code', { text: id }), info ? info.button : null);
      });
      return h('li', null, h('span', { text: t('rule_' + rule.key) }), chips.length ? h('div', { className: 'rule-checks' }, chips) : null, panels);
    }));
    restoreFocus(els.transformRules, focused);
    var showForm = !isCatalog && !!(a && a.admitted) && !state.detail.result && content.state !== 'running' && content.state !== 'packaged';
    els.drmForm.hidden = !showForm;
    if (!showForm) clearSecrets();
    refreshControls();
  }

  // --- runs and the log panel ----------------------------------------------------------------
  function renderRuns(content) {
    var jobs = content.jobs || [];
    var focused = focusKeyOf(els.runsList);
    if (!jobs.length) { UI.replace(els.runsList, h('li', { className: 'empty', text: t('runsEmpty') })); return; }
    UI.replace(els.runsList, jobs.map(function (job) {
      var chipKind = job.state === 'succeeded' ? 'good' : job.state === 'running' ? 'run' : job.state === 'interrupted' ? 'warn' : 'fail';
      var current = state.logView && state.logView.jobId === job.job_id;
      var actions = [h('button', { type: 'button', className: 'btn btn-sm', text: t('openLog'), 'aria-pressed': current ? 'true' : 'false', dataset: { focusKey: 'log:' + job.job_id }, onclick: function () { openLog(job); } })];
      if (job.state === 'running') {
        var stopping = job.stop_requested || state.stopRequests[job.job_id];
        actions.push(h('button', { type: 'button', className: 'btn btn-quiet btn-sm', text: t(stopping ? 'stopPending' : 'stopJob'),
          'aria-label': t(stopping ? 'stopPendingLabel' : 'stopJobLabel', { asset: job.asset_id || job.job_id }), 'aria-disabled': stopping ? 'true' : null,
          title: t('stopHelp'), dataset: { focusKey: 'stop:' + job.job_id }, onclick: function () { requestStop(job); } }));
      }
      if (job.state !== 'running') actions.push(h('button', { type: 'button', className: 'btn btn-quiet btn-sm', text: t('removeHistory'), title: t('historyHelp'), 'aria-label': t('removeHistoryLabel', { asset: job.asset_id || job.job_id }), dataset: { focusKey: 'rm:' + job.job_id }, onclick: function (e) { deleteHistoryJob(job, e.target); } }));
      return h('li', { className: 'run-row', dataset: { jobId: job.job_id }, 'aria-current': current ? 'true' : null },
        h('span', { text: t('kind_' + job.kind) }),
        UI.chip(chipKind, App.I18n.has('state_' + job.state) ? t('state_' + job.state) : String(job.state)),
        h('span', { className: 'run-when', text: (UI.fmtDate(job.created_at, App.I18n.getLanguage()) || t('timeUnknown')) + (job.exit_code != null ? ' · ' + t('exitCode', { code: String(job.exit_code) }) : '') }),
        h('span', { className: 'run-actions' }, actions));
    }));
    restoreFocus(els.runsList, focused);
  }
  function requestStop(job) {
    if (!job || job.state !== 'running' || job.stop_requested || state.stopRequests[job.job_id]) return Promise.resolve();
    state.stopRequests[job.job_id] = 'sending';
    var content = selected();
    if (content) renderRuns(content);
    return App.Api.stopJob(job.job_id).then(function (status) {
      if (status && status.stop_requested) {
        state.stopRequests[job.job_id] = 'requested';
        if (!state.finishedJobs[job.job_id] && (!state.run || state.run.jobId === job.job_id)) notify(job.asset_id, 'info', t('stopRequested') + ' ' + t('stopHelp'));
      } else delete state.stopRequests[job.job_id];
      // Acceptance is not a terminal event: keep observing the same run and its log.
      return loadAll(true);
    }, function (error) {
      delete state.stopRequests[job.job_id];
      if (!state.finishedJobs[job.job_id] && (!state.run || state.run.jobId === job.job_id)) notify(job.asset_id, 'fail', t('stopFailed') + ' ' + App.I18n.error(error));
      var current = selected();
      if (current) renderRuns(current);
    });
  }
  function deleteHistoryJob(job, button) {
    if (button) button.disabled = true;
    App.Api.deleteHlsJob(job.job_id).then(function () { if (state.logView && state.logView.jobId === job.job_id) closeLogView(); return loadAll(); },
      function (e) { if (button) button.disabled = false; UI.replace(els.contentNotice, UI.notice('fail', t('deleteFailed') + ' ' + App.I18n.error(e))); });
  }
  // Open a job's log: the live view for the running job, a replay through the same SSE route
  // (every line, then `done`) for a finished one.
  function openLog(job) {
    if (state.logView && state.logView.jobId === job.job_id && !els.logPanel.hidden) { closeLogView(); return; } // pressed again: close
    if (state.run && state.run.jobId === job.job_id) { showLogView(state.run.view); return; }
    if (state.logView && state.logView.jobId === job.job_id) { renderLogPanel(); return; }
    closeLogView();
    var view = newLogView(job.job_id, job.asset_id, job.kind, false);
    view.notice = t('logLoading');
    state.logView = view;
    renderLogPanel();
    var owner = selected();
    if (owner) renderRuns(owner); // the pressed state of the log buttons follows the view
    view.watcher = App.Jobs.watch(job.job_id, {
      onLine: function (line) { if (state.logView === view) appendLine(view, line); },
      onTerminal: function (status, info) { if (state.logView !== view) return; view.outcome = status.state; view.interrupted = !!(info && info.logInterrupted); view.notice = ''; settleProgress(view, status.state === 'succeeded'); renderLogPanel(); },
      onLost: function (reason) { if (state.logView !== view) return; view.notice = t(reason); view.outcome = 'lost'; renderLogPanel(); },
      onWaiting: function (reason) { if (state.logView !== view) return; view.notice = t(reason); renderLogPanel(); },
    });
  }
  function showLogView(view) { state.logView = view; renderLogPanel(); var content = selected(); if (content) renderRuns(content); }
  function closeLogView() {
    var view = state.logView;
    if (view && !view.live && view.watcher) { view.watcher.stop(); view.watcher = null; }
    state.logView = null;
    renderLogPanel();
    var content = selected();
    if (content) renderRuns(content);
  }
  function appendLine(view, line) {
    var rec;
    try { rec = JSON.parse(line); } catch (_e) { rec = { message: line }; }
    if (!rec || typeof rec !== 'object') rec = { message: String(line) };
    view.lines.push(rec);
    applyRecord(view, rec);
    if (state.run && state.run.view === view && rec.stage && rec.stage !== 'tool') renderLease(); // the stage line follows the log, not the clock
    if (state.logView === view && els.logPanel.hidden === false) {
      // Follow the tail only while the reader is at the bottom; a reader who scrolled up stays put.
      var follow = els.logList.scrollTop + els.logList.clientHeight >= els.logList.scrollHeight - 8;
      if (state.logRaw) appendRawLine(view, rec); else els.logList.appendChild(logLine(rec));
      if (follow) els.logList.scrollTop = els.logList.scrollHeight;
      renderLogMeta(view);
    }
  }
  // The JSON view appends one text node per record: a log holds thousands of tool records, and
  // rebuilding the whole text for each one would freeze the page.
  function appendRawLine(view, rec) {
    var raw = els.logList.children && els.logList.children[0];
    // Only the last text node is inspected: reading the whole text on every append would rebuild
    // megabytes per record.
    if (raw && raw.className === 'log-raw') raw.appendChild(document.createTextNode((raw.lastChild && raw.lastChild.textContent ? '\n' : '') + JSON.stringify(rec)));
    else renderLogLines(view);
  }
  function applyRecord(view, rec) {
    var p = view.progress;
    // line and the progress strips follow the pipeline's records only.
    if (!rec.stage || rec.stage === 'tool') return;
    p.lastRec = rec;
    if (rec.stage === 'hls-source') {
      var id = UI.safeCode(rec.rendition_id || rec.track_id);
      if (id) {
        var row = p.tracks[id] || (p.tracks[id] = { status: 'running', index: null, total: null });
        if (Number.isInteger(rec.index) && Number.isInteger(rec.total) && rec.total > 0 && rec.index >= 0 && rec.index <= rec.total) { row.index = rec.index; row.total = rec.total; }
        if (rec.message === 'playlist-complete') row.status = 'done';
      }
      return;
    }
    var rid = UI.safeCode(rec.rendition_id || rec.track_id);
    if (!rid) return;
    var current = p.renditions[rid];
    if (rec.stage === 'report') p.renditions[rid] = 'done';
    else if ((rec.stage === 'decrypt' || rec.stage === 'encrypt') && rec.level === 'error') { if (current !== 'done') p.renditions[rid] = 'failed'; }
    else if (rec.stage === 'package-hls' && rec.message === 'track-packaged') p.renditions[rid] = 'done';
    else if (rec.stage === 'package-hls' && rec.message === 'track-failed') p.renditions[rid] = 'failed';
    else if ((RENDITION_WORK_STAGES[rec.stage] || rec.stage === 'package-hls') && current === undefined) p.renditions[rid] = 'running';
  }
  // Make the counts honest: anything still running after a terminal state was aborted -- unless
  // part of the log was never received, in which case unseen work is marked unknown.
  function settleProgress(view, success) {
    var settle = view.interrupted ? 'unknown' : (success ? 'done' : 'failed');
    Object.keys(view.progress.renditions).forEach(function (id) { if (view.progress.renditions[id] === 'running') view.progress.renditions[id] = settle; });
    Object.keys(view.progress.tracks).forEach(function (id) { if (view.progress.tracks[id].status === 'running') view.progress.tracks[id].status = settle; });
  }
  function stageText(view, withCounter) {
    var rec = view.progress.lastRec;
    if (view.outcome && view.outcome !== 'lost') return t(view.outcome === 'succeeded' ? 'outcomeSucceeded' : view.outcome === 'interrupted' ? 'outcomeInterrupted' : 'outcomeFailed');
    if (!rec || !rec.stage) return view.live ? t('stageStarting') : '';
    if (rec.stage === 'hls-source') return sourceProgressText(rec, withCounter !== false);
    var label = CATALOG_STAGE_KEYS[rec.stage] ? t(CATALOG_STAGE_KEYS[rec.stage]) : rec.stage === 'package-hls' ? t('stagePackaging') : rec.stage === 'analysis' ? t('stageAnalyzing') : t('stageWorking');
    var rid = UI.safeCode(rec.rendition_id || rec.track_id);
    return rid ? label + ' · ' + rid : label;
  }
  function sourceCounter(rec) {
    return rec && Number.isInteger(rec.index) && Number.isInteger(rec.total) && rec.total > 0 && rec.index >= 0 && rec.index <= rec.total ? rec.index + ' / ' + rec.total : '';
  }
  function sourceProgressText(rec, withCounter) {
    var text = t(SOURCE_PROGRESS_KEYS[rec.message] || 'stageAnalyzing');
    var id = UI.safeCode(rec.rendition_id);
    if (id) text += ' · ' + id;
    var counter = withCounter ? sourceCounter(rec) : '';
    return counter ? text + ' · ' + counter : text;
  }
  function logLineText(rec) {
    if (rec.stage === 'hls-source') return sourceProgressText(rec);
    var message = typeof rec.message === 'string' ? rec.message : '';
    var id = UI.safeCode(rec.rendition_id || rec.track_id);
    // Classification tokens (clear-HLS logs) carry their translation; everything else is shown as written.
    if (App.I18n.has('log_' + message)) return (id ? id + ': ' : '') + t('log_' + message) + ' (' + message + ')';
    if (App.I18n.has('finding_' + message)) return (id ? id + ': ' : '') + t('finding_' + message) + ' (' + message + ')';
    return (id ? id + ': ' : '') + message;
  }
  // A shell-like rendering of argv: tokens with spaces or quotes are quoted so the line can be
  // copied; nothing is interpreted.
  // shlex.quote's rule: a token stays bare only when every character is shell-safe; an empty
  // token is written as '' so it is not lost when the line is pasted.
  function shellLine(argv) {
    return (argv || []).map(function (token) {
      token = String(token);
      if (token === '') return "''";
      return /^[A-Za-z0-9_@%+=:,.\/-]+$/.test(token) ? token : "'" + token.replace(/'/g, "'\\''") + "'";
    }).join(' ');
  }
  function logLine(rec) {
    var ts = rec.ts ? String(rec.ts).replace(/^.*T/, '').replace(/\..*$/, '') : '';
    if (rec.event === 'command' && Array.isArray(rec.argv)) {
      return h('div', { className: 'log-line log-cmd level-info' }, h('span', { className: 'ts', text: ts }), h('span', { className: 'log-stage', text: (UI.safeCode(rec.tool) || 'tool') + (Number.isInteger(rec.call_id) ? ' #' + rec.call_id : '') }),
        h('span', { className: 'cmd' }, h('span', { className: 'prompt', 'aria-hidden': 'true', text: '$ ' }), shellLine(rec.argv)));
    }
    if (rec.event === 'tool-output') {
      var failed = rec.returncode !== 0;
      var meta = [t('logExit', { code: rec.returncode == null ? '—' : String(rec.returncode) })];
      if (typeof rec.duration_s === 'number') meta.push(rec.duration_s.toFixed(2) + ' s');
      if (rec.truncated) meta.push(t('logTruncated'));
      var hasBody = !!(rec.stdout || rec.stderr);
      // Thousands of tool outputs can sit in one log: the text is built only when its
      // disclosure is opened (failed outputs open at once), so the page stays responsive.
      var body = h('div', { className: 'out-body' });
      var built = false;
      function build() {
        if (built) return; built = true;
        if (rec.stdout) body.append(h('div', { className: 'out-label', text: 'stdout' }), h('pre', { className: 'out', text: rec.stdout }));
        if (rec.stderr) body.append(h('div', { className: 'out-label', text: 'stderr' }), h('pre', { className: 'out', text: rec.stderr }));
      }
      var details = h('details', { className: 'out-box' }, h('summary', { text: meta.join(' · ') + (hasBody ? '' : ' · ' + t('toolOutputEmpty')) }), hasBody ? body : null);
      details.addEventListener('toggle', function () { if (details.open) build(); });
      if (failed && hasBody) { details.open = true; build(); }
      return h('div', { className: 'log-line log-out level-' + (failed ? 'error' : 'info') }, h('span', { className: 'ts', text: ts }), h('span', { className: 'log-stage', text: (UI.safeCode(rec.tool) || 'tool') + (Number.isInteger(rec.call_id) ? ' #' + rec.call_id : '') }), details);
    }
    return h('div', { className: 'log-line level-' + (rec.level || 'info') }, h('span', { className: 'ts', text: ts }), h('span', { className: 'log-stage', text: rec.stage || '-' }), h('span', { text: logLineText(rec) }));
  }
  function renderLogLines(view) {
    if (state.logRaw) UI.replace(els.logList, h('pre', { className: 'log-raw', text: view.lines.map(function (rec) { return JSON.stringify(rec); }).join('\n') }));
    else UI.replace(els.logList, view.lines.map(logLine));
    els.logList.scrollTop = els.logList.scrollHeight;
  }
  function renderLogMeta(view) {
    var live = view.live && state.run && state.run.view === view;
    var chipKind = live ? 'run' : view.outcome === 'succeeded' ? 'good' : view.outcome === 'interrupted' ? 'warn' : view.outcome ? 'fail' : null;
    els.logChip.className = 'chip' + (chipKind ? ' chip-' + chipKind : '');
    UI.setText(els.logChip, live ? t('logLive') + ' · ' + UI.fmtElapsed(Date.now() - state.run.startedAt, t) : (view.outcome ? (App.I18n.has('state_' + view.outcome) ? t('state_' + view.outcome) : t('jobLost')) : t('logReplay')));
    UI.setText(els.logStage, stageText(view));
    UI.setText(els.logCount, String(view.lines.length));
    UI.setText(els.logNotice, view.notice || (view.interrupted && !live ? t('logInterrupted') : ''));
    var p = view.progress;
    var rows = Object.keys(p.renditions).map(function (id) { var st = p.renditions[id]; return { id: id, label: id, status: st, fraction: null, state: t('strip_' + st) }; })
      .concat(Object.keys(p.tracks).map(function (id) { var row = p.tracks[id]; var fraction = row.total ? Math.min(1, row.index / row.total) : null; return { id: id, label: id, status: row.status, fraction: fraction, state: row.total ? row.index + '/' + row.total : t('strip_' + row.status) }; }));
    UI.replace(els.logStrips, rows.length ? UI.strips(rows) : null);
  }
  function renderLogPanel() {
    var view = state.logView;
    var content = selected();
    if (!view || !content || view.assetId !== content.asset_id) { els.logPanel.hidden = true; return; }
    els.logPanel.hidden = false;
    UI.setText(els.logTitle, t('kind_' + view.kind) + (view.jobId ? ' · ' + String(view.jobId).slice(0, 8) : ''));
    els.logJson.setAttribute('aria-pressed', String(!!state.logRaw));
    UI.setText(els.logJson, t(state.logRaw ? 'logFormatted' : 'logJson'));
    var jobId = UI.safeId(view.jobId || '');
    els.logDownload.hidden = !jobId;
    if (jobId) els.logDownload.setAttribute('href', '/api/jobs/' + encodeURIComponent(jobId) + '/log.jsonl');
    renderLogLines(view);
    renderLogMeta(view);
  }

  // --- report and playback -------------------------------------------------------------------
  function renderReport(content) {
    UI.replace(els.reportView);
    if (content.orphan) { UI.replace(els.reportState, h('p', { className: 'help', text: t('reportNoneOrphan') })); return; }
    if (content.kind === 'catalog') {
      if (state.detail.report) { UI.replace(els.reportState); App.ReportView.render(els.reportView, state.detail.report, { kind: 'catalog', id: content.asset_id }, { pageLink: true }); }
      else if (state.detail.reportError) UI.replace(els.reportState, UI.notice('fail', state.detail.reportError));
      else UI.replace(els.reportState, h('p', { className: 'help', text: t(Model.hasVerifiedOutput(content) ? 'reportLoading' : content.converted ? 'outputUnverified' : 'reportNoneCatalog') }));
      return;
    }
    if (state.detail.result) { UI.replace(els.reportState); App.ReportView.render(els.reportView, state.detail.result, { kind: 'hls', id: state.detail.result.result_id || content.result_id }, { pageLink: true }); }
    else if (state.detail.resultError) UI.replace(els.reportState, h('div', null, UI.notice('fail', state.detail.resultError), content.result_id ? h('div', { className: 'actions' }, h('button', { type: 'button', className: 'btn btn-sm', text: t('retryResult'), onclick: function () { loadResult(content, content.result_id, state.selectToken); } })) : null));
    else if (!content.result_id) UI.replace(els.reportState, h('p', { className: 'help', text: t('reportNoneHls') }));
  }
  function renderPlaybackControls(content) {
    var ready = content.orphan ? false : content.kind === 'catalog' ? Model.hasVerifiedOutput(content) : !!(state.detail.result && state.detail.result.all_pass);
    els.previewBtn.disabled = !ready || isJobRunning() || state.previewBusy;
    els.previewCredentials.hidden = !state.previewNeedsCredentials;
  }
  function revokeSession() {
    var id = state.sessionId;
    state.sessionId = null;
    if (id) App.Api.revokePlaybackSession(id).catch(function () {});
  }
  function preparePreview(withCredentials) {
    var content = selected();
    if (!content || isJobRunning() || state.previewBusy || (content.kind === 'catalog' && !Model.hasVerifiedOutput(content))) return;
    var token = state.selectToken;
    state.previewBusy = true;
    UI.replace(els.previewState, h('span', { className: 'state-line', text: t('previewStarting') }));
    renderPlaybackControls(content);
    if (content.kind === 'catalog') {
      App.Api.getAssetUrls(content.asset_id).then(function (info) {
        if (token !== state.selectToken) return;
        state.detail.catalogInfo = info;
        playback.hide();
        if (!info.output_readiness || info.output_readiness.state !== 'verified' || !UI.safeOutputUrl(info.new_url)) {
          content.output_readiness = { state: 'unverified', reason: 'generation-unconfirmed', job_id: null };
          state.catalog.forEach(function (row) { if (row.asset_id === content.asset_id) row.output_readiness = content.output_readiness; });
          discardCatalogDetails();
          content.state = Model.contentState(content, state.queue);
          state.detail.catalogInfo = info;
          renderHead(content); renderTransform(content); renderReport(content);
          UI.replace(els.previewState, UI.notice('warn', t('outputUnverified')));
          return;
        }
        playback.prepareCatalog(content.asset_id, info);
        UI.replace(els.previewState, h('span', { className: 'state-line', text: t('previewReady') }));
      }, function (e) { if (token === state.selectToken) UI.replace(els.previewState, UI.notice('fail', t('playbackInfoFailed') + ' ' + App.I18n.error(e))); })
        .then(function () { state.previewBusy = false; if (token === state.selectToken) renderPlaybackControls(content); });
      return;
    }
    var result = state.detail.result;
    var body = { result_id: result.result_id };
    if (withCredentials) { body.kid = els.previewKid.value.trim(); body.key = els.previewKey.value.trim(); }
    var request = App.Api.createPlaybackSession(body);
    els.previewKid.value = ''; els.previewKey.value = ''; body = null;
    request.then(function (session) {
      if (token !== state.selectToken || state.detail.result !== result) {
        if (session && session.session_id) App.Api.revokePlaybackSession(session.session_id).catch(function () {});
        return;
      }
      revokeSession();
      var urls = { oldUrl: UI.safeOutputUrl(session.old_url), newUrl: UI.safeOutputUrl(session.new_url), licenseUrl: UI.safeOutputUrl(session.license_url) };
      if (!urls.oldUrl || !urls.newUrl || !urls.licenseUrl) {
        if (session && session.session_id) App.Api.revokePlaybackSession(session.session_id).catch(function () {}); // never kept
        throw { unsafeUrl: true };
      }
      state.sessionId = session.session_id;
      playback.hide();
      state.previewNeedsCredentials = false;
      playback.prepareHls(result, urls, session.source_renditions);
      UI.replace(els.previewState, h('span', { className: 'state-line', text: t('previewReady') }));
    }).catch(function (e) {
      if (token !== state.selectToken) return;
      if (e && e.unsafeUrl) { UI.replace(els.previewState, UI.notice('fail', t('sessionUrlInvalid'))); return; }
      var code = e && e.body && typeof e.body.error === 'string' ? e.body.error : '';
      // The server's code decides: a missing or rejected key context is answered by the form;
      // an unavailable result or source is not a credentials problem.
      if (code === 'playback-credentials-required' || code === 'playback-credentials-invalid' || (!code && e && (e.status === 410 || e.status === 422))) {
        state.previewNeedsCredentials = true;
        if (typeof els.previewKid.focus === 'function') els.previewKid.focus();
        UI.replace(els.previewState, UI.notice(withCredentials ? 'fail' : 'info', t(withCredentials ? 'previewCredentialsRejected' : 'previewCredentialsHelp')));
      } else if (code === 'result-unavailable' || code === 'source-preview-unavailable' || code === 'result-not-found') {
        UI.replace(els.previewState, UI.notice('fail', t('resultUnavailable')));
      } else UI.replace(els.previewState, UI.notice('fail', App.I18n.error(e)));
    }).then(function () { state.previewBusy = false; if (token === state.selectToken) renderPlaybackControls(content); });
  }

  // ========================================================================================
  // DRM parameters (clear HLS)
  // ========================================================================================
  function validateDrmValue(key, value) {
    if (!value) return 'required';
    if (key === 'kid') return /^[a-f\d]{32}$/i.test(value.replace(/-/g, '')) ? null : 'hexError';
    if (key === 'key' || key === 'iv') return /^[a-f\d]{32}$/i.test(value) ? null : 'hexError';
    if (key === 'widevine_pssh') return (/^[A-Za-z\d+/]+={0,2}$/.test(value) && value.length % 4 === 0) || (/^[a-f\d]+$/i.test(value) && value.length % 2 === 0 && value.length >= 16) ? null : 'psshError';
    if (key === 'fairplay_uri') return /^skd:\/\/[^\s]+$/.test(value) ? null : 'fpsError';
    try { var url = new URL(value); return /^https?:$/.test(url.protocol) && !url.username && !url.password ? null : 'laUrlError'; } catch (_e) { return 'laUrlError'; }
  }
  function showFieldError(key) {
    var input = byId(DRM_FIELDS[key]);
    var error = validateDrmValue(key, input.value.trim());
    input.setAttribute('aria-invalid', String(!!error));
    UI.setText(byId(DRM_FIELDS[key] + '-error'), error ? t(error) : '');
    return !error;
  }
  function clearSecrets() {
    Object.keys(DRM_FIELDS).forEach(function (key) {
      var input = byId(DRM_FIELDS[key]);
      input.value = '';
      input.removeAttribute('aria-invalid');
      UI.setText(byId(DRM_FIELDS[key] + '-error'), '');
    });
    els.previewKid.value = '';
    els.previewKey.value = '';
    setReveal(false); // masked by default: the next entry never starts in clear text
  }
  // Synthetic sample values: they show the exact format of each parameter (32-hex KID/key/IV,
  // a base64 Widevine PSSH whose embedded KID matches, an https LA_URL, an skd:// URI). They are
  // test values, never real keys; the note beside the button says so.
  var SAMPLE_DRM = { kid: '000102030405060708090a0b0c0d0e0f', key: '10101010101010101010101010101010', iv: '20202020202020202020202020202020', widevine_pssh: 'AAAAMnBzc2gAAAAA7e+LqXnWSs6jyCfc1R0h7QAAABISEAABAgMEBQYHCAkKCwwNDg8=', playready_la_url: 'https://license.example.invalid/playready/rightsmanager.asmx', fairplay_uri: 'skd://sample-content-id' };
  function fillSampleDrm() {
    Object.keys(DRM_FIELDS).forEach(function (key) { var input = byId(DRM_FIELDS[key]); input.value = SAMPLE_DRM[key]; input.removeAttribute('aria-invalid'); UI.setText(byId(DRM_FIELDS[key] + '-error'), ''); });
    setReveal(true); state.sampleRevealed = true; // the point is to see the format; the first real edit masks again
    UI.replace(els.transformState, UI.notice('info', t('sampleValuesNote')));
    refreshControls();
  }
  function setReveal(reveal) {
    els.drmReveal.setAttribute('aria-pressed', String(reveal));
    UI.setText(els.drmReveal, t(reveal ? 'hideValues' : 'showValues'));
    // Masked text fields, never type="password": a password field makes the browser offer to save
    // the content key as a login the moment the form is cleared after the request (and would
    // autofill it next time). The mask is CSS (-webkit-text-security) on [data-masked="true"].
    Object.keys(DRM_FIELDS).forEach(function (key) { byId(DRM_FIELDS[key]).setAttribute('data-masked', reveal ? 'false' : 'true'); });
  }

  // ========================================================================================
  // Runs: launching, observing, finishing
  // ========================================================================================
  function refreshControls() {
    var running = isJobRunning();
    els.analyzeBtn.disabled = running || !validSource();
    els.hlsUrl.disabled = running; els.hlsAssetId.disabled = running;
    els.audioSelect.disabled = running;
    Object.keys(DRM_FIELDS).forEach(function (key) { byId(DRM_FIELDS[key]).disabled = running; });
    els.drmSample.disabled = running;
    els.packageBtn.disabled = running || !drmReady();
    var convertBtn = byId('convert-btn');
    var content = selected();
    if (convertBtn) convertBtn.disabled = running || !content || content.state === 'running' || content.state === 'queued';
    if (content) renderPlaybackControls(content);
  }
  function startRun(kind, assetId, jobId) {
    var run = { kind: kind, assetId: assetId, jobId: jobId || null, analysisId: null, startedAt: Date.now(), watcher: null, outcome: null, exitCode: null, elapsedMs: null, view: newLogView(jobId, assetId, kind, true) };
    state.run = run;
    state.lastRun = run;
    if (kind === 'convert') {
      state.catalog.forEach(function (row) { if (row.asset_id === assetId) row.output_readiness = { state: 'unverified', reason: 'generation-unconfirmed', job_id: jobId || null }; });
      recompose();
    }
    if (state.logView && !state.logView.live) closeLogView();
    if (state.selectedId === assetId) {
      state.logView = run.view;
      // The run rewrites this content's output: stop playing files that are being replaced.
      revokeSession();
      playback.hide();
      UI.replace(els.previewState);
    }
    startElapsedTimer();
    refreshControls();
    renderLease();
    renderLogPanel();
    return run;
  }
  function endRun(run) {
    if (run.watcher) { run.watcher.stop(); run.watcher = null; }
    if (state.run === run) state.run = null;
    run.elapsedMs = Date.now() - run.startedAt;
    stopElapsedTimer();
    refreshControls();
    renderLease();
  }
  function startElapsedTimer() {
    stopElapsedTimer();
    elapsedTimer = setInterval(function () { if (state.run) { renderLease(); if (state.logView === state.run.view && !els.logPanel.hidden) renderLogMeta(state.run.view); } }, 1000);
  }
  function stopElapsedTimer() { if (elapsedTimer) { clearInterval(elapsedTimer); elapsedTimer = null; } }
  function renderLease() {
    var run = state.run;
    els.lease.dataset.running = String(!!run);
    if (run) {
      // The live region carries only text that changes with the job (asset, kind, stage); the
      // ticking elapsed time sits in an aria-hidden span so screen readers are not spammed.
      UI.setText(els.leaseText, t('leaseRunning', { asset: run.assetId, kind: t('kind_' + run.kind) }) + (run.view.progress.lastRec ? ' · ' + stageText(run.view, false) : ''));
      var rec = run.view.progress.lastRec;
      var counter = rec && rec.stage === 'hls-source' ? sourceCounter(rec) : '';
      UI.setText(els.leaseElapsed, (counter ? '· ' + counter + ' ' : '') + '· ' + UI.fmtElapsed(Date.now() - run.startedAt, t));
      return;
    }
    var pending = (state.queue.pending || []).length;
    UI.setText(els.leaseText, pending ? t('leaseQueued', { n: pending }) : t('leaseIdle'));
    UI.setText(els.leaseElapsed, '');
  }
  function notify(assetId, kind, text) {
    if (state.selectedId === assetId) UI.replace(els.contentNotice, UI.notice(kind, text));
  }
  function watchRun(run, resume) {
    run.view.jobId = run.jobId; if (state.logView === run.view) renderLogPanel();
    var handlers = {
      onLine: function (line) { if (state.run === run) appendLine(run.view, line); },
      onTerminal: function (status, info) {
        if (state.run !== run) return;
        run.view.interrupted = run.view.interrupted || !!(info && info.logInterrupted);
        finishRun(run, status);
      },
      onLost: function (reason) {
        if (state.run !== run) return;
        if (run.jobId) state.finishedJobs[run.jobId] = true;
        run.outcome = 'lost'; run.view.outcome = 'lost'; run.view.live = false; run.view.notice = t(reason);
        endRun(run);
        notify(run.assetId, 'fail', t(reason));
        renderLogPanel();
        loadAll();
      },
      onWaiting: function (reason) {
        if (state.run !== run) return;
        if (reason === 'logDropped') run.view.interrupted = true;
        run.view.notice = t(reason);
        if (state.logView === run.view) renderLogMeta(run.view);
      },
    };
    run.watcher = resume ? App.Jobs.resume(run.jobId, handlers) : App.Jobs.watch(run.jobId, handlers);
  }
  function finishRun(run, status) {
    var success = status.state === 'succeeded';
    if (run.jobId) state.finishedJobs[run.jobId] = true;
    run.outcome = status.state; run.exitCode = status.exit_code;
    run.view.outcome = status.state; run.view.live = false; run.view.notice = '';
    settleProgress(run.view, success);
    endRun(run);
    if (state.logView === run.view) renderLogPanel();
    var elapsed = UI.fmtElapsed(run.elapsedMs, t);
    var refresh = loadAll();
    if (run.kind === 'convert') {
      refresh.then(function () {
        var completed = state.contents.filter(function (c) { return c.asset_id === run.assetId; })[0];
        var ready = success && Model.hasVerifiedOutput(completed);
        notify(run.assetId, !success ? 'fail' : ready ? 'good' : 'warn', !success ? t('convertFailed', { code: status.exit_code != null ? String(status.exit_code) : '—', elapsed: elapsed }) : t(ready ? 'convertDone' : 'convertUnverified', { elapsed: elapsed }));
        var content = selected();
        if (content && content.asset_id === run.assetId) { renderDetail(content); if (Model.hasCatalogReport(content)) loadCatalogReport(content, state.selectToken); }
      });
      return;
    }
    if (run.kind === 'analysis') {
      if (!success) notify(run.assetId, 'fail', t(status.state === 'interrupted' ? 'finding_analysis-interrupted' : 'analysisJobFailed'));
      else if (state.selectedId === run.assetId) UI.replace(els.contentNotice); // the input section states the verdict
      refresh.then(function () {
        var content = selected();
        if (!content || content.asset_id !== run.assetId) return;
        renderDetail(content);
        if (content.analysis_id) loadAnalysis(content, state.selectToken);
      });
      return;
    }
    if (run.kind === 'package') {
      if (state.selectedId === run.assetId) UI.replace(els.transformState); // the content notice states the outcome
      if (!success) { notify(run.assetId, 'fail', t(status.state === 'interrupted' ? 'packageInterrupted' : 'packageFailed') + ' ' + t('reenterDrm')); refresh.then(function () { var c = selected(); if (c && c.asset_id === run.assetId) renderDetail(c); }); return; }
      notify(run.assetId, 'good', t('packageDone', { elapsed: elapsed }));
      // The SSE terminal event only promises state/exit_code; the authoritative result id comes
      // from the job status, and the result body from its own endpoint.
      Promise.all([refresh, App.Api.getJobStatus(run.jobId)]).then(function (parts) {
        var content = selected();
        if (!content || content.asset_id !== run.assetId) return;
        var resultId = UI.safeId(parts[1].result_id || '');
        if (!resultId) throw { status: 404 };
        content.result_id = resultId;
        renderDetail(content);
        return loadResult(content, resultId, state.selectToken);
      }).catch(function (e) { var c = selected(); if (c && c.asset_id === run.assetId) { state.detail.resultError = e && e.status === 410 ? t('resultUnavailable') : App.I18n.error(e); renderReport(c); } });
    }
  }
  // A running job discovered from the records (reload, queue runner, another tab): observe it.
  function restoreRun(job) {
    if (isJobRunning() || !job || job.state !== 'running') return;
    var run = startRun(job.kind === 'analysis' ? 'analysis' : job.kind === 'package' ? 'package' : 'convert', job.asset_id || '', job.job_id);
    run.analysisId = job.analysis_id;
    var started = Date.parse(job.created_at);
    if (!isNaN(started)) run.startedAt = started;
    watchRun(run, true);
    renderLease();
  }

  // --- launchers -----------------------------------------------------------------------------
  function startConvert(content) {
    if (isJobRunning() || content.kind !== 'catalog') return;
    var run = startRun('convert', content.asset_id, null);
    notify(content.asset_id, 'info', t('convertStarting'));
    App.Api.postConvert(content.asset_id).then(function (res) {
      if (state.run !== run) return;
      run.jobId = res.job_id; run.view.jobId = res.job_id; if (state.logView === run.view) renderLogPanel();
      notify(content.asset_id, 'info', t('converting'));
      watchRun(run, false);
      loadAll();
    }, function (e) {
      if (state.run !== run) return;
      run.outcome = 'failed'; run.view.live = false;
      endRun(run);
      if (state.logView === run.view) closeLogView();
      notify(content.asset_id, 'fail', t('convertStartFailed') + ' ' + App.I18n.error(e));
      loadAll(); // A rejected launch may have left the earlier generation untouched.
    });
  }
  // The output name is derived from the URL (the last directory, or the playlist's name) and
  // kept unique against the known contents; the operator may edit it but never has to.
  // A path segment that carries a signed token (`bcdn_token=…&expires=…`, `hmac=…~exp=…`) or an
  // opaque id (a long run of letters and digits with no separator) never becomes the output
  // name, which lands in records, folders, tool paths and logs: such segments are skipped and
  // the host label stands in when nothing else is left. The name stays editable.
  function tokenLike(part) {
    if (/[=~&;,]/.test(part)) return true;
    if (/^[A-Za-z0-9_-]{16,}\.[A-Za-z0-9_-]{16,}/.test(part)) return true; // a JWT-shaped segment
    if (part.length >= 24 && /[a-z]/.test(part) && /[A-Z]/.test(part) && /\d/.test(part)) return true; // mixed-case opaque
    return part.length >= 20 && /\d/.test(part) && /[A-Za-z]/.test(part) && !/[.\-]/.test(part);
  }
  function suggestedName(url) {
    var base = '';
    try {
      var parsed = new URL(url);
      var parts = parsed.pathname.split('/').filter(Boolean).map(function (part) { try { return decodeURIComponent(part); } catch (_e) { return part; } });
      if (parts.length && /\.m3u8$/i.test(parts[parts.length - 1])) parts.pop();
      parts = parts.filter(function (part) { return !tokenLike(part); });
      var host = /^[\d.]+$|^\[/.test(parsed.hostname) ? parsed.hostname.replace(/[^A-Za-z0-9]+/g, '-').replace(/^-|-$/g, '') : parsed.hostname.split('.')[0];
      base = parts.length ? parts[parts.length - 1] : host;
      if (!parts.length || /^(master|index|playlist|manifest)$/i.test(base)) base = host + (parts.length ? '-' + parts[parts.length - 1] : '');
    } catch (_e) { return ''; }
    base = base.replace(/\.[^.]*$/, '').replace(/[^A-Za-z0-9_.-]+/g, '-').replace(/^[^A-Za-z0-9]+/, '').replace(/-+$/, '').slice(0, 60) || 'hls';
    var taken = Object.create(null);
    state.contents.forEach(function (c) { taken[c.asset_id] = true; });
    var candidate = base, n = 2;
    while (taken[candidate]) candidate = base + '-' + (n++);
    return candidate;
  }
  function syncSuggestedName() {
    if (state.nameEdited) return;
    var name = suggestedName(els.hlsUrl.value.trim());
    if (els.hlsAssetId.value !== name) els.hlsAssetId.value = name;
  }
  function validSource() {
    var url = els.hlsUrl.value.trim(), id = els.hlsAssetId.value.trim();
    try { var parsed = new URL(url); if (!/^https?:$/.test(parsed.protocol) || parsed.username || parsed.password) return false; } catch (_e) { return false; }
    return !!UI.safeId(id) && id !== '.' && id !== '..';
  }
  function catalogTree() {
    return ['<catalog_root>/', '  <entry>/', '    asset.json        {asset_id, label, mpd_path, hls_master_path, keys_file}', '    keys.env          DRM_KID= / DRM_KEY= / DRM_IV=', '    download/<host>/<path>/...   ' + t('convTreeMirror')].join('\n');
  }
  function openAdd() {
    state.addOpen = true;
    state.nameEdited = false;
    els.hlsUrl.value = ''; els.hlsAssetId.value = '';
    UI.setText(els.addCatalogTree, catalogTree());
    UI.replace(els.addRescanState);
    state.selectedId = null; state.selectToken += 1;
    revokeSession(); playback.hide(); clearSecrets();
    if (state.logView && !state.logView.live) closeLogView();
    UI.replace(els.addState);
    renderDetailVisibility();
    refreshControls();
    if (typeof els.addTitle.focus === 'function') els.addTitle.focus(); // the panel, not one of its two kinds
  }
  function closeAdd() { state.addOpen = false; renderDetailVisibility(); }
  function startAnalysis() {
    if (isJobRunning() || !validSource()) return;
    var url = els.hlsUrl.value.trim(), assetId = els.hlsAssetId.value.trim();
    var run = startRun('analysis', assetId, null);
    UI.replace(els.addState, h('p', { className: 'state-line', text: t('analysisStarting') }));
    App.Api.analyzeHls({ url: url, asset_id: assetId, access_profile: 'default' }).then(function (res) {
      if (state.run !== run) return;
      run.jobId = res.job_id; run.analysisId = res.analysis_id; run.view.jobId = res.job_id; if (state.logView === run.view) renderLogPanel();
      els.hlsUrl.value = ''; // Clear the submitted field; the server retains its private source analysis.
      watchRun(run, false);
      loadAll().then(function () { if (state.run === run || state.lastRun === run) { state.addOpen = false; selectContent(assetId); UI.replace(els.contentNotice, h('p', { className: 'state-line', text: t('analyzing') })); } });
    }, function (e) {
      if (state.run !== run) return;
      run.outcome = 'failed'; run.view.live = false;
      endRun(run);
      UI.replace(els.addState, UI.notice('fail', App.I18n.error(e)));
    });
  }
  function startPackage() {
    var content = selected();
    if (isJobRunning() || !content || content.kind !== 'hls') return;
    var valid = true;
    Object.keys(DRM_FIELDS).forEach(function (key) { if (!showFieldError(key)) valid = false; });
    if (!valid || !drmReady()) return;
    var analysis = state.detail.analysis;
    var drm = {};
    Object.keys(DRM_FIELDS).forEach(function (key) { drm[key] = byId(DRM_FIELDS[key]).value.trim(); });
    var run = startRun('package', content.asset_id, null);
    run.analysisId = analysis.analysis_id;
    var request = App.Api.packageHls({
      analysis_id: analysis.analysis_id,
      selection: { analysis_id: analysis.analysis_id, snapshot_digest: analysis.snapshot_digest, audio_source_id: audioSelectionRequired() ? els.audioSelect.value : null },
      drm: drm,
    });
    clearSecrets();
    drm = null;
    UI.replace(els.transformState, h('p', { className: 'state-line', text: t('packaging') }));
    request.then(function (res) {
      if (state.run !== run) return;
      run.jobId = res.job_id; run.view.jobId = res.job_id; if (state.logView === run.view) renderLogPanel();
      watchRun(run, false);
      loadAll();
    }, function (e) {
      if (state.run !== run) return;
      run.outcome = 'failed'; run.view.live = false;
      endRun(run);
      if (state.logView === run.view) closeLogView();
      var code = e && e.status === 409 && e.body ? e.body.error : null;
      var exists = code === 'result-already-exists' || code === 'result-already-published';
      UI.replace(els.transformState, UI.notice('fail', App.I18n.error(e) + (exists ? '' : ' ' + t('reenterDrm'))));
      refreshControls();
    });
  }

  // ========================================================================================
  // Settings and conventions sheet
  // ========================================================================================
  function openConventions() {
    var open = els.conventions.hidden;
    els.conventions.hidden = !open;
    els.conventionsBtn.setAttribute('aria-expanded', String(open));
    if (!open) { if (typeof els.conventionsBtn.focus === 'function') els.conventionsBtn.focus(); return; }
    if (typeof els.conventionsClose.focus === 'function') els.conventionsClose.focus();
    if (state.platform) { renderConventions(); return; }
    UI.replace(els.conventionsBody, h('p', { className: 'state-line', text: t('loading') }));
    App.Api.getPlatform().then(function (platform) { state.platform = platform; renderConventions(); }, function (e) { UI.replace(els.conventionsBody, UI.notice('fail', t('convLoadFailed') + ' ' + App.I18n.error(e))); });
  }
  function renderConventions() {
    var p = state.platform;
    if (!p) return;
    function safePath(value) { return typeof value === 'string' ? value : '—'; }
    var profiles = p.profiles === null ? [h('dd', { text: t('convProfilesNone') })] : (p.profiles || []).map(function (profile) {
      return h('dd', { className: 'mono', text: profile.name + ': ' + (profile.allowed_origins || []).join(', ') + (profile.allow_http ? ' (' + t('convAllowHttp') + ')' : '') });
    });
    var inputTree = catalogTree();
    var outputTree = [['<output_root>/<asset_id>/', ''], ['  cbcs/              ', 'served'], ['  manifests/         ', 'served'], ['  clear/             ', 'private'], ['  _work/             ', 'private'], ['  reports/           ', 'private'],
      ['    phase1-summary.json | package-hls-summary.json', ''], ['    job-<job_id>.json | hls-job-<job_id>.json', ''], ['<output_root>/_jobs/<job_id>.jsonl', 'private']];
    var command = ['drmr batch \\', '  --work-list <work-list.jsonl> \\', '  --output ' + safePath(p.output_root) + ' \\', '  --resume-log <resume.jsonl> \\', '  --storage local --storage-root <storage-root> \\',
      '  --key-provider file --key-file <keys.jsonl>     # ' + t('convCmdKeys'), '  --hls-config-dir <config-dir>                   # ' + t('convCmdHls')].join('\n');
    UI.replace(els.conventionsBody,
      h('h3', { text: t('convInstance') }),
      h('dl', { className: 'kv' }, h('dt', { text: t('convCatalogRoot') }), h('dd', { className: 'mono', text: safePath(p.catalog_root) }), h('dt', { text: t('convOutputRoot') }), h('dd', { className: 'mono', text: safePath(p.output_root) }),
        h('dt', { text: t('convJobsDir') }), h('dd', { className: 'mono', text: safePath(p.jobs_dir) }), h('dt', { text: t('convProfiles') }), profiles, h('dt', { text: t('convConcurrency') }), h('dd', { text: t('convConcurrencyValue') })),
      h('h3', { text: t('convInput') }), h('pre', { className: 'tree', text: inputTree }), h('p', { className: 'help', text: t('convInputNotes') }), h('p', { className: 'help', text: t('convClearHls') }),
      h('h3', { text: t('convOutput') }), h('pre', { className: 'tree' }, outputTree.map(function (row, i) { return [h('span', { text: row[0] }), row[1] ? h('span', { className: row[1], text: '  ' + t(row[1] === 'served' ? 'convServed' : 'convPrivate') }) : null, i < outputTree.length - 1 ? '\n' : null]; })), h('p', { className: 'help', text: t('convOutputNotes') }),
      h('h3', { text: t('convLogs') }), h('p', { className: 'help', text: t('convLogsNotes') }),
      h('h3', { text: t('convKeys') }),
      h('div', { className: 'table-wrap' }, h('table', { className: 'data' }, h('thead', null, h('tr', null, h('th', { text: t('convKeysPath') }), h('th', { text: t('convKeysHow') }))),
        h('tbody', null, [1, 2, 3, 4, 5].map(function (n) { return h('tr', null, h('td', { text: t('convKeysRow' + n + 'a') }), h('td', { text: t('convKeysRow' + n + 'b') })); })))),
      h('h3', { text: t('convBulk') }), h('p', { className: 'help', text: t('convBulkHelp') }), h('pre', { text: command }), h('p', { className: 'help', text: t('convBulkFormats') }), h('p', { className: 'help', text: t(p.batch_available ? 'batchAvailable' : 'batchUnavailable') }),
      h('h3', { text: t('convLimits') }), h('ul', { className: 'help' }, [1, 2, 3].map(function (n) { return h('li', { text: t('convLimit' + n) }); })));
  }

  // ========================================================================================
  // Boot
  // ========================================================================================
  function cacheElements() {
    ['lease', 'lease-text', 'lease-elapsed', 'list-truncated', 'add-btn', 'add-title', 'add-rescan-btn', 'add-rescan-state', 'add-conventions-btn', 'add-catalog-tree', 'conventions-btn', 'conventions', 'conventions-close', 'conventions-body', 'list-count', 'list-refresh', 'list-filter', 'list-error', 'bulk-bar', 'bulk-count', 'bulk-queue-btn', 'bulk-clear-btn', 'queue-pane', 'queue-list', 'queue-skipped', 'queue-notice', 'content-list',
      'detail-empty', 'hls-url', 'hls-asset-id', 'analyze-btn', 'add-cancel-btn', 'add-state', 'content', 'content-id', 'content-label', 'content-chips', 'content-actions', 'content-notice',
      'input-state', 'input-kv', 'input-findings', 'input-table', 'audio-choice', 'audio-source-select', 'transform-summary', 'transform-help', 'transform-rules', 'drm-form', 'drm-reveal', 'drm-sample', 'package-btn', 'transform-state',
      'runs-list', 'log-panel', 'log-title', 'log-chip', 'log-stage', 'log-count', 'log-close', 'log-json', 'log-download', 'log-notice', 'log-strips', 'log-list', 'report-state', 'report-view',
      'preview-btn', 'preview-credentials', 'preview-kid', 'preview-key', 'preview-state'].forEach(function (id) {
      els[id.replace(/-([a-z])/g, function (_, c) { return c.toUpperCase(); })] = byId(id);
    });
    els.addSection = byId('add-content'); els.audioSelect = byId('audio-source-select');
  }
  function bind() {
    els.addBtn.addEventListener('click', openAdd);
    els.addRescanBtn.addEventListener('click', function () {
      UI.replace(els.addRescanState, h('p', { className: 'state-line', text: t('loading') }));
      loadAll().then(function () { if (state.addOpen) UI.replace(els.addRescanState, h('p', { className: 'state-line', text: state.listError ? t('listLoadFailed') : t('rescanned', { n: state.contents.length }) })); });
    });
    els.addConventionsBtn.addEventListener('click', openConventions);
    els.addCancelBtn.addEventListener('click', closeAdd);
    els.conventionsBtn.addEventListener('click', openConventions);
    els.conventionsClose.addEventListener('click', openConventions);
    els.conventions.addEventListener('keydown', function (e) { if (e.key === 'Escape' && !els.conventions.hidden) openConventions(); });
    els.listRefresh.addEventListener('click', function () { loadAll(); });
    els.listFilter.addEventListener('input', function () { state.filter = els.listFilter.value; renderList(); });
    els.bulkQueueBtn.addEventListener('click', function () { var ids = Object.keys(state.checked); if (ids.length) enqueue(ids); });
    els.bulkClearBtn.addEventListener('click', function () { state.checked = Object.create(null); renderList(); });
    els.hlsUrl.addEventListener('input', function () { syncSuggestedName(); refreshControls(); });
    els.hlsAssetId.addEventListener('input', function () { state.nameEdited = els.hlsAssetId.value.trim() !== ''; if (!state.nameEdited) syncSuggestedName(); refreshControls(); });
    els.drmSample.addEventListener('click', fillSampleDrm);
    els.logJson.addEventListener('click', function () { state.logRaw = !state.logRaw; renderLogPanel(); });
    els.analyzeBtn.addEventListener('click', startAnalysis);
    els.addSection.addEventListener('keydown', function (e) { if (e.key === 'Enter' && e.target && String(e.target.tagName).toLowerCase() === 'input' && !els.analyzeBtn.disabled) { e.preventDefault(); startAnalysis(); } });
    Object.keys(DRM_FIELDS).forEach(function (key) {
      var input = byId(DRM_FIELDS[key]);
      input.addEventListener('input', function () {
        if (state.sampleRevealed) { state.sampleRevealed = false; setReveal(false); } // a real value typed over a sample is masked
        if (input.getAttribute('aria-invalid') === 'true') showFieldError(key);
        refreshControls();
      });
      input.addEventListener('blur', function () { if (input.value.trim()) showFieldError(key); });
    });
    els.audioSelect.addEventListener('change', refreshControls);
    els.drmForm.addEventListener('keydown', function (e) { if (e.key === 'Enter' && !els.packageBtn.disabled) { e.preventDefault(); startPackage(); } });
    els.packageBtn.addEventListener('click', startPackage);
    els.drmReveal.addEventListener('click', function () { state.sampleRevealed = false; setReveal(els.drmReveal.getAttribute('aria-pressed') !== 'true'); });
    els.logClose.addEventListener('click', closeLogView);
    els.previewBtn.addEventListener('click', function () { preparePreview(false); });
    els.previewCredentials.addEventListener('submit', function (e) { e.preventDefault(); preparePreview(true); });
    byId('language-select').addEventListener('change', function (e) { App.I18n.setLanguage(e.target.value); });
    document.addEventListener('workbench-language', function () {
      state.openInfo = {}; // the (i) panels close on a language change, as on a change of selection
      renderList(); renderQueue(); renderLease(); renderBulkBar();
      if (state.addOpen) UI.setText(els.addCatalogTree, catalogTree());
      var content = selected();
      if (content) renderDetail(content);
      setReveal(els.drmReveal.getAttribute('aria-pressed') === 'true');
      if (state.platform && !els.conventions.hidden) renderConventions();
      playback.rerender();
    });
    window.addEventListener('pagehide', function () { clearSecrets(); revokeSession(); });
  }
  function boot() {
    cacheElements();
    playback = App.Playback.create(byId);
    App.I18n.apply();
    bind();
    setReveal(false);
    renderLease();
    renderList();
    renderDetailVisibility();
    var params = {};
    try { new URL(location.href).searchParams.forEach(function (value, key) { params[key] = value; }); } catch (_e) { /* no deep link */ }
    loadAll().then(function () {
      var asset = /^[A-Za-z0-9_-][A-Za-z0-9._-]{0,255}$/.test(params.asset || '') ? params.asset : '';
      var result = UI.safeId(params.result || '');
      if (!asset && result) { var owner = state.contents.filter(function (c) { return c.result_id === result; })[0]; if (owner) asset = owner.asset_id; }
      if (asset && state.contents.some(function (c) { return c.asset_id === asset; })) selectContent(asset);
    });
  }
  document.addEventListener('DOMContentLoaded', boot);
})();
