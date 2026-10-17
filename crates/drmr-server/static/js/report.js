/* Read-only report page (/reports/hls/<result_id>, /reports/catalog/<asset_id>): no playback
 * session, no key input, no conversion request. Renders the saved result through the shared
 * report view (reportview.js). */
(function () {
  'use strict';
  var App = window.App, UI = App.UI, h = UI.h;
  var report = null, token = 0, identity = null, lastError = null, loading = false;
  function t(key, values) { return App.I18n.t(key, values); }
  function el(id) { return document.getElementById(id); }

  function render() {
    if (!report) return;
    document.title = t('reportTitle') + ' — ' + (typeof report.asset_id === 'string' ? report.asset_id : '—');
    el('report-content').hidden = false;
    UI.replace(el('report-state'));
    UI.setText(el('report-asset'), report.asset_id || '—');
    App.ReportView.render(el('report-view'), report, identity, { comparisonLink: true });
  }

  function errorText(error) { return error && error.status === 404 ? t('reportNotFound') : error && error.status === 410 ? t('reportUnavailable') : App.I18n.error(error); }
  function renderState() {
    if (report) { render(); return; }
    el('report-content').hidden = true;
    if (!identity) UI.replace(el('report-state'), UI.notice('fail', t('reportNotFound')));
    else if (loading) UI.replace(el('report-state'), h('p', { className: 'state-line', text: t('reportLoading') }));
    else if (lastError) UI.replace(el('report-state'), UI.notice('fail', errorText(lastError)));
  }
  function load() {
    var current = ++token;
    loading = true; lastError = null;
    renderState();
    el('report-reload').disabled = true;
    if (!identity) { loading = false; el('report-reload').disabled = false; return; }
    var request = identity.kind === 'hls' ? App.Api.getHlsResult(identity.id) : App.Api.getCatalogReport(identity.id);
    request.then(function (value) { if (current !== token) return; report = value; loading = false; render(); },
      function (error) { if (current !== token) return; report = null; loading = false; lastError = error || { status: 0 }; renderState(); })
      .then(function () { if (current === token) el('report-reload').disabled = false; });
  }

  document.addEventListener('DOMContentLoaded', function () {
    var match = /^\/reports\/hls\/([A-Za-z0-9][A-Za-z0-9_.-]{0,127})$/.exec(location.pathname);
    if (match) identity = { kind: 'hls', id: match[1] };
    else { match = /^\/reports\/catalog\/([A-Za-z0-9_-][A-Za-z0-9._-]*)$/.exec(location.pathname); if (match) identity = { kind: 'catalog', id: match[1] }; }
    App.I18n.apply();
    document.title = t('reportTitle');
    el('language-select').addEventListener('change', function (event) { App.I18n.setLanguage(event.target.value); });
    el('report-reload').addEventListener('click', load);
    document.addEventListener('workbench-language', function () { document.title = t('reportTitle'); renderState(); });
    load();
  });
})();
