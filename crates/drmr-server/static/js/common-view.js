/* Common workbench. Polling updates status nodes without replacing editing controls. */
(function () {
  'use strict';
  var App = window.App, UI = App.UI, h = UI.h;
  function t(key, values) { return App.I18n.t(key, values); }
  function word(tag, key, props) { return h(tag, Object.assign({ text: t(key), dataset: { i18n: key } }, props || {})); }
  function errorText(error) {
    return t(App.CommonReport.errorInfo(error).key);
  }
  function operationText(kind) { return t({ analyze: 'commonAnalyze', plan: 'commonSavePlan', convert: 'commonConvert', verify: 'commonVerify' }[kind] || 'commonUnknown'); }
  function ownerText(data) {
    var owner = data.owner;
    if (owner) {
      var status;
      if (owner.phase === 'server-changed') status = t('commonServerChanged');
      else if (owner.error && owner.error.code === 'cleanup-unconfirmed') status = t('commonCleanup');
      else if (owner.phase === 'unknown') status = t('commonUnknownOwner');
      else if (owner.stop === 'unknown') status = t('stopOutcomeUnknown');
      else if (owner.stop === 'sending') status = t('commonStopSending');
      else if (owner.stop === 'requested') status = t('commonStopping');
      else if (owner.stop === 'rejected') status = t('commonStopRejected');
      else status = owner.phase === 'pending' || owner.phase === 'submitting' ? t('commonPending') : t('commonRunning', { operation: operationText(owner.kind) });
      var item = data.sessions && data.sessions.get(owner.sessionId);
      return item ? t('commonOwnerLabel', { source: safeLabel(item.session.source), status: status }) : status;
    }
    if (data.creation) return t(data.creation.phase === 'unknown' ? 'commonCreationUnknown' : 'commonOpening');
    var execution = data.capabilities && data.capabilities.execution;
    return execution && execution.availability !== 'idle' ? t(execution.availability === 'closing' ? 'commonGlobalClosing' : execution.availability === 'cleanup-unconfirmed' ? 'commonCleanup' : 'commonGlobalBusy') : t('leaseIdle');
  }

  function safeLabel(source) {
    if (source.kind === 'url') return t('commonUrl');
    return source.kind === 'local' && source.label === 'Local input' ? t('commonLocalInput') : source.label;
  }
  function validName(name) { return typeof name === 'string' && /^[a-z][a-z0-9-]{0,31}$/.test(name) && !/^(con|prn|aux|nul|com[1-9]|lpt[1-9])$/.test(name); }
  function create(host, nav, global, model, hooks) {
    hooks = hooks || {};
    var drafts = new Map(), navRows = new Map(), draft = null, currentId = undefined, serverId = null, rows = [], reportViews = new Map(), rowSequence = 0, visible = true;
    var source = { media: null, plan: null, fetch: null };
    var picker = App.CommonFiles.create(host);
    function button(id, key, action, primary) { return word('button', key, { id: id, type: 'button', className: 'btn' + (primary ? ' btn-primary' : ''), 'data-common-action': id || key, onclick: action }); }
    function field(key, control, help) { return h('label', { className: 'field' }, word('span', key), control, help ? word('small', help) : null); }
    function options(values) { return values.map(function (value) { return typeof value === 'string' ? h('option', { value: value, text: value }) : word('option', value.key, { value: value.value }); }); }
    function choose(role, receive, key) {
      return button(null, key || 'commonPick', function () {
        var instance = model.snapshot().serverId, selected = model.snapshot().selectedId;
        picker.open(role, function (selection) { if (instance !== model.snapshot().serverId || selected !== model.snapshot().selectedId) return; receive(selection); update(); });
      });
    }
    function selectionField(key, role, read, write, help, privateFile) {
      var status = h('span', { className: 'help' });
      var pick = choose(role, function (value) { write(value); });
      pick.setAttribute('id', 'common-pick-' + role); pick.setAttribute('data-common-pick', role);
      pick.setAttribute('aria-labelledby', 'common-label-' + role + ' common-pick-' + role);
      var clear = button(null, 'commonClear', function () { write(null); update(); });
      var section = h('div', { className: 'field', role: 'group', 'data-common-role': role, 'aria-labelledby': 'common-label-' + role }, word('span', key, { id: 'common-label-' + role }), h('div', { className: 'actions' }, pick, clear, status), help ? word('small', help) : null);
      return { node: section, update: function () { var value = read(); UI.setText(status, value ? privateFile ? t('commonConfigured') : value.label : t('commonNotSelected')); clear.disabled = !value; pick.disabled = !model.snapshot().capabilities; } };
    }
    var title = word('h1', 'commonTitle', { id: 'common-title', tabindex: '-1' });
    var errorMessage = h('span'), errorCode = h('code');
    var error = h('div', { className: 'notice notice-fail', role: 'alert', hidden: true }, errorMessage, errorCode);
    var sourceKind = h('select', { id: 'common-source-kind', onchange: updateSource }, options([{ value: 'local', key: 'commonLocal' }, { value: 'url', key: 'commonUrl' }, { value: 'saved-plan', key: 'commonSavedPlan' }])); sourceKind.value = 'local';
    var media = selectionField('commonMedia', 'media', function () { return source.media; }, function (v) { source.media = v; }, 'commonFileHelp');
    var saved = selectionField('commonPlanFile', 'saved-plan', function () { return source.plan; }, function (v) { source.plan = v; }, 'commonFileHelp');
    var fetch = selectionField('commonFetch', 'fetch-policy', function () { return source.fetch; }, function (v) { source.fetch = v; }, 'commonFetchHelp', true);
    var url = h('input', { id: 'common-url', type: 'url', autocomplete: 'off', spellcheck: 'false', autocapitalize: 'off', autocorrect: 'off', maxlength: '16384', oninput: updateSource });
    var urlField = field('commonUrlLabel', url, 'commonUrlHelp');
    var open = button('common-open', 'commonOpen', function () {
      if (!validSource()) return;
      var value = sourceKind.value === 'local' ? { kind: 'local', input_ref: source.media.ref } : sourceKind.value === 'saved-plan' ? { kind: 'saved-plan', plan_file_ref: source.plan.ref } : { kind: 'url', url: url.value.trim(), fetch_policy_ref: source.fetch ? source.fetch.ref : null };
      url.value = ''; model.createSession(value); value = null; updateSource();
    }, true);
    var newInput = h('section', { className: 'common-section', 'aria-labelledby': 'common-input-title' },
      word('h2', 'commonInput', { id: 'common-input-title' }), field('commonSourceKind', sourceKind), media.node, saved.node, urlField, fetch.node, h('div', { className: 'actions' }, open));
    var sourceLabel = h('p', { className: 'common-source-label' });
    var close = button('common-close', 'commonSessionClose', function () { var item = model.selected(); if (item) Promise.resolve(model.close(item.session.session_id)).then(function () { if (!model.selected()) title.focus(); }); });
    var capacity = h('p', { className: 'help' });
    var currentInput = h('section', { className: 'common-section' }, sourceLabel, h('div', { className: 'actions' }, close), word('p', 'commonSessionCloseHelp', { className: 'help' }), capacity);
    var keys = selectionField('commonKeys', 'keys', function () { return draft && draft.keys; }, function (v) { if (draft) draft.keys = v; }, 'commonKeysHelp', true);
    var workers = h('input', { id: 'common-workers', type: 'number', min: '1', max: '64', step: '1', inputmode: 'numeric', oninput: function () { if (draft) draft.workers = workers.value; update(); } });
    var decode = h('input', { id: 'common-decode', type: 'checkbox', 'aria-describedby': 'common-decode-help', onchange: function () { if (draft) draft.decode = decode.checked; update(); } });
    var workerHelp = h('small', { className: 'help' });
    var runtime = h('section', { className: 'common-section' }, word('h2', 'commonRuntime'), keys.node, h('label', { className: 'field' }, word('span', 'commonWorkers'), workers, workerHelp), h('div', { className: 'field' }, h('label', { className: 'common-decode-option' }, decode, word('span', 'commonDecodeRequested')), word('small', 'commonDecodeHelp', { id: 'common-decode-help' })));
    var analyze = button('common-analyze', 'commonAnalyze', function () { if (!analyze.disabled) model.submit({ kind: 'analyze' }, runtimeValue()); }, true);
    var analysisReport = h('div', { id: 'common-analysis-report' });
    var analysis = h('section', { className: 'common-section', 'aria-labelledby': 'common-analysis-title' }, word('h2', 'commonAnalysis', { id: 'common-analysis-title' }), h('div', { className: 'actions' }, analyze), analysisReport);
    var outputBody = h('tbody');
    var outputTable = h('div', { className: 'common-output-wrap' }, h('table', { className: 'data common-outputs' },
      h('thead', null, h('tr', null, ['commonOutputId', 'commonFormat', 'commonProtection', 'commonDrms', 'commonConformance', 'commonActions'].map(function (key) { return word('th', key, { scope: 'col' }); }))), outputBody));
    var add = button('common-add-output', 'commonAddOutput', function () { if (!draft || rows.length >= 8) return; addRow({ id: 'output-' + (rows.length + 1), format: 'mp4', target: 'clear', drms: [], conformance: 'none' }); dirty(); });
    var parent = selectionField('commonOutputParent', 'output-parent', function () { return draft && draft.parent; }, function (v) { if (draft) { draft.parent = v; dirty(); } }, 'commonDestinationHelp');
    var destination = h('input', { id: 'common-destination', autocomplete: 'off', spellcheck: 'false', maxlength: '32', oninput: function () { if (draft) { draft.destination = destination.value; dirty(); } } });
    var outputError = h('p', { className: 'help' });
    var savePlan = button('common-save-plan', 'commonSavePlan', function () {
      var item = model.selected();
      if (!item || savePlan.disabled) return;
      draft.pendingSignature = signature();
      model.submit({ kind: 'plan', analysis_ref: item.analysis.analysis_ref, output_parent_ref: draft.parent.ref, destination_name: destination.value, outputs: outputValues() }, runtimeValue());
    }, true);
    var outputs = h('section', { className: 'common-section', 'aria-labelledby': 'common-outputs-title' },
      word('h2', 'commonOutputs', { id: 'common-outputs-title' }), word('p', 'commonOutputsHelp', { className: 'help' }),
      outputTable, h('div', { className: 'actions' }, add), word('p', 'commonOutputNameHelp', { id: 'common-output-name-help', className: 'help' }),
      parent.node, field('commonDestination', destination, 'commonDestinationHelp'), outputError, h('div', { className: 'actions' }, savePlan));
    var planSelect = h('select', { id: 'common-plan-select', onchange: function () { draft.dirty = false; model.usePlan(planSelect.value); update(); } });
    var planSelectField = field('commonPlanSelect', planSelect);
    var planInfo = h('dl', { className: 'kv' }), planOutputs = h('div'), planStatus = h('p', { className: 'help' }), dirtyNotice = word('p', 'commonPlanDirty', { className: 'notice notice-warn', hidden: true });
    var exportLink = word('a', 'commonExport', { id: 'common-export', className: 'btn', download: '', hidden: true });
    var plan = h('section', { className: 'common-section', 'aria-labelledby': 'common-plan-title' },
      word('h2', 'commonPlan', { id: 'common-plan-title' }), word('p', 'commonPlanHelp', { className: 'help' }), planSelectField, planInfo, planOutputs, planStatus, dirtyNotice, h('div', { className: 'actions' }, exportLink), word('p', 'commonExportHelp', { className: 'help' }));
    var convert = button('common-convert', 'commonConvert', function () { var item = model.selected(); if (item && item.plan && !convert.disabled) model.submit({ kind: 'convert', plan_ref: item.plan.plan_ref }, runtimeValue()); }, true);
    var convertReport = h('div', { id: 'common-conversion-report' });
    var conversion = h('section', { className: 'common-section', 'aria-labelledby': 'common-conversion-title' }, word('h2', 'commonConversion', { id: 'common-conversion-title' }), h('div', { className: 'actions' }, convert), convertReport);
    var verify = button('common-verify', 'commonVerify', function () { var item = model.selected(); if (item && item.plan && !verify.disabled) model.submit({ kind: 'verify', plan_ref: item.plan.plan_ref }, runtimeValue()); }, true);
    var verifyReport = h('div', { id: 'common-verification-report' });
    var verification = h('section', { className: 'common-section', 'aria-labelledby': 'common-verification-title' }, word('h2', 'commonVerification', { id: 'common-verification-title' }), word('p', 'commonVerifyHelp', { className: 'help' }), h('div', { className: 'actions' }, verify), verifyReport);
    var selectedStatus = h('p', { id: 'common-selected-status', className: 'state-line', role: 'status', 'aria-live': 'polite' });
    var selectedRecovery = button('common-result-refresh', 'commonCheckStatus', function () { model.recover(); });
    var selectedState = h('div', { className: 'common-section' }, selectedStatus, selectedRecovery);
    var content = h('div', { className: 'common-workbench' },
      h('header', { className: 'common-head' }, title, word('p', 'commonSteps', { className: 'common-steps' }), word('p', 'commonIntro', { className: 'help' }),
        word('a', 'commonGuide', { className: 'common-guide', href: 'https://kazuhide.github.io/drmr/guide/#cli-reference', target: '_blank', rel: 'noopener noreferrer' })),
      error, newInput, currentInput, runtime, analysis, outputs, plan, selectedState, conversion, verification);
    host.appendChild(content);
    var globalText = h('span'), globalActivity = h('span', { className: 'help' }), globalProgress = h('span', { className: 'help', 'aria-hidden': 'true' });
    var globalStop = button('common-global-stop', 'commonStop', function () { model.stop(); });
    var globalCheck = button('common-global-check', 'commonCheckStatus', function () { if (model.snapshot().creation) model.recoverCreation(); else model.recover(); });
    UI.replace(global, globalText, globalActivity, globalProgress, h('div', { className: 'actions' }, globalStop, globalCheck));
    function defaultDraft() { return { keys: null, workers: '', parent: null, destination: '', values: [{ id: 'output', format: 'mp4', target: 'clear', drms: [], conformance: 'none' }], dirty: false, planSeen: null, pendingSignature: null }; }
    function validSource() {
      if (sourceKind.value === 'local') return !!source.media;
      if (sourceKind.value === 'saved-plan') return !!source.plan;
      try { var value = new URL(url.value.trim()); return /^https?:$/.test(value.protocol) && !value.username && !value.password; } catch (_error) { return false; }
    }
    function updateSource() {
      media.node.hidden = sourceKind.value !== 'local'; saved.node.hidden = sourceKind.value !== 'saved-plan';
      urlField.hidden = sourceKind.value !== 'url'; fetch.node.hidden = sourceKind.value !== 'url';
      if (sourceKind.value !== 'url') url.value = '';
      open.disabled = !model.snapshot().capabilities || !!model.snapshot().creation || !validSource();
      media.update(); saved.update(); fetch.update();
    }
    function runtimeValue() { return { keys_ref: draft && draft.keys ? draft.keys.ref : null, max_workers: workers.value.trim() === '' ? null : Number(workers.value), decode: decode.checked }; }
    function validRuntime() { var value = runtimeValue().max_workers, cap = model.snapshot().capabilities; return value === null || Number.isInteger(value) && value >= 1 && value <= (cap ? cap.worker_policy.maximum : 0); }
    function outputValues() { return rows.map(function (row) { return { id: row.id.value, format: row.format.value, target: row.target.value, drms: row.drms.filter(function (drm) { return drm.control.checked; }).map(function (drm) { return drm.value; }), conformance: row.conformance.value }; }); }
    function signature() { return JSON.stringify({ parent: draft && draft.parent && draft.parent.ref, destination: destination.value, outputs: outputValues() }); }
    function validOutputs() {
      var values = outputValues(), names = new Set();
      return values.length >= 1 && values.length <= 8 && values.every(function (output) {
        if (!validName(output.id) || names.has(output.id)) return false;
        names.add(output.id); return true;
      });
    }
    function dirty() { if (!draft) return; draft.values = outputValues(); draft.dirty = true; model.invalidatePlan(); update(); }
    function addRow(value) {
      var serial = ++rowSequence, row = {}, cells = [];
      function control(key, element) {
        element.setAttribute('id', 'common-output-' + serial + '-' + key); element.setAttribute('data-common-field', key);
        element.addEventListener(key === 'id' ? 'input' : 'change', dirty);
        row[key] = element;
        var label = h('span', { className: 'common-cell-label' }), td = h('td', null, h('label', { className: 'field' }, label, element));
        cells.push({ node: td, label: label, key: { id: 'commonOutputId', format: 'commonFormat', target: 'commonProtection', conformance: 'commonConformance' }[key], control: element }); return td;
      }
      var id = h('input', { autocomplete: 'off', spellcheck: 'false', maxlength: '32', value: value.id, 'aria-describedby': 'common-output-name-help' });
      var format = h('select', null, options([{ value: 'mp4', key: 'commonFormatMp4' }, { value: 'dash', key: 'commonFormatDash' }, { value: 'hls', key: 'commonFormatHls' }, { value: 'hls-ts', key: 'commonFormatHlsTs' }])); format.value = value.format;
      var target = h('select', null, options(['clear', 'cenc', 'cbcs', 'hls-aes128', 'hls-sample-aes'])); target.value = value.target;
      var conformance = h('select', null, options([{ value: 'none', key: 'commonNone' }, { value: 'required', key: 'commonRequired' }])); conformance.value = value.conformance;
      row.drms = [{ value: 'widevine', label: 'Widevine' }, { value: 'playready', label: 'PlayReady' }, { value: 'fairplay', label: 'FairPlay' }].map(function (drm) {
        drm.control = h('input', { type: 'checkbox', 'data-common-drm': drm.value, checked: value.drms.indexOf(drm.value) !== -1, onchange: dirty });
        return drm;
      });
      var drmCell = h('td', null, word('span', 'commonDrms', { className: 'common-cell-label' }), h('div', { className: 'common-drm-choices' }, row.drms.map(function (drm) { return h('label', null, drm.control, h('span', { text: drm.label })); })));
      var remove = button(null, 'close', function () {
        if (rows.length <= 1) return;
        var index = rows.indexOf(row); rows.splice(index, 1); row.node.remove(); dirty(); rowLabels();
        var next = rows[Math.min(index, rows.length - 1)]; if (next) next.id.focus();
      });
      row.node = h('tr', { dataset: { outputRow: String(serial) } }, control('id', id), control('format', format), control('target', target), drmCell, control('conformance', conformance), h('td', null, remove));
      row.cells = cells; row.remove = remove;
      rows.push(row); outputBody.appendChild(row.node); rowLabels();
    }
    function rowLabels() {
      rows.forEach(function (row, index) {
        row.cells.forEach(function (cell) { UI.setText(cell.label, t(cell.key)); cell.control.setAttribute('aria-label', t('commonRowField', { n: index + 1, field: t(cell.key) })); });
        row.drms.forEach(function (drm) { drm.control.setAttribute('aria-label', t('commonRowField', { n: index + 1, field: drm.label })); });
        UI.setText(row.remove, t('commonRemoveOutput', { n: index + 1 })); row.remove.disabled = rows.length <= 1;
      });
      add.disabled = rows.length >= 8;
    }
    function selectDraft(item) {
      var next = item ? item.session.session_id : null;
      if (currentId === next) return;
      if (draft) draft.values = outputValues();
      currentId = next; rows = []; UI.replace(outputBody); reportViews.clear();
      if (!next) { draft = null; return; }
      if (!drafts.has(next)) drafts.set(next, defaultDraft());
      draft = drafts.get(next); draft.values.forEach(addRow); workers.value = draft.workers; decode.checked = draft.decode === true; destination.value = draft.destination;
    }
    function renderPlan(item) {
      var value = item && item.plan;
      var optionsKey = item ? Array.from(item.plans.keys()).join('|') : '';
      if (planSelect.dataset.options !== optionsKey) {
        planSelect.dataset.options = optionsKey;
        UI.replace(planSelect, item ? Array.from(item.plans.values()).map(function (entry) { return h('option', { value: entry.plan_ref, text: 'v' + entry.plan_schema_version + ' · ' + entry.plan_id.slice(0, 12) }); }) : []);
      }
      planSelect.value = value ? value.plan_ref : '';
      planSelectField.hidden = !item || !item.plans.size;
      if (draft && value && draft.planSeen !== value.plan_ref) {
        draft.planSeen = value.plan_ref;
        if (draft.pendingSignature === signature()) draft.dirty = false;
      }
      dirtyNotice.hidden = !draft || !draft.dirty;
      exportLink.hidden = !value;
      if (value) exportLink.setAttribute('href', value.export_url); else exportLink.removeAttribute('href');
      var key = value ? value.plan_ref + '|' + value.destination_state : '';
      if (planInfo.dataset.plan !== key) {
        planInfo.dataset.plan = key;
        UI.replace(planInfo, value ? [word('dt', 'commonPlanId'), h('dd', { className: 'mono', text: value.plan_id }), word('dt', 'commonPlanVersion'), h('dd', { text: 'v' + value.plan_schema_version }), word('dt', 'commonRequiredCoverage'), h('dd', null, word('span', value.required_depth === 'native' ? 'commonNativeCoverage' : 'commonDecodedCoverage'))] : []);
        UI.replace(planOutputs, value ? h('ul', { className: 'common-plan-outputs' }, value.outputs.map(function (output) {
          return h('li', null, output.id === null ? word('span', 'commonSingleOutput') : h('code', { text: output.id }), h('span', { text: output.format + ' · ' + output.target }), [2, 4].indexOf(value.plan_schema_version) !== -1 ? h('span', null, word('span', 'commonDrms'), ': ', (output.drms || []).length ? h('span', { text: output.drms.join(', ') }) : word('span', 'commonNotRequested')) : null, [2, 4].indexOf(value.plan_schema_version) !== -1 ? h('span', null, word('span', 'commonConformance'), ': ', word('span', output.conformance === 'required' ? 'commonRequired' : 'commonNotRequested')) : null);
        })) : null);
      }
      var destinationKey = value ? { absent: 'commonDestinationAbsent', present: 'commonDestinationPresent', mixed: 'commonDestinationMixed', unavailable: 'commonDestinationUnavailable' }[value.destination_state] : 'commonPlanNone';
      UI.setText(planStatus, t(destinationKey));
    }
    function renderReport(host, kind, item) {
      var report = item && item.reports && item.reports.get(kind);
      if (report && kind !== 'analyze' && (!item.plan || report.plan_ref !== item.plan.plan_ref)) report = null;
      var previous = reportViews.get(kind);
      if (!previous || previous.report !== report) reportViews.set(kind, { report: report, translate: App.CommonReport.render(host, report) });
    }
    function bindSelected(item) {
      var job = item && item.lastJob || {}, token = /^[A-Za-z0-9][A-Za-z0-9_-]{0,127}$/;
      [
        ['outcome', item && item.outcome, /^(succeeded|failed|interrupted|rejected|collecting|unavailable)$/],
        ['job-id', job.job_id, token], ['request-id', job.request_id, token],
        ['operation', job.operation, /^(analyze|plan|convert|verify)$/],
        ['job-state', job.state, /^(running|succeeded|failed|interrupted)$/],
        ['plan-id', job.plan_id, /^[a-f0-9]{64}$/], ['plan-ref', job.plan_ref, token]
      ].forEach(function (field) {
        var attribute = 'data-common-' + field[0], value = field[1];
        if (typeof value === 'string' && field[2].test(value)) selectedStatus.setAttribute(attribute, value); else selectedStatus.removeAttribute(attribute);
      });
    }
    function renderNav(data) {
      data.sessions.forEach(function (item, sid) {
        var row = navRows.get(sid);
        if (!row) {
          var name = h('span', { className: 'row-id' }), label = h('span', { className: 'row-label' });
          var button = h('button', { type: 'button', className: 'row-main', onclick: function () { if (hooks.show) hooks.show(); model.select(sid); update(); title.focus(); } }, name, label);
          row = { node: h('li', { className: 'content-row common-session-row' }, button), name: name, label: label };
          navRows.set(sid, row); nav.appendChild(row.node);
        }
        UI.setText(row.name, safeLabel(item.session.source));
        UI.setText(row.label, item.session.source.kind === 'saved-plan' ? t('commonSavedPlan') : t('commonInput'));
        row.node.setAttribute('aria-current', visible && data.selectedId === sid ? 'true' : 'false');
      });
      navRows.forEach(function (row, sid) { if (!data.sessions.has(sid)) { row.node.remove(); navRows.delete(sid); drafts.delete(sid); } });
    }
    function update() {
      var data = model.snapshot(), item = model.selected();
      if (serverId !== data.serverId) { if (serverId !== null) { drafts.clear(); source = { media: null, plan: null, fetch: null }; url.value = ''; picker.close(); currentId = undefined; } serverId = data.serverId; }
      if (data.capabilities) picker.update(data.capabilities);
      selectDraft(item); renderNav(data); updateSource();
      newInput.hidden = !!item;
      [currentInput, runtime, analysis, outputs, plan, selectedState, conversion, verification].forEach(function (node) { node.hidden = !item; });
      var problem = item && item.error || data.error;
      error.hidden = !problem; UI.setText(errorMessage, problem ? errorText(problem) : '');
      var info = App.CommonReport.errorInfo(problem); errorCode.hidden = !info.code; UI.setText(errorCode, info.code || '');
      bindSelected(item);
      if (item) {
        UI.setText(sourceLabel, safeLabel(item.session.source)); close.disabled = !!item.closing;
        UI.setText(capacity, t(item.session.requests_used >= 32 ? 'commonCapacityFull' : 'commonCapacity', { used: item.session.requests_used }));
        keys.update(); parent.update();
        var max = data.capabilities ? data.capabilities.worker_policy.maximum : 64;
        workers.setAttribute('max', String(max)); UI.setText(workerHelp, t('commonWorkersHelp', { max: max }));
        var imported = item.session.source.kind === 'saved-plan', can = model.canStart() && validRuntime();
        analyze.disabled = imported || !can; outputs.hidden = imported; analyze.hidden = imported;
        savePlan.disabled = imported || !can || !item.analysis || !draft.parent || !validName(destination.value) || !validOutputs();
        UI.setText(outputError, !item.analysis ? t('commonAnalyzeFirst') : draft.dirty && !validOutputs() ? t('commonInvalid') : '');
        renderPlan(item);
        convert.disabled = !can || !item.plan || item.plan.destination_state !== 'absent' || draft.dirty;
        verify.disabled = !can || !item.plan || item.plan.destination_state === 'absent' || item.plan.destination_state === 'unavailable';
        var owner = data.owner, text = owner && owner.sessionId === item.session.session_id ? ownerText(data) : owner ? t('commonOwnerElsewhere') : item.outcome ? t({ succeeded: 'commonSucceeded', failed: 'commonFailed', interrupted: 'commonInterrupted', rejected: 'commonRejected', collecting: 'commonCollecting', unavailable: 'commonArtifactUnavailable' }[item.outcome] || 'commonUnknown') : t('commonNoResult');
        UI.setText(selectedStatus, text);
        selectedRecovery.hidden = !owner && !item.lastJob; selectedRecovery.disabled = !!(owner && owner.checking);
      }
      renderReport(analysisReport, 'analyze', item); renderReport(convertReport, 'convert', item); renderReport(verifyReport, 'verify', item);
      var execution = data.capabilities && data.capabilities.execution;
      global.hidden = !data.owner && !data.creation && (!execution || execution.availability === 'idle');
      UI.setText(globalText, ownerText(data));
      globalStop.hidden = !data.owner || !data.owner.jobId || data.owner.phase === 'server-changed' || data.owner.stop === 'unknown';
      globalStop.disabled = !!(data.owner && (data.owner.stop === 'sending' || data.owner.stop === 'requested' || data.owner.checking));
      UI.setText(globalStop, t(data.owner && data.owner.stop === 'sending' ? 'commonStopSending' : data.owner && data.owner.stop === 'requested' ? 'commonStopping' : 'commonStop'));
      globalCheck.disabled = !!(data.owner && data.owner.checking || data.creation && data.creation.checking);
      var ownerItem = data.owner && data.sessions.get(data.owner.sessionId);
      var observed = ownerItem && ownerItem.lastJob && ownerItem.lastJob.job_id === data.owner.jobId ? ownerItem.lastJob.progress : null;
      UI.setText(globalActivity, observed && observed.last_activity ? t('commonActivity_' + observed.last_activity) : '');
      UI.setText(globalProgress, observed ? t('commonProgress', { seconds: Math.floor(observed.elapsed_ms / 1000), closed: observed.scopes_closed, started: observed.scopes_started, workers: observed.workers === null ? '—' : observed.workers }) : '');
      var guide = content.children[0].children[3];
      if (guide) guide.setAttribute('href', 'https://kazuhide.github.io/drmr/' + (App.I18n.getLanguage() === 'ja' ? 'ja/' : '') + 'guide/#cli-reference');
    }
    return { update: update,
      newInput: function () { url.value = ''; picker.close(); model.select(null); if (hooks.show) hooks.show(); visible = true; host.hidden = false; update(); title.focus(); },
      show: function () { visible = true; host.hidden = false; update(); },
      hide: function () { visible = false; host.hidden = true; url.value = ''; picker.close(); renderNav(model.snapshot()); },
      clearSecrets: function () { url.value = ''; picker.close(); },
      translate: function () { rowLabels(); picker.translate(); reportViews.forEach(function (view) { view.translate(); }); update(); },
    };
  }
  App.CommonView = { create: create, errorText: errorText, ownerText: ownerText, validName: validName };
})();
