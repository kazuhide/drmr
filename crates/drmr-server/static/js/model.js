/* Content state, transformation rules and report checks. */
(function () {
  'use strict';
  var App = (window.App = window.App || {});

  var TERMINAL = { succeeded: true, failed: true, interrupted: true };
  var CATALOG_RULES = [
    { key: 'noReencode', checks: ['6-4.framemd5', '6-2.audio-pcm-identity'] },
    { key: 'scheme', checks: ['6-1.dash-scheme-cbcs'] },
    { key: 'boundaries', checks: ['6-1.segment-duration', '6-1.boundary-agreement', '6-1.number-origin'] },
    { key: 'iv', checks: ['6-1.iv-agreement', '6-1.hls-iv-agreement'] },
    { key: 'playready', checks: ['6-1.pssh-agreement', '6-1.dash-playready-sync'] },
    { key: 'widevineFairplay', checks: ['6-1.dash-widevine-unchanged'] },
    { key: 'dashPatch', checks: ['6-1.dash-segmenttemplate-unchanged'] },
    { key: 'hlsPatch', checks: ['6-1.hls-segment-count', '6-1.hls-version-unchanged', '6-2.audio-consolidation-structure', '6-1.hls-content-gate', '6-1.hls-extinf'] },
    { key: 'outOfScope', checks: ['6-5.dash-out-of-scope-untouched', '6-5.hls-master-untouched'] },
    { key: 'conformance', checks: ['6-6.dash-conformance'] },
  ];
  var HLS_RULES = [
    { key: 'hlsNoReencode', checks: ['source-correspondence-pass', 'timing-pass', 'decoded-identity-pass'] },
    { key: 'hlsScheme', checks: ['cbcs-signaling-pass'] },
    { key: 'hlsSignaling', checks: [] },
    { key: 'hlsManifests', checks: [] },
    { key: 'hlsSubtitles', checks: ['text-payload-pass', 'text-timing-map-pass'] },
    { key: 'hlsAudioChoice', checks: ['selection-recorded'] },
    { key: 'hlsEvidence', checks: [] },
  ];

  function byNewest(a, b) {
    var ta = String(a.created_at || ''), tb = String(b.created_at || '');
    return ta < tb ? 1 : ta > tb ? -1 : (a.job_id < b.job_id ? 1 : -1);
  }

  // compose(catalogRows, jobRecords, queueSnapshot) -> [Content] sorted by asset id.
  function compose(catalog, jobs, queue) {
    var byAsset = Object.create(null);
    (jobs || []).forEach(function (job) {
      if (!job || typeof job.asset_id !== 'string') return;
      (byAsset[job.asset_id] = byAsset[job.asset_id] || []).push(job);
    });
    Object.keys(byAsset).forEach(function (id) { byAsset[id].sort(byNewest); });
    var contents = [];
    var seen = Object.create(null);
    (catalog || []).forEach(function (row) {
      if (!row || typeof row.asset_id !== 'string') return;
      seen[row.asset_id] = true;
      contents.push(makeContent('catalog', row.asset_id, row, byAsset[row.asset_id] || []));
    });
    Object.keys(byAsset).forEach(function (id) {
      if (seen[id]) return;
      var hlsJobs = byAsset[id].filter(function (job) { return job.kind === 'analysis' || job.kind === 'package'; });
      if (hlsJobs.length) { contents.push(makeContent('hls', id, null, hlsJobs)); return; }
      // Preserve runs and logs when a catalog entry has been removed.
      var orphan = makeContent('catalog', id, null, byAsset[id]);
      orphan.orphan = true;
      contents.push(orphan);
    });
    contents.sort(function (a, b) { return a.asset_id < b.asset_id ? -1 : a.asset_id > b.asset_id ? 1 : 0; });
    contents.forEach(function (content) { content.state = contentState(content, queue); });
    return contents;
  }

  function makeContent(kind, assetId, row, jobs) {
    var content = { kind: kind, asset_id: assetId, label: row ? (row.label || '') : '', entry: row ? row.entry : null, paths: row ? row.paths : null, converted: !!(row && row.converted), jobs: jobs, analysis_id: null, result_id: null, state: 'ready' };
    jobs.forEach(function (job) {
      if (job.kind === 'package' && job.state === 'succeeded' && !content.result_id && job.result_id) content.result_id = job.result_id;
      if (job.kind === 'analysis' && job.state === 'succeeded' && !content.analysis_id && job.analysis_id) content.analysis_id = job.analysis_id;
    });
    // a package whose analysis is known only through the package record
    if (!content.analysis_id) jobs.forEach(function (job) { if (job.kind === 'package' && job.analysis_id && !content.analysis_id) content.analysis_id = job.analysis_id; });
    return content;
  }

  function contentState(content, queue) {
    var jobs = content.jobs || [];
    if (jobs.some(function (job) { return job.state === 'running'; })) return 'running';
    if (content.orphan) return 'orphan';
    if (queue && (queue.pending || []).some(function (item) { return item.asset_id === content.asset_id; })) return 'queued';
    // A failed recent run takes precedence over an earlier success.
    var newest = jobs[0];
    if (newest && (newest.state === 'failed' || newest.state === 'interrupted')) return 'failed';
    if (content.kind === 'catalog' ? content.converted : !!content.result_id) return 'packaged';
    if (content.kind === 'hls' && content.analysis_id) return 'analyzed';
    return 'ready';
  }

  function rules(kind) { return kind === 'catalog' ? CATALOG_RULES : HLS_RULES; }

  // Aggregate a report's checks by id: manifest-level checks keep their status, per-track checks
  // ("<track>:<check>" for HLS results; per-rendition `checks` arrays for catalog reports) are
  // combined -- FAIL if any failed, else SKIP if all skipped, else PASS if any passed, else UNKNOWN.
  function checkStatuses(report, kind) {
    var buckets = Object.create(null);
    function add(id, status) {
      id = String(id || '');
      var colon = kind === 'hls' ? id.indexOf(':') : -1;
      if (colon >= 0) id = id.slice(colon + 1);
      if (!id) return;
      (buckets[id] = buckets[id] || []).push(normalize(status));
    }
    if (report) {
      (report.checks || []).forEach(function (check) { add(check.id || check.check_id, check.status); });
      (report.tracks || []).forEach(function (track) { (track.checks || []).forEach(function (check) { add(typeof check === 'string' ? check : (check.id || check.check_id), typeof check === 'string' ? 'PASS' : check.status); }); });
      (report.track_reports || []).forEach(function (track) { (track.checks || []).forEach(function (check) { add(check.id, check.status); }); });
    }
    var out = Object.create(null);
    Object.keys(buckets).forEach(function (id) {
      var list = buckets[id];
      out[id] = list.indexOf('FAIL') >= 0 ? 'FAIL' : list.every(function (x) { return x === 'SKIP'; }) ? 'SKIP' : list.indexOf('PASS') >= 0 ? 'PASS' : 'UNKNOWN';
    });
    return out;
  }
  function normalize(status) {
    if (status === true) return 'PASS';
    if (status === false) return 'FAIL';
    var s = String(status || '').toUpperCase();
    return s === 'PASS' || s === 'FAIL' || s === 'SKIP' ? s : 'UNKNOWN';
  }

  // A rule's overall status from its checks: FAIL > SKIP (all skipped) > PASS > none.
  function ruleStatus(rule, statuses) {
    var found = rule.checks.map(function (id) { return statuses[id]; }).filter(Boolean);
    if (!found.length) return null;
    if (found.indexOf('FAIL') >= 0) return 'FAIL';
    if (found.every(function (x) { return x === 'SKIP'; })) return 'SKIP';
    if (found.indexOf('PASS') >= 0) return 'PASS';
    return 'UNKNOWN';
  }

  function isTerminal(state) { return !!TERMINAL[state]; }

  App.Model = { compose: compose, contentState: contentState, rules: rules, checkStatuses: checkStatuses, ruleStatus: ruleStatus, isTerminal: isTerminal, byNewest: byNewest };
})();
