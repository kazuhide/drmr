/* HTTP JSON requests and server-sent log events. */
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
  function objectResponse(body) {
    if (!body || typeof body !== 'object' || Array.isArray(body)) throw requestError('request-response-invalid', true);
    return body;
  }
  function jobResponse(body) {
    objectResponse(body);
    if (typeof body.job_id !== 'string' || !/^[A-Za-z0-9][A-Za-z0-9_.-]{0,127}$/.test(body.job_id)) throw requestError('request-response-invalid', true);
    return body;
  }

  function statusResponse(body, mutation) {
    if (!body || typeof body !== 'object' || Array.isArray(body) ||
        !/^(running|succeeded|failed|interrupted)$/.test(body.state) ||
        (mutation && typeof body.stop_requested !== 'boolean')) throw requestError('request-response-invalid', mutation);
    return body;
  }

  function getCatalog() {
    return fetchJson('/api/catalog');
  }

  function getAssetUrls(assetId) {
    return fetchJson('/api/assets/' + encodeURIComponent(assetId));
  }

  function postConvert(assetId) {
    return fetchJson('/api/convert', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ asset_id: assetId }),
    }).then(jobResponse);
  }

  function getJobStatus(jobId) {
    return fetchJson('/api/jobs/' + encodeURIComponent(jobId)).then(function (body) { return statusResponse(body, false); });
  }
  //   * every log record is an unnamed `data:` message -> onLine(rawJsonLine)
  //   * the stream ends with ONE terminal `event: done` whose data is
  //     {"state": "succeeded"|"failed"|"interrupted", "exit_code": <int>} -> onDone(payload)
  //     (payload is null if the data could not be parsed -- the caller should then ask
  //     GET /api/jobs/<id> instead of trusting it)
  //   * `error` therefore means a REAL connection problem -> onError(event) and nothing else.
  //     EventSource also fires `error` when the server simply closes the stream after
  //     `done`; that never reaches here because `done` closes the source first, and a closed
  //     EventSource fires no further events.
  // The source is always close()d on the first terminal event (done OR error): left open, the
  // browser would auto-reconnect and the server would replay the whole log from line 1,
  // duplicating every record (and double-counting the timing breakdown).
  // Exactly one of onDone / onError is ever called, at most once.
  function streamJobLog(jobId, onLine, onDone, onError) {
    var source = new EventSource('/api/jobs/' + encodeURIComponent(jobId) + '/log');
    var finished = false;

    function finish() {
      finished = true;
      try { source.close(); } catch (_e) { /* already closed */ }
    }

    source.onmessage = function (event) {
      if (finished) return;
      onLine(event.data);
    };

    source.addEventListener('done', function (event) {
      if (finished) return;
      finish();
      var payload = null;
      try {
        payload = JSON.parse(event.data);
      } catch (_parseErr) {
        payload = null;
      }
      if (onDone) onDone(payload);
    });

    source.onerror = function (e) {
      if (finished) return;
      finish();
      if (onError) onError(e);
    };

    return source;
  }

  function postJson(url, body) {
    return fetchJson(url, {method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify(body)}).then(objectResponse);
  }

  App.Api = {
    fetchJson: fetchJson,
    analyzeHls: function (body) { return postJson('/api/hls/analyses', body).then(jobResponse); },
    getAnalysis: function (id) { return fetchJson('/api/hls/analyses/' + encodeURIComponent(id)); },
    packageHls: function (body) { return postJson('/api/hls/packages', body).then(jobResponse); },
    getHlsResult: function (id) { return fetchJson('/api/hls/results/' + encodeURIComponent(id)); },
    getHlsJobs: function (cursor) { return fetchJson('/api/hls/jobs?limit=50' + (cursor ? '&cursor=' + encodeURIComponent(cursor) : '')); },
    deleteHlsJob: function (id) { return fetchJson('/api/hls/jobs/' + encodeURIComponent(id), {method:'DELETE'}); },
    createPlaybackSession: function (body) { return postJson('/api/hls/playback-sessions', body); },
    revokePlaybackSession: function (id) { return fetchJson('/api/hls/playback/' + encodeURIComponent(id), {method:'DELETE'}); },
    getCatalogReport: function(id) { return fetchJson('/api/reports/catalog/' + encodeURIComponent(id)); },
    getPlatform: function () { return fetchJson('/api/platform'); },
    getQueue: function () { return fetchJson('/api/queue'); },
    postQueue: function (assetIds) { return postJson('/api/queue', { asset_ids: assetIds }); },
    deleteQueueItem: function (assetId) { return fetchJson('/api/queue/' + encodeURIComponent(assetId), {method:'DELETE'}); },
    getCatalog: getCatalog,
    getAssetUrls: getAssetUrls,
    postConvert: postConvert,
    getJobStatus: getJobStatus,
    stopJob: function (id) { return postJson('/api/jobs/' + encodeURIComponent(id) + '/stop', {}).then(function (body) { return statusResponse(body, true); }); },
    streamJobLog: streamJobLog,
  };
})();
