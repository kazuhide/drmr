/* Report renderer shared by the content detail (console.js) and the stable report pages
 * (report.js): outcome, evidence, manifest links, the checks (each with its label and, behind
 * an (i), what it verifies), and the per-track sections. Segment tables are built only when
 * their disclosure is opened. */
(function () {
  'use strict';
  var App = (window.App = window.App || {});
  var UI = App.UI, h = UI.h;
  function t(key, values) { return App.I18n.t(key, values); }
  function code(value) { return UI.safeCode(value) || '—'; }
  function text(value) { return typeof value === 'string' && value.length ? value : '—'; }
  function num(value) { return typeof value === 'number' && isFinite(value) ? String(value) : '—'; }

  // Report links may only point at this origin's published outputs, never elsewhere.
  function outputUrl(value) {
    if (typeof value !== 'string' || value.indexOf('/output/') !== 0 || value.indexOf('\\') >= 0 || value.indexOf('%') >= 0) return null;
    try { var url = new URL(value, location.origin); return url.origin === location.origin && url.pathname.indexOf('/output/') === 0 && !url.search && !url.hash ? url.pathname : null; } catch (_e) { return null; }
  }
  var KNOWN_TRACK_CHECKS = ['selection-recorded', 'source-correspondence-pass', 'timing-pass', 'decoded-identity-pass', 'cbcs-signaling-pass', 'text-payload-pass', 'text-timing-map-pass'];

  function kv(pairs) {
    var dl = h('dl', { className: 'kv' });
    pairs.forEach(function (pair) { if (!pair) return; dl.appendChild(h('dt', { text: pair[0] })); dl.appendChild(h('dd', { className: pair[2] || null, text: pair[1] })); });
    return dl;
  }
  function statusOf(check) {
    if (typeof check === 'string') return KNOWN_TRACK_CHECKS.indexOf(check) >= 0 ? 'PASS' : 'UNKNOWN';
    if (check.status === 'PASS' || check.status === 'FAIL' || check.status === 'SKIP') return check.status;
    return check.passed === true ? 'PASS' : check.passed === false ? 'FAIL' : 'UNKNOWN';
  }
  function checkId(check) { return typeof check === 'string' ? check : (check.id || check.check_id); }
  // "<track>:<check>" ids (HLS results) are explained by their check part: `check_<id>` is the
  // label, `help_<id>` what the check verifies (shown behind an (i)). Unknown ids get neither.
  function checkPart(id) {
    var base = String(id || '');
    var colon = base.indexOf(':');
    return colon >= 0 ? base.slice(colon + 1) : base;
  }
  function explain(id) { var key = 'check_' + checkPart(id); return App.I18n.has(key) ? t(key) : null; }
  function help(id) { var key = 'help_' + checkPart(id); return App.I18n.has(key) ? t(key) : null; }
  function checkList(items) {
    return h('ul', { className: 'checks' }, (items || []).map(function (check) {
      var status = statusOf(check), id = checkId(check);
      var cls = status === 'PASS' ? 'check-pass' : status === 'FAIL' ? 'check-fail' : status === 'SKIP' ? 'check-skip' : 'check-unknown';
      var explanation = explain(id), what = help(id);
      var info = what ? UI.infoToggle(t('checkHelpLabel', { check: explanation || checkPart(id) }), what) : null;
      return h('li', { className: cls }, h('span', { className: 'check-status', text: t('check_' + status) }),
        h('div', { className: 'check-text' },
          h('span', { className: 'check-line' }, explanation ? h('span', { text: explanation }) : null, info ? info.button : null),
          h('code', { className: 'mono', text: code(id) }),
          info ? info.panel : null));
    }));
  }
  function segmentTable(rows) {
    var columns = ['source_index', 'output_index', 'timescale', 'presentation_start', 'duration', 'sample_count'];
    return h('div', { className: 'table-wrap' }, h('table', { className: 'data' },
      h('thead', null, h('tr', null, columns.map(function (k) { return h('th', { scope: 'col', text: t(k) }); }))),
      h('tbody', null, rows.map(function (row) { return h('tr', null, columns.map(function (k) { return h('td', { className: 'num', text: num(row[k]) }); })); }))));
  }

  // render(container, report, identity: {kind: 'hls'|'catalog', id}, options: {comparisonLink: bool})
  function render(container, report, identity, options) {
    options = options || {};
    var ready = identity.kind !== 'catalog' || !!(report.output_readiness && report.output_readiness.state === 'verified');
    var complete = identity.kind !== 'catalog' || (report.full_enumeration === true && report.round_trip_verified === true);
    var meta = [[t('reportIdentifier'), identity.id, 'mono']];
    if (report.created_at) meta.push([t('reportDate'), UI.fmtDate(report.created_at, App.I18n.getLanguage()) || '—']);
    if (identity.kind === 'catalog') {
      meta.push([t('reportFullEnumeration'), t(report.full_enumeration === true ? 'pass' : 'reportNotChecked')]);
      meta.push([t('reportRoundTrip'), t(report.round_trip_verified === true ? 'pass' : 'reportNotChecked')]);
    }
    // What the report is evidence of. Packaging gates are measured locally for both kinds (a
    // report that is not all-pass is a failed packaging); real-license playback and conformance
    // are separate gates this tool never claims. A result that states its own evidence wins.
    var evidence = report.evidence && typeof report.evidence === 'object' ? report.evidence : { packaging: report.all_pass === true ? 'measured-local' : 'failed', real_license: 'not-verified', conformance: 'not-verified' };
    var links = [];
    [['dash_url', 'dashManifest'], ['hls_url', 'hlsManifest']].forEach(function (pair) {
      var url = outputUrl(report[pair[0]]);
      if (ready && url && report.all_pass) links.push(h('a', { href: url, target: '_blank', rel: 'noopener', text: t(pair[1]) }));
    });
    if (options.comparisonLink && identity.kind === 'hls' && report.all_pass) links.push(h('a', { href: '/?result=' + encodeURIComponent(identity.id), text: t('reportComparison') }));
    if (options.pageLink) links.push(h('a', { href: '/reports/' + identity.kind + '/' + encodeURIComponent(identity.id), text: t('openReportPage') }));
    var rows = identity.kind === 'hls' ? (report.track_reports || []) : (report.tracks || []);
    UI.replace(container,
      UI.notice(!report.all_pass ? 'fail' : ready && complete ? 'good' : 'warn', t(!report.all_pass ? 'checksFail' : !ready ? 'outputUnverified' : complete ? 'checksPass' : 'reportIncomplete')),
      !ready ? UI.notice('warn', t('reportDiagnostic')) : null,
      kv(meta),
      h('div', { className: 'evidence' }, h('span', { className: 'muted', text: t('reportEvidence') }), ['packaging', 'real_license', 'conformance'].map(function (k) {
        var v = UI.safeCode(evidence[k]) || 'not-verified';
        return UI.chip(v === 'measured-local' ? 'good' : v === 'failed' ? 'fail' : 'warn', t('evidence_' + k) + ': ' + (App.I18n.has('ev_' + v) ? t('ev_' + v) : v));
      })),
      links.length ? h('div', { className: 'links' }, links) : null,
      h('h3', { text: t('reportChecks') }), checkList(report.checks),
      rows.length ? h('h3', { text: t('reportTracks') }) : null,
      rows.map(function (track) {
        var pairs = [];
        if (track.disposition) pairs.push([t('reportDisposition'), code(track.disposition)]);
        if (identity.kind === 'hls') {
          var source = (report.tracks || []).filter(function (item) { return (item.id || item.rep_id) === track.source_track_id; })[0];
          if (source) {
            if (source.content_type) pairs.push([t('kind'), App.I18n.has('type_' + source.content_type) ? t('type_' + source.content_type) : code(source.content_type)]);
            if (source.codec) pairs.push([t('codec'), code(source.codec), 'mono']);
            if (source.container) pairs.push([t('container'), code(source.container)]);
            if (source.language) pairs.push([t('languageCol'), code(source.language)]);
            if (source.width && source.height) pairs.push([t('reportResolution'), num(source.width) + ' × ' + num(source.height)]);
            if (typeof source.bandwidth === 'number') pairs.push([t('reportBandwidth'), num(source.bandwidth)]);
            if (typeof source.segment_count === 'number') pairs.push([t('reportSourceCount'), num(source.segment_count)]);
            if (typeof source.duration === 'number') pairs.push([t('reportDuration'), num(source.duration)]);
          }
        } else {
          pairs.push([t('kind'), App.I18n.has('type_' + track.content_type) ? t('type_' + track.content_type) : code(track.content_type)], [t('reportSourceCount'), num(track.source_segment_count)], [t('reportOutputCount'), num(track.output_segment_count)], [t('reportSegmentDuration'), num(track.requested_segment_duration_s)]);
        }
        var section = h('section', { className: 'report-track' }, h('h4', { className: 'mono', text: text(track.source_track_id || track.rep_id) }), kv(pairs), checkList(track.checks));
        if (track.segments && track.segments.length) {
          var body = h('div', { className: 'disclosure-body' });
          var details = h('details', { className: 'disclosure' }, h('summary', { text: t('reportSegments') + ' (' + track.segments.length + ')' }), body);
          var rendered = false; // hundreds of rows: build only when opened
          details.addEventListener('toggle', function () { if (details.open && !rendered) { rendered = true; body.appendChild(segmentTable(track.segments)); } });
          section.appendChild(details);
        }
        return section;
      }));
  }

  App.ReportView = { render: render, checkList: checkList, outputUrl: outputUrl };
})();
