/* Shared UI toolkit for the console and the report page: a safe DOM builder (text only,
 * never innerHTML with data), formatters, chips, notices and the per-track progress strips.
 * No framework, no build step; loaded as a classic script before the page modules. */
(function () {
  'use strict';
  var App = (window.App = window.App || {});

  // h(tag, props, ...children): props may carry className, attributes, dataset (data-*),
  // handlers (onclick -> addEventListener('click')), and `text`. Children: strings, nodes,
  // arrays, or null/false (skipped). Every string becomes a text node -- no markup injection.
  function h(tag, props) {
    var el = document.createElement(tag);
    if (props) {
      Object.keys(props).forEach(function (key) {
        var value = props[key];
        if (value == null || value === false) return;
        if (key === 'className') el.className = value;
        else if (key === 'text') el.textContent = String(value);
        else if (key === 'dataset') Object.keys(value).forEach(function (k) { el.dataset[k] = String(value[k]); });
        else if (key.indexOf('on') === 0 && typeof value === 'function') el.addEventListener(key.slice(2), value);
        else if (key === 'value') el.value = value;
        else if (key === 'checked' || key === 'disabled' || key === 'hidden' || key === 'open') el[key] = !!value;
        else el.setAttribute(key, String(value));
      });
    }
    for (var i = 2; i < arguments.length; i++) appendChild(el, arguments[i]);
    return el;
  }
  function appendChild(el, child) {
    if (child == null || child === false) return;
    if (Array.isArray(child)) { child.forEach(function (c) { appendChild(el, c); }); return; }
    if (typeof child === 'string' || typeof child === 'number') { el.appendChild(document.createTextNode(String(child))); return; }
    el.appendChild(child);
  }
  function replace(el) {
    el.replaceChildren();
    for (var i = 1; i < arguments.length; i++) appendChild(el, arguments[i]);
    return el;
  }
  function setText(el, text) {
    text = text == null ? '' : String(text);
    if (el.textContent !== text) el.textContent = text;
  }

  // ---- identifiers & URLs (allowlists mirror the server's own validators) ----------------
  function safeCode(value) {
    return typeof value === 'string' && /^[A-Za-z\d_.:-]{1,128}$/.test(value) ? value : '';
  }
  function safeId(value) {
    return typeof value === 'string' && /^[A-Za-z0-9][A-Za-z0-9_.-]{0,127}$/.test(value) ? value : '';
  }
  // Only same-origin, rooted, non-protocol-relative paths ever become playable/linkable URLs.
  function safeOutputUrl(value) {
    if (typeof value !== 'string' || value.charAt(0) !== '/' || value.indexOf('//') === 0) return null;
    try {
      var url = new URL(value, location.origin);
      return url.origin === location.origin ? url.pathname + url.search : null;
    } catch (_e) { return null; }
  }

  // ---- formatting -------------------------------------------------------------------------
  function fmtKbps(bandwidth) {
    if (typeof bandwidth !== 'number' || !isFinite(bandwidth) || bandwidth <= 0) return '';
    return bandwidth >= 1e6 ? (bandwidth / 1e6).toFixed(2) + ' Mbps' : Math.round(bandwidth / 1000) + ' kbps';
  }
  function fmtSeconds(seconds) {
    if (typeof seconds !== 'number' || !isFinite(seconds) || seconds < 0) return '';
    var whole = Math.round(seconds);
    var m = Math.floor(whole / 60), s = whole % 60;
    return m + ':' + (s < 10 ? '0' : '') + s;
  }
  function fmtElapsed(ms, t) {
    var total = Math.max(0, Math.round(ms / 1000));
    var m = Math.floor(total / 60), s = total % 60;
    return m > 0 ? t('unitMinSec', { m: m, s: s }) : t('unitSec', { s: s });
  }
  function fmtDate(iso, lang) {
    var d = new Date(iso);
    if (isNaN(d.getTime())) return null;
    try { return d.toLocaleString(lang === 'en' ? 'en-US' : 'ja-JP', { year: 'numeric', month: 'short', day: 'numeric', hour: '2-digit', minute: '2-digit' }); }
    catch (_e) { return d.toISOString(); }
  }

  // ---- codec label ------------------------------------------------------------------------
  function codecLabel(track) {
    var codecs = track.codecs && String(track.codecs).split(',')[0];
    return codecs || track.codec || '';
  }

  // ---- (i) explanations --------------------------------------------------------------------
  // infoToggle(ariaLabel, text, options) -> {button, panel}: a small (i) button that
  // shows/hides one explanation paragraph. The caller places the panel where it should open
  // (after the line the button sits on) and passes the translated accessible name of the
  // button (what it explains); the button reports its state (aria-expanded / aria-controls).
  // options: open (start open), onToggle(open) (the caller remembers the state across its own
  // re-renders), focusKey (a data-focus-key so keyboard focus survives a re-render).
  var infoSeq = 0;
  function infoToggle(ariaLabel, text, options) {
    options = options || {};
    var id = 'info-' + (++infoSeq);
    var open = !!options.open;
    var panel = h('p', { className: 'info-panel', id: id, hidden: !open, text: text });
    var button = h('button', { type: 'button', className: 'info-btn', 'aria-expanded': String(open), 'aria-controls': id, 'aria-label': ariaLabel,
      onclick: function () { var next = panel.hidden; panel.hidden = !next; button.setAttribute('aria-expanded', String(next)); if (options.onToggle) options.onToggle(next); } },
      h('span', { className: 'info-mark', 'aria-hidden': 'true', text: 'i' }));
    if (options.focusKey) button.dataset.focusKey = options.focusKey;
    return { button: button, panel: panel };
  }

  // ---- chips & notices ---------------------------------------------------------------------
  function chip(kind, text) {
    return h('span', { className: 'chip' + (kind ? ' chip-' + kind : ''), text: text });
  }
  function notice(kind, text, code) {
    return h('div', { className: 'notice notice-' + kind, role: kind === 'fail' ? 'alert' : 'status' },
      h('span', null, text), code ? h('code', { text: code }) : null);
  }

  // ---- progress strips -------------------------------------------------------------------
  // rows: [{id, label, status: 'pending'|'running'|'done'|'failed', fraction: 0..1|null, state: text}]
  function strips(rows) {
    var root = h('div', { className: 'strips' });
    rows.forEach(function (r) {
      var det = r.fraction == null;
      var fill = h('div', { className: 'strip-fill' + (det && r.status === 'running' ? ' indeterminate' : ''), style: 'width:' + (det ? (r.status === 'done' || r.status === 'failed' ? 100 : 0) : Math.round(r.fraction * 100)) + '%' });
      root.appendChild(h('div', { className: 'strip', dataset: { status: r.status, trackId: String(r.id) } },
        h('span', { className: 'strip-label', title: String(r.label), text: String(r.label) }),
        h('div', { className: 'strip-track', role: 'progressbar', 'aria-valuemin': '0', 'aria-valuemax': '100', 'aria-valuenow': det ? null : String(Math.round(r.fraction * 100)), 'aria-label': String(r.label) }, fill),
        h('span', { className: 'strip-state', text: r.state || '' })));
    });
    return root;
  }

  App.UI = {
    h: h, replace: replace, setText: setText, safeCode: safeCode, safeId: safeId, safeOutputUrl: safeOutputUrl,
    fmtKbps: fmtKbps, fmtSeconds: fmtSeconds, fmtElapsed: fmtElapsed, fmtDate: fmtDate,
    codecLabel: codecLabel, chip: chip, notice: notice, strips: strips, infoToggle: infoToggle,
  };
})();
