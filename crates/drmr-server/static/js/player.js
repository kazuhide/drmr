/* Shaka Player adapter for local ClearKey playback and track selection. */
(function () {
  'use strict';
  var App = (window.App = window.App || {});

  // RFC 6381 codec strings start with a 4-char sample-entry code ("avc1", "hvc1", "hev1",
  // "mp4a", ...) -- the part before the first '.'. That prefix is the "codec family" Shaka's
  // `preferredVideoCodecs` matches against (one family per load). Shared with
  // console.js (loaded after this file) so both use ONE definition.
  function codecFamily(codec) {
    var s = String(codec == null ? '' : codec);
    var dot = s.indexOf('.');
    return dot === -1 ? s : s.slice(0, dot);
  }
  App.codecFamily = codecFamily;

  // base64url -> lowercase hex (W3C ClearKey JWK `k`/`kid` are unpadded base64url; Shaka's
  // clearKeys map is hex). In-memory only, never emitted or logged.
  function b64urlToHex(s) {
    s = String(s).replace(/-/g, '+').replace(/_/g, '/');
    while (s.length % 4) s += '=';
    var bin = atob(s), hex = '';
    for (var i = 0; i < bin.length; i++) hex += bin.charCodeAt(i).toString(16).padStart(2, '0');
    return hex;
  }

  // Keyless ClearKey: fetch this asset's license endpoint, convert its JWK Set into Shaka's
  // client-side clearKeys hex map in memory. Returns Promise<{'<kidHex>': '<keyHex>'}>.
  function fetchKeylessClearKeys(assetId, licenseUrl, signal) {
    return fetch(licenseUrl || '/clearkey/license?asset_id=' + encodeURIComponent(assetId),
      licenseUrl ? {method: 'POST', headers: {'Content-Type': 'application/json'}, body: '{}', signal: signal} : {signal: signal}).then(function (r) {
      if (!r.ok) throw new Error('ClearKey license fetch failed: HTTP ' + r.status);
      return r.json();
    }).then(function (jwk) {
      var map = {};
      ((jwk && jwk.keys) || []).forEach(function (k) { map[b64urlToHex(k.kid)] = b64urlToHex(k.k); });
      if (!Object.keys(map).length) throw new Error('ClearKey license response contained no keys');
      return map;
    });
  }

  // rep_id is `track.originalVideoId` for a DASH variant -- NOT Shaka's own internal numeric
  // `track.id`, which is unrelated to the manifest's own Representation@id.
  function repIdForTrack(t) {
    return t.originalVideoId != null ? String(t.originalVideoId) : String(t.id);
  }

  function createShakaPlayer(videoEl) {
    var shakaPlayer = null;
    var generation = 0;
    var sourceRenditions = [];
    var pendingDestroy = Promise.resolve();
    var destroyOwner = null;
    var activeSetup = null;
    // One local setup budget includes the previous owner's teardown, key body and media load.
    var SETUP_TIMEOUT_MS = 20000;
    var bufferPollTimer = null;
    var listeners = { event: [], state: [] };

    var state = {
      status: 'idle', // idle | loading | playing | error
      phase: 'idle', // idle | teardown | key | media | ready
      url: null,
      renditions: [], // [{id, label}] -- Shaka's own view, one row per distinct DASH rep_id
      currentRenditionId: null, // last-selected rendition (video OR audio rep_id) / active video id
      selectedVideoId: null, // the VIDEO rep_id pinned/active -- unlike currentRenditionId, never an audio id
      abrEnabled: true, // false once a rendition is manually picked
      keyStatus: 'n/a',
      bufferSeconds: null,
      currentTime: null,
      lastError: null,
      preferredVideoCodecs: [], // codec family last requested via load()
      loadedVideoFamily: null, // codec family of the tracks Shaka ACTUALLY exposes after load()
    };
    // The one pending `playing` listener (see load()); tracked so stop() can detach it and
    // repeated loads never stack listeners on the same <video>.
    var onPlayingHandler = null;

    function notify(kind, value) {
      listeners[kind].forEach(function (cb) {
        try { cb(value); } catch (_e) { console.error('[player] observer notification failed'); }
      });
    }
    function emitEvent(level, message) {
      var rec = { ts: new Date().toISOString(), level: level, message: message };
      notify('event', rec);
      if (level === 'error') console.error('[player]', message);
      else console.log('[player]', message);
    }
    function emitState() {
      var snapshot = getState();
      notify('state', snapshot);
    }
    function getState() {
      return {
        status: state.status,
        phase: state.phase,
        url: state.url,
        renditions: state.renditions.slice(),
        currentRenditionId: state.currentRenditionId,
        abrEnabled: state.abrEnabled,
        keyStatus: state.keyStatus,
        bufferSeconds: state.bufferSeconds,
        currentTime: state.currentTime,
        lastError: state.lastError,
        preferredVideoCodecs: state.preferredVideoCodecs.slice(),
        loadedVideoFamily: state.loadedVideoFamily,
      };
    }

    function clearRenditions() {
      state.renditions = [];
      state.currentRenditionId = null;
      state.selectedVideoId = null;
      state.loadedVideoFamily = null;
      sourceRenditions = [];
    }

    function detachPlayingHandler() {
      if (onPlayingHandler) {
        videoEl.removeEventListener('playing', onPlayingHandler);
        onPlayingHandler = null;
      }
    }

    function startBufferPoll() {
      stopBufferPoll();
      bufferPollTimer = setInterval(function () {
        try {
          if (videoEl.buffered && videoEl.buffered.length) {
            var end = videoEl.buffered.end(videoEl.buffered.length - 1);
            state.bufferSeconds = Math.max(0, end - videoEl.currentTime);
          } else {
            state.bufferSeconds = null;
          }
          state.currentTime = videoEl.currentTime;
        } catch (_e) {
          state.bufferSeconds = null;
          state.currentTime = null;
        }
        emitState();
      }, 500);
    }
    function stopBufferPoll() {
      if (bufferPollTimer) {
        clearInterval(bufferPollTimer);
        bufferPollTimer = null;
      }
    }

    function destroyCurrent() {
      if (!shakaPlayer) return;
      var owner = { player: shakaPlayer };
      destroyOwner = owner;
      shakaPlayer = null;
      // Keep the owner even when native destruction rejects. A failed destroy cannot
      // authorize a second player on this video; reloading the page is the recovery path.
      pendingDestroy = Promise.resolve().then(function () { return owner.player.destroy(); }).then(function () {
        if (destroyOwner === owner) destroyOwner = null;
      }, function () {});
    }

    function stop() {
      var stoppedGeneration = ++generation;
      if (activeSetup) activeSetup.cancel();
      stopBufferPoll();
      detachPlayingHandler();
      destroyCurrent();
      return pendingDestroy.then(function () {
        if (generation !== stoppedGeneration) return;
        clearRenditions();
        state.status = 'idle';
        state.phase = 'idle';
        state.url = null;
        state.abrEnabled = true;
        state.keyStatus = 'n/a';
        state.preferredVideoCodecs = [];
        state.bufferSeconds = null;
        state.currentTime = null;
        state.lastError = null;
        emitEvent('info', 'stopped');
        emitState();
      });
    }

    function populateRenditions() {
      var tracks = shakaPlayer.getVariantTracks();
      // getVariantTracks() is (video, audio) PAIRS, so one video rep_id repeats once per audio
      // pairing -- keep ONE row per distinct id.
      var seen = {};
      state.renditions = [];
      tracks.forEach(function (t) {
        var id = repIdForTrack(t);
        if (seen[id]) return;
        seen[id] = true;
        state.renditions.push({
          id: id,
          label: '#' + id + ' ' + (t.width || '?') + 'x' + (t.height || '?') + ' ' +
            Math.round((t.bandwidth || 0) / 1000) + 'kbps',
        });
      });
      var active = tracks.filter(function (t) { return t.active; })[0];
      state.currentRenditionId = active ? repIdForTrack(active) : (tracks[0] ? repIdForTrack(tracks[0]) : null);
      state.selectedVideoId = active && active.originalVideoId != null ? String(active.originalVideoId) : null;
      // Shaka keeps ONE video codec family per load (see header), so the first track that has a
      // video codec names the family that is really loaded -- which can differ from what
      // load() requested (e.g. requested [] -> Shaka's own pick; requested an unsupported family).
      var withVideo = tracks.filter(function (t) { return t.videoCodec; })[0];
      state.loadedVideoFamily = withVideo ? codecFamily(withVideo.videoCodec) : null;
      emitState();
    }

    // Returns true/false (found/not found). Falls back to selectAudioRendition when no video
    // variant matches `repId` (it may name an audio-only representation).
    function sourceVariant(repId, tracks) {
      var source = sourceRenditions.filter(function (r) { return String(r.rep_id) === String(repId) && r.content_type === 'video'; })[0];
      if (!source) return null;
      function family(codec) { return codec === 'h264' ? 'avc1' : codec === 'hevc' ? 'hvc1' : codecFamily(codec); }
      var matches = tracks.filter(function (track) {
        return track.width === source.width && track.height === source.height && family(track.videoCodec) === family(source.codec);
      });
      // Prefer the exact master BANDWIDTH, including audio. If an origin omitted it,
      // only an unambiguous video identity is selectable; never pick by list order.
      var exact = matches.filter(function (track) { return Number(track.bandwidth) === Number(source.bandwidth); });
      if (exact.length) matches = exact;
      var ids = new Set(matches.map(function (track) { return track.videoId == null ? repIdForTrack(track) : String(track.videoId); }));
      return ids.size === 1 ? matches[0] : null;
    }

    function selectRendition(repId) {
      if (!shakaPlayer) return false;
      var tracks = shakaPlayer.getVariantTracks();
      var track = tracks.filter(function (t) { return repIdForTrack(t) === String(repId); })[0];
      if (!track) track = sourceVariant(repId, tracks);
      if (!track) return selectAudioRendition(repId);
      shakaPlayer.configure('abr.enabled', false);
      state.abrEnabled = false;
      shakaPlayer.selectVariantTrack(track, /* clearBuffer= */ true);
      state.currentRenditionId = repIdForTrack(track);
      if (track.originalVideoId != null) state.selectedVideoId = String(track.originalVideoId);
      emitEvent('info', 'switched to rendition #' + state.currentRenditionId + ' (ABR disabled)');
      emitState();
      return true;
    }

    // Matches `originalAudioId`, preferring a variant that also keeps the video the viewer is
    // on, so switching audio doesn't silently change video quality too. "The video the viewer
    // is on" is: with ABR on, whatever variant is active right now (ABR keeps moving it); with
    // ABR off, the video that was pinned (falling back to the active one). NOT
    // `currentRenditionId`, which holds an AUDIO rep_id after a previous audio pick.
    function selectAudioRendition(repId) {
      if (!shakaPlayer) return false;
      var tracks = shakaPlayer.getVariantTracks();
      var matches = tracks.filter(function (t) { return t.originalAudioId != null && String(t.originalAudioId) === String(repId); });
      if (!matches.length) return false;
      var activeTrack = tracks.filter(function (t) { return t.active; })[0];
      var activeVideoId = activeTrack && activeTrack.originalVideoId != null ? String(activeTrack.originalVideoId) : null;
      var keepVideoId = state.abrEnabled
        ? (activeVideoId || state.selectedVideoId)
        : (state.selectedVideoId || activeVideoId);
      var keepingCurrentVideo = keepVideoId == null ? null : matches.filter(function (t) {
        return t.originalVideoId != null && String(t.originalVideoId) === String(keepVideoId);
      })[0];
      var track = keepingCurrentVideo || matches[0];
      shakaPlayer.configure('abr.enabled', false);
      state.abrEnabled = false;
      shakaPlayer.selectVariantTrack(track, /* clearBuffer= */ true);
      if (track.originalVideoId != null) state.selectedVideoId = String(track.originalVideoId);
      state.currentRenditionId = String(repId);
      emitEvent('info', 'switched to audio rendition #' + repId + ' (ABR disabled)');
      emitState();
      return true;
    }

    function selectTextRendition(repId) {
      if (!shakaPlayer) return false;
      if (repId == null || repId === '') {
        shakaPlayer.setTextTrackVisibility(false);
        return true;
      }
      var matches = shakaPlayer.getTextTracks().filter(function (track) {
        return track.originalTextId != null && String(track.originalTextId) === String(repId);
      });
      if (matches.length !== 1) return false;
      shakaPlayer.selectTextTrack(matches[0]);
      shakaPlayer.setTextTrackVisibility(true);
      return true;
    }

    function enableAbr() {
      if (!shakaPlayer) return;
      shakaPlayer.configure('abr.enabled', true);
      state.abrEnabled = true;
      emitEvent('info', 'ABR (auto) re-enabled');
      emitState();
    }

    // load(assetId, url, preferredVideoCodecs) -- always stop()s first so a re-load never
    // races two shaka.Player instances against the same <video>.
    function load(assetId, url, preferredVideoCodecs, options) {
      options = options || {};
      var deadline = performance.now() + SETUP_TIMEOUT_MS;
      var stopped = stop();
      var loadGeneration = generation;
      var controller = new AbortController();
      var timer, resolveSetup, rejectSetup;
      var attempt = { settled: false, ready: false, playing: false, cancel: cancel };
      var result = new Promise(function (resolve, reject) { resolveSetup = resolve; rejectSetup = reject; });
      activeSetup = attempt;
      state.url = url;
      state.status = 'loading';
      state.phase = 'teardown';
      state.keyStatus = 'n/a';
      state.lastError = null;
      emitState();
      function isCurrent() { return generation === loadGeneration; }
      function finish() {
        attempt.settled = true;
        clearTimeout(timer);
        if (activeSetup === attempt) activeSetup = null;
      }
      function cancel() {
        if (attempt.settled) return;
        finish();
        controller.abort();
        resolveSetup();
      }
      function setupError() {
        var code = state.phase === 'teardown' ? 'playback-cleanup-unconfirmed' : 'playback-setup-timeout';
        var error = new Error(code);
        error.code = code;
        return error;
      }
      function checkDeadline() {
        if (performance.now() >= deadline) throw setupError();
      }
      var keyFetchFailed = false;
      function fail(e) {
        if (!isCurrent() || attempt.settled) return;
        finish();
        controller.abort();
        // Invalidate every callback from this attempt before beginning asynchronous teardown.
        ++generation;
        stopBufferPoll();
        detachPlayingHandler();
        destroyCurrent();
        if (!e || typeof e !== 'object') e = new Error('playback-load-failed');
        var message = typeof e.message === 'string' ? e.message : (typeof e.code === 'number' ? 'shaka error ' + e.code : 'playback-load-failed');
        var isKeyError = keyFetchFailed;
        try { isKeyError = isKeyError || (e && e.category === shaka.util.Error.Category.DRM); } catch (_e) { /* defensive */ }
        state.keyStatus = isKeyError ? 'error' : (state.keyStatus === 'configured' ? 'configured' : 'n/a');
        state.status = 'error';
        state.lastError = { message: message, code: e.code, url: url };
        var wrapped = e instanceof Error ? e : new Error(message);
        wrapped.code = e.code;
        wrapped.url = url;
        // Settle the caller even if a presentation observer fails while rendering the error.
        rejectSetup(wrapped);
        emitEvent('error', 'load failed: ' + (e.code || '') + ' ' + message + ' (url: ' + url + ')');
        emitState();
      }
      timer = setTimeout(function () { fail(setupError()); }, Math.max(0, deadline - performance.now()));
      stopped.then(function () {
        if (!isCurrent()) return;
        checkDeadline();
        if (destroyOwner) {
          var cleanup = new Error('playback-cleanup-unconfirmed');
          cleanup.code = 'playback-cleanup-unconfirmed';
          throw cleanup;
        }
        sourceRenditions = options.clear ? (options.sourceRenditions || []).slice() : [];
        state.url = url;
        state.status = 'loading';
        videoEl.style.display = '';
        shaka.polyfill.installAll();
        if (!shaka.Player.isBrowserSupported()) throw new Error('browser not supported by Shaka Player');
        var player = new shaka.Player(videoEl);
        shakaPlayer = player;
        player.configure('abr.enabled', true);
        state.abrEnabled = true;
        player.configure('preferredVideoCodecs', preferredVideoCodecs || []);
        state.preferredVideoCodecs = preferredVideoCodecs || [];
        player.addEventListener('error', function (e) {
          if (!isCurrent()) return;
          var detail = e.detail || {};
          if (!attempt.settled) { fail(detail); return; }
          var isDrm = false;
          try { isDrm = detail.category === shaka.util.Error.Category.DRM; } catch (_e) { /* defensive */ }
          if (isDrm) state.keyStatus = 'error';
          state.status = 'error';
          state.lastError = { message: 'shaka error ' + (detail.code || ''), url: url };
          emitEvent('error', 'ERROR ' + JSON.stringify(detail));
          emitState();
        });
        player.addEventListener('trackschanged', function () {
          if (isCurrent()) populateRenditions();
        });
        state.phase = options.clear ? 'media' : 'key';
        state.keyStatus = options.clear ? 'n/a' : 'requesting';
        emitState();
        return (options.clear ? Promise.resolve(null) : fetchKeylessClearKeys(assetId, options.licenseUrl, controller.signal))
          .catch(function (err) { keyFetchFailed = true; throw err; })
          .then(function (clearKeys) {
            if (!isCurrent()) return;
            checkDeadline();
            state.phase = 'media';
            state.keyStatus = clearKeys ? 'configured' : 'n/a';
            emitState();
            // Explicit local test keys override the source manifest's DRM-system signaling.
            if (clearKeys) player.configure({ drm: { clearKeys: clearKeys }, manifest: { dash: { ignoreDrmInfo: true } } });
            detachPlayingHandler();
            onPlayingHandler = function () {
              if (!isCurrent()) return;
              // A delayed timer task must not admit a first playing event past its budget.
              if (performance.now() >= deadline) { fail(setupError()); return; }
              attempt.playing = true;
              detachPlayingHandler();
              if (attempt.ready) finish();
              state.status = 'playing';
              state.keyStatus = options.clear ? 'n/a' : 'usable';
              emitEvent('info', 'playing');
              emitState();
            };
            videoEl.addEventListener('playing', onPlayingHandler);
            return player.load(url);
          })
          .then(function () {
            if (!isCurrent()) return;
            checkDeadline();
            attempt.ready = true;
            state.phase = 'ready';
            emitEvent('info', 'loaded ' + url);
            populateRenditions();
            startBufferPoll();
            // The caller can now request play, but setup is not complete until actual playing.
            if (attempt.playing) finish();
            resolveSetup();
          });
      }).catch(fail);
      return result;
    }

    return {
      load: load,
      stop: stop,
      selectRendition: selectRendition,
      selectAudioRendition: selectAudioRendition,
      selectTextRendition: selectTextRendition,
      enableAbr: enableAbr,
      getState: getState,
      onEvent: function (cb) { listeners.event.push(cb); },
      onState: function (cb) { listeners.state.push(cb); },
    };
  }

  App.createShakaPlayer = createShakaPlayer;
})();
