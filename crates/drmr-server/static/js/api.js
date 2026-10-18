/* HTTP JSON requests and server-sent log events. */
(function () {
  'use strict';
  var App = (window.App = window.App || {});

  function fetchJson(url, options) {
    return fetch(url, options)
      .catch(function (networkErr) {
        var err = new Error('network_error');
        err.url = url;
        err.cause = networkErr;
        throw err;
      })
      .then(function (res) {
        return res.text().then(function (text) {
          var body = null;
          if (text) {
            try {
              body = JSON.parse(text);
            } catch (_parseErr) {
              body = { raw: text };
            }
          }
          if (!res.ok) {
            var err = new Error('request_failed');
            err.url = url;
            err.status = res.status;
            err.body = body;
            throw err;
          }
          return body;
        });
      });
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
    });
  }

  function getJobStatus(jobId) {
    return fetchJson('/api/jobs/' + encodeURIComponent(jobId));
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
    return fetchJson(url, {method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify(body)});
  }

  App.Api = {
    fetchJson: fetchJson,
    analyzeHls: function (body) { return postJson('/api/hls/analyses', body); },
    getAnalysis: function (id) { return fetchJson('/api/hls/analyses/' + encodeURIComponent(id)); },
    packageHls: function (body) { return postJson('/api/hls/packages', body); },
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
    stopJob: function (id) { return postJson('/api/jobs/' + encodeURIComponent(id) + '/stop', {}); },
    streamJobLog: streamJobLog,
  };
})();
