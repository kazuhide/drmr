/* Role-bound browsing of operator/native allowed folders; never reads file contents. */
(function () {
  'use strict';
  var App = window.App, UI = App.UI, h = UI.h;
  function t(key) { return App.I18n.t(key); }
  function text(tag, key, props) { return h(tag, Object.assign({ text: t(key), dataset: { i18n: key } }, props || {})); }
  function create(host) {
    var caps = null, role = null, callback = null, roots = [], root = null, stack = [], cursor = null, parent = null, generation = 0, listingVersion = 0, invoker = null;
    var title = text('h2', 'commonPicker', { id: 'common-picker-title' });
    var rootSelect = h('select', { id: 'common-picker-root', onchange: function () {
      root = roots.filter(function (value) { return value.root_ref === rootSelect.value; })[0] || null;
      stack = []; load(false);
    } });
    var up = text('button', 'commonUp', { type: 'button', className: 'btn btn-sm', onclick: function () { if (stack.length) { stack.pop(); load(false); } } });
    var path = h('p', { className: 'help common-picker-path' });
    var state = h('p', { className: 'state-line', role: 'status', 'aria-live': 'polite' });
    var list = h('ul', { className: 'common-files' });
    var more = text('button', 'commonMore', { type: 'button', className: 'btn btn-sm', hidden: true, onclick: function () { load(true); } });
    var chooseFolder = text('button', 'commonChooseFolder', { id: 'common-picker-choose-folder', type: 'button', className: 'btn btn-primary', hidden: true, onclick: function () { if (parent) choose({ ref: parent, role: role, label: folderLabel() }, listingVersion); } });
    var cancel = text('button', 'close', { type: 'button', className: 'btn', onclick: close });
    var dialog = h('dialog', { className: 'common-picker', 'aria-labelledby': 'common-picker-title' },
      h('div', { className: 'sheet-head' }, title, cancel),
      h('label', { className: 'field' }, text('span', 'commonRoot'), rootSelect),
      h('div', { className: 'actions' }, up, path), state, list, more,
      h('div', { className: 'actions' }, chooseFolder));
    host.appendChild(dialog);
    dialog.addEventListener('cancel', function () { generation++; listingVersion++; callback = null; });
    dialog.addEventListener('close', function () { if (!dialog.open) { generation++; listingVersion++; callback = null; } });
    function folderLabel() { return (root ? root.label : '') + stack.map(function (part) { return ' / ' + part.label; }).join(''); }
    function close() {
      generation++; listingVersion++; callback = null;
      if (dialog.open && typeof dialog.close === 'function') dialog.close();
      if (invoker && typeof invoker.focus === 'function') invoker.focus();
    }
    function choose(value, turn) {
      if (turn !== listingVersion || !callback || !caps) return;
      var deliver = callback; close(); deliver(value);
    }
    async function load(append) {
      var turn = ++generation, version = append ? listingVersion : ++listingVersion, server = caps && caps.server_id;
      var next = append ? cursor : null;
      parent = null; cursor = null; chooseFolder.disabled = true; more.disabled = true; up.disabled = !stack.length;
      UI.setText(path, folderLabel());
      if (!append) UI.replace(list);
      UI.setText(state, t(root ? 'commonPickerLoading' : 'commonNoRoots'));
      if (!root) return;
      var directory = stack.length ? stack[stack.length - 1].ref : null;
      try {
        var page = await App.CommonApi.files({ root_ref: root.root_ref, role: role, directory_ref: directory, cursor: next });
        if (turn !== generation || !caps || caps.server_id !== server) return;
        if (page.server_id !== server) throw new Error('response-binding-invalid');
        parent = page.output_parent_ref; cursor = page.next_cursor;
        var count = 0;
        page.entries.forEach(function (entry) {
          if (entry.kind === 'file' && role === 'output-parent') return;
          count++;
          var button = h('button', { type: 'button', className: 'common-file', text: entry.label, 'data-common-entry-kind': entry.kind,
            onclick: function () {
              if (version !== listingVersion) return;
              if (entry.kind === 'directory') { stack.push({ ref: entry.directory_ref, label: entry.label }); load(false); }
              else choose({ ref: entry.file_ref, role: role, label: entry.label }, version);
            } });
          list.appendChild(h('li', null, h('span', { className: 'common-file-kind', 'aria-hidden': 'true', text: entry.kind === 'directory' ? '▸' : '·' }), button));
        });
        UI.setText(state, !count && !append ? t('commonFolderEmpty') : '');
        chooseFolder.disabled = !parent;
        more.hidden = !cursor; more.disabled = !cursor;
      } catch (_error) {
        if (turn === generation) { UI.setText(state, t('commonPickerError')); more.hidden = true; }
      }
    }
    return {
      update: function (value) {
        if (caps && caps.server_id !== value.server_id) close();
        caps = value;
      },
      open: function (nextRole, deliver) {
        generation++; callback = deliver; role = nextRole; dialog.setAttribute('data-common-role', role); stack = []; invoker = document.activeElement;
        roots = caps ? caps.roots.filter(function (entry) { return entry.roles.indexOf(role) !== -1; }) : [];
        root = roots[0] || null;
        UI.replace(rootSelect, roots.map(function (entry) { return h('option', { value: entry.root_ref, text: entry.label }); }));
        rootSelect.value = root ? root.root_ref : ''; rootSelect.disabled = !root;
        chooseFolder.hidden = role !== 'output-parent';
        if (!dialog.open && typeof dialog.showModal === 'function') dialog.showModal();
        return load(false);
      },
      close: close,
      translate: function () { UI.setText(path, folderLabel()); if (!root) UI.setText(state, t('commonNoRoots')); },
    };
  }
  App.CommonFiles = { create: create };
})();
