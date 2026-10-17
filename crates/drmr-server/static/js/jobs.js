/* Job observer: follows one submitted job to its terminal state.
 *
 * Primary channel is the SSE log stream (/api/jobs/<id>/log). Its terminal `done` event is the
 * only thing that may end a run successfully -- EventSource fires `error` both for a dropped
 * connection and for a normally closed stream, so completion is never inferred from `error`.
 * A real drop falls back to polling GET /api/jobs/<id> until the server reports a terminal
 * state; a 404 there means the server forgot the job (in-memory job state, e.g. a restart).
 *
 * Exactly one of onTerminal / onLost is called, at most once. */
(function () {
  'use strict';
  var App = (window.App = window.App || {});

  var POLL_INTERVAL_MS = 2000;
  var POLL_MAX_CONSECUTIVE_FAILURES = 30; // ~60 s of an unreachable server

  function isTerminal(status) {
    return !!status && (status.state === 'succeeded' || status.state === 'failed' || status.state === 'interrupted');
  }

  // handlers: {onLine(rawLine), onTerminal(status, {logInterrupted}), onLost(reasonKey), onWaiting(reasonKey)}
  function watch(jobId, handlers) {
    var source = null;
    var pollTimer = null;
    var finished = false;
    var logInterrupted = false;
    var polling = false;

    function finish(fn) {
      if (finished) return;
      finished = true;
      if (pollTimer) { clearTimeout(pollTimer); pollTimer = null; }
      if (source) { try { source.close(); } catch (_e) { /* closed */ } source = null; }
      fn();
    }

    function poll() {
      if (polling) return;
      polling = true;
      var failures = 0;
      function tick() {
        pollTimer = null;
        if (finished) return;
        App.Api.getJobStatus(jobId).then(function (status) {
          if (finished) return;
          failures = 0;
          if (isTerminal(status)) { finish(function () { handlers.onTerminal(status, { logInterrupted: logInterrupted }); }); return; }
          if (handlers.onWaiting) handlers.onWaiting('jobStillRunning');
          pollTimer = setTimeout(tick, POLL_INTERVAL_MS);
        }, function (e) {
          if (finished) return;
          if (e && e.status === 404) { finish(function () { handlers.onLost('jobNotFound'); }); return; }
          failures += 1;
          if (failures >= POLL_MAX_CONSECUTIVE_FAILURES) { finish(function () { handlers.onLost('jobUnreachable'); }); return; }
          if (handlers.onWaiting) handlers.onWaiting('serverRetrying');
          pollTimer = setTimeout(tick, POLL_INTERVAL_MS);
        });
      }
      tick();
    }

    source = App.Api.streamJobLog(
      jobId,
      function onLine(line) { if (!finished) handlers.onLine(line); },
      function onDone(payload) {
        if (finished) return;
        source = null;
        if (isTerminal(payload)) finish(function () { handlers.onTerminal(payload, { logInterrupted: logInterrupted }); });
        else poll(); // `done` without a usable payload: ask the server, never guess
      },
      function onError() {
        if (finished) return;
        source = null;
        logInterrupted = true;
        if (handlers.onWaiting) handlers.onWaiting('logDropped');
        poll();
      }
    );

    return {
      stop: function () { finish(function () {}); },
      isLogInterrupted: function () { return logInterrupted; },
      markInterrupted: function () { logInterrupted = true; },
    };
  }

  // Resume an already-known job (page reload): ask the server first; if it is still running,
  // start watching (the stream replays the whole log so the UI can rebuild progress), else
  // report the terminal state straight away.
  function resume(jobId, handlers) {
    var watcher = null;
    var cancelled = false;
    App.Api.getJobStatus(jobId).then(function (status) {
      if (cancelled) return;
      if (isTerminal(status)) handlers.onTerminal(status, { logInterrupted: true });
      else watcher = watch(jobId, handlers);
    }, function (e) {
      if (cancelled) return;
      if (e && e.status === 404) { handlers.onLost('jobNotFound'); return; }
      watcher = watch(jobId, handlers); // the stream itself may still work; it falls back to polling
      watcher.markInterrupted();
    });
    return { stop: function () { cancelled = true; if (watcher) watcher.stop(); } };
  }

  App.Jobs = { watch: watch, resume: resume, isTerminal: isTerminal };
})();
