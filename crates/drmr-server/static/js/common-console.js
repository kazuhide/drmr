/* Shared console composition; polling observes only, never retries a mutation. */
(function () {
  'use strict';
  var App = window.App, model = null, view = null, timer = null, paused = false, observing = false;
  function changed() {
    if (paused || !view) return;
    view.update();
    if (App.ConsoleBridge) App.ConsoleBridge.commonChanged();
  }
  async function observe() {
    if (paused || observing || !model) return;
    observing = true;
    try {
      await model.refresh();
      if (!paused && model.snapshot().owner) await model.recover();
      if (!paused && model.snapshot().creation && model.snapshot().creation.phase === 'unknown') await model.recoverCreation();
    } finally {
      observing = false;
      if (!paused) timer = setTimeout(observe, 3000);
    }
  }
  function show() {
    if (!view) return;
    if (App.ConsoleBridge) App.ConsoleBridge.showCommon();
    view.show();
  }
  function boot() {
    var host = document.getElementById('common-workflow');
    if (!host) return;
    model = App.CommonState.create({ onChange: changed, onObserverError: function () { console.error('common-view-unavailable'); }, legacyBusy: function () { return !!(App.ConsoleBridge && App.ConsoleBridge.ownsExecution()); } });
    view = App.CommonView.create(host, document.getElementById('common-sessions'), document.getElementById('common-owner'), model, { show: show });
    document.getElementById('common-new').addEventListener('click', function () { view.newInput(); });
    document.addEventListener('workbench-language', function () { if (view) view.translate(); });
    window.addEventListener('pagehide', function () {
      paused = true;
      if (timer !== null) clearTimeout(timer);
      timer = null; view.clearSecrets();
    });
    window.addEventListener('pageshow', function (event) {
      if (!event.persisted) return;
      paused = false; changed(); observe();
    });
    show(); observe();
  }
  App.CommonConsole = {
    isBusy: function () { var data = model && model.snapshot(); return !!(data && (data.owner || data.creation || data.capabilities && data.capabilities.execution.availability !== 'idle')); },
    leaseText: function () { return model ? App.CommonView.ownerText(model.snapshot()) : ''; },
    hide: function () { if (view) view.hide(); },
    show: show,
  };
  document.addEventListener('DOMContentLoaded', boot);
})();
