/* Shared text-only DOM construction for the common console. */
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

  App.UI = { h: h, replace: replace, setText: setText };
})();
