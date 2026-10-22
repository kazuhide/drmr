/* Bounded HTTP JSON requests with explicit mutation uncertainty. */
(function () {
  'use strict';
  var App = (window.App = window.App || {});

  // This is a page-response budget, not a deadline for the server's media work.
  var REQUEST_TIMEOUT_MS = 15000;
  function requestError(code, uncertain) {
    var error = new Error(code);
    error.code = code;
    error.uncertain = uncertain;
    return error;
  }
  function fetchJson(url, options) {
    options = Object.assign({}, options || {});
    var mutation = !/^(GET|HEAD)$/i.test(options.method || 'GET');
    var controller = new AbortController();
    options.signal = controller.signal;
    var deadline = performance.now() + REQUEST_TIMEOUT_MS;
    return new Promise(function (resolve, reject) {
      var settled = false, refusal = null;
      function interrupted(code) {
        if (refusal !== null) { var error = requestError('request-failed', false); error.status = refusal; error.body = null; return error; }
        return requestError(code, mutation);
      }
      function finish(error, body) {
        if (settled) return;
        settled = true;
        clearTimeout(timer);
        if (error) { controller.abort(); reject(error); }
        else resolve(body);
      }
      var timer = setTimeout(function () { finish(interrupted('request-timeout')); }, REQUEST_TIMEOUT_MS);
      Promise.resolve().then(function () { return fetch(url, options); }).then(function (response) {
        if (!response.ok) refusal = response.status;
        return response.text().then(function (text) {
          if (settled) return;
          if (performance.now() >= deadline) { finish(interrupted('request-timeout')); return; }
          var body = null, valid = true;
          if (text) {
            try { body = JSON.parse(text); }
            catch (_error) { valid = false; }
          }
          if (performance.now() >= deadline) { finish(interrupted('request-timeout')); return; }
          if (!response.ok) {
            var error = requestError('request-failed', false);
            error.status = response.status;
            error.body = valid ? body : null;
            finish(error);
          } else if (!valid) finish(requestError('request-response-invalid', mutation));
          else finish(null, body);
        });
      }).catch(function () { finish(interrupted('network-error')); });
    });
  }
  App.Api = { fetchJson: fetchJson };
})();
