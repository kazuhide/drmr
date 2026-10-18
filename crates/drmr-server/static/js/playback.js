/* Before/after playback: two independent Shaka players (player.js) with rendition and subtitle
 * switching. Shaka keeps ONE video codec family per load, so a rendition of another family
 * is selected by reloading with that family preferred. Every URL that
 * reaches a player has passed UI.safeOutputUrl in the caller. */
(function () {
  'use strict';
  var App = (window.App = window.App || {});
  var UI = App.UI, h = UI.h;
  function t(key, values) { return App.I18n.t(key, values); }
  var AUTO_VALUE = '';
  var STATUS_KEYS = { idle: 'playerIdle', loading: 'playerLoading', playing: 'playerPlaying', error: 'playerError' };
  var KEY_STATUS_KEYS = { requesting: 'keyRequesting', configured: 'keyConfigured', usable: 'keyUsable', error: 'keyError' };

  function create(byId) {
    var section = byId('play-section');
    var sides = { old: createSide('old'), new: createSide('new') };

    function createSide(key) {
      var videoEl = byId('video-' + key);
      var side = {
        key: key, videoEl: videoEl, player: App.createShakaPlayer(videoEl),
        playBtn: byId('play-' + key + '-btn'), selectEl: byId(key + '-rendition-select'),
        textSelect: byId(key + '-text-select'), textField: byId(key + '-text-field'), stateEl: byId(key + '-state'),
        textSelection: '', ctx: null, playOwner: null, busy: false, note: null, hint: null, pendingValue: null,
      };
      side.player.onState(function (s) { renderSideState(side, s); });
      videoEl.addEventListener('playing', function () { if (side.hint) { side.hint = null; renderSideState(side); } });
      side.playBtn.addEventListener('click', function () { onPlayClick(side); });
      side.selectEl.addEventListener('change', function () { onRenditionChange(side); });
      side.textSelect.addEventListener('change', function () { onTextChange(side); });
      return side;
    }

    function renderSideState(side, s) {
      s = s || side.player.getState();
      var status = side.busy && s.status !== 'error' ? 'loading' : s.status;
      var label = status === 'loading' && s.phase === 'ready' ? 'playerAwaitingPlayback' : status === 'loading' && s.phase === 'media' ? 'playerLoadingMedia' : (STATUS_KEYS[status] || 'playerIdle');
      var parts = [t(label)];
      if (status === 'loading' && s.phase === 'teardown') parts.push(t('playbackCleanup'));
      if (s.keyStatus && s.keyStatus !== 'n/a') parts.push(t(KEY_STATUS_KEYS[s.keyStatus] || 'keyRequesting'));
      if (s.lastError) {
        if (s.lastError.code === 'playback-setup-timeout') parts.push(t('playbackSetupTimeout'));
        else if (s.lastError.code === 'playback-cleanup-unconfirmed') parts.push(t('playbackCleanupUnconfirmed'));
        else { var code = String(s.lastError.message || '').match(/\b\d{4}\b/); parts.push(t('errorDetail') + ' ' + (code ? code[0] : t('requestError'))); }
      }
      if (side.note) parts.push(side.note);
      if (side.hint) parts.push(side.hint);
      UI.setText(side.stateEl, parts.join(' · '));
      syncRenditionSelect(side, s);
      refreshTextControls(side, s);
    }
    function hasOption(selectEl, value) { return Array.prototype.some.call(selectEl.options, function (o) { return o.value === value; }); }
    function syncRenditionSelect(side, s) {
      var want;
      if (side.pendingValue != null) want = side.pendingValue;
      else if (s.status === 'idle' || s.abrEnabled || s.currentRenditionId == null) want = AUTO_VALUE;
      else want = String(s.currentRenditionId);
      if (side.selectEl.value !== want && hasOption(side.selectEl, want)) side.selectEl.value = want;
    }
    function refreshSideControls(side) {
      var usable = !!(side.ctx && side.ctx.url) && !side.busy;
      side.playBtn.disabled = !usable;
      side.selectEl.disabled = !usable;
      refreshTextControls(side);
    }
    function populateRenditionSelect(side) {
      var seen = Object.create(null), videos = [], audios = [];
      ((side.ctx && side.ctx.renditions) || []).forEach(function (r) {
        if (!r || r.rep_id == null || r.content_type === 'text' || r.content_type === 'subtitle') return;
        var id = String(r.rep_id);
        if (seen[id]) return;
        seen[id] = true;
        var detail = [];
        if (r.width && r.height) detail.push(r.width + '×' + r.height);
        if (r.codec) detail.push(r.codec);
        if (r.content_type === 'audio') audios.push({ value: id, label: id + (detail.length ? '  ' + detail.join(' ') : '') + ' ' + t('audioSuffix') });
        else videos.push({ value: id, label: id + (detail.length ? '  ' + detail.join(' ') : '') });
      });
      UI.replace(side.selectEl, [{ value: AUTO_VALUE, label: t('autoAbr') }].concat(videos, audios).map(function (o) { return h('option', { value: o.value, text: o.label }); }));
      side.selectEl.value = AUTO_VALUE;
      populateTextOptions(side);
    }
    function populateTextOptions(side) {
      var tracks = ((side.ctx && side.ctx.renditions) || []).filter(function (track) { return track.content_type === 'text' || track.content_type === 'subtitle'; });
      side.textField.hidden = !tracks.length;
      side.textSelect.hidden = !tracks.length;
      UI.replace(side.textSelect, h('option', { value: '', text: t('subtitlesOff') }), tracks.map(function (track) {
        return h('option', { value: String(track.rep_id), text: String(track.rep_id) + (track.language ? ' / ' + track.language : '') });
      }));
      side.textSelect.value = hasOption(side.textSelect, side.textSelection) ? side.textSelection : '';
      refreshTextControls(side);
    }
    function refreshTextControls(side, snapshot) {
      var s = snapshot || side.player.getState();
      side.textSelect.disabled = side.busy || !side.ctx || s.url !== side.ctx.url || s.status === 'idle' || s.status === 'error';
    }
    function onTextChange(side) {
      if (side.textSelect.disabled) return;
      var value = side.textSelect.value;
      if (typeof side.player.selectTextRendition === 'function' && side.player.selectTextRendition(value || null)) { side.textSelection = value; side.note = null; }
      else { side.textSelect.value = side.textSelection; side.note = t('subtitleUnavailable'); }
      renderSideState(side);
    }
    function findRendition(renditions, repId) {
      return (renditions || []).filter(function (r) { return r && String(r.rep_id) === String(repId); })[0] || null;
    }
    function resetSide(side) {
      side.ctx = null; side.playOwner = null; side.busy = false; side.note = null; side.hint = null; side.pendingValue = null; side.textSelection = '';
      populateTextOptions(side);
      UI.replace(side.selectEl);
      refreshSideControls(side);
      side.player.stop();
      UI.setText(side.stateEl, '');
    }
    function prepareSide(side, ctx) {
      side.ctx = ctx; side.playOwner = null; side.busy = false; side.note = ctx.url ? null : t('noManifest'); side.hint = null; side.pendingValue = null;
      populateRenditionSelect(side);
      refreshSideControls(side);
      renderSideState(side);
    }
    function onPlayClick(side) {
      if (!side.ctx || !side.ctx.url || side.busy) return;
      loadAndPlay(side, [], null);
    }
    function playVideo(side, owner) {
      function failed(error) {
        if (side.playOwner !== owner || (error && error.name === 'AbortError')) return;
        side.hint = error && error.name === 'NotAllowedError' ? t('autoplayBlocked') : t('playFailed');
        renderSideState(side);
      }
      try {
        var result = side.videoEl.play();
        if (result && typeof result.catch === 'function') result.catch(failed);
      } catch (error) { failed(error); }
    }
    function loadAndPlay(side, preferredCodecs, pick) {
      var ctx = side.ctx, owner = {};
      side.playOwner = owner;
      side.busy = true; side.note = null; side.hint = null; side.pendingValue = pick ? pick.repId : AUTO_VALUE;
      refreshSideControls(side);
      renderSideState(side);
      function stillCurrent() { return side.ctx === ctx && side.playOwner === owner; }
      return side.player.load(ctx.assetId, ctx.url, preferredCodecs, { licenseUrl: ctx.licenseUrl, clear: ctx.clear, sourceRenditions: ctx.renditions }).then(function () {
        if (!stillCurrent()) return;
        if (pick) {
          var ok = pick.isAudio ? side.player.selectAudioRendition(pick.repId) : side.player.selectRendition(pick.repId);
          if (!ok) side.note = t('renditionUnavailable');
        }
        if (typeof side.player.selectTextRendition === 'function') {
          var restored = side.player.selectTextRendition(side.textSelection || null);
          if (side.textSelection && !restored) { side.textSelection = ''; side.textSelect.value = ''; side.player.selectTextRendition(null); side.note = t('subtitleUnavailable'); }
        }
        playVideo(side, owner);
      }).catch(function (e) {
        if (!stillCurrent()) return;
        if (side.player.getState().status !== 'error') side.note = t('errorPrefix') + ' ' + App.I18n.error(e);
      }).then(function () {
        if (!stillCurrent()) return;
        side.busy = false; side.pendingValue = null;
        refreshSideControls(side);
        renderSideState(side);
      });
    }
    function onRenditionChange(side) {
      var ctx = side.ctx;
      if (!ctx || !ctx.url || side.busy) return;
      var value = side.selectEl.value;
      side.note = null;
      var s = side.player.getState();
      var loadedInPlace = s.url === ctx.url && (s.status === 'playing' || s.status === 'loading');
      if (value === AUTO_VALUE) { if (loadedInPlace) side.player.enableAbr(); renderSideState(side); return; }
      var rep = findRendition(ctx.renditions, value);
      var isAudio = !!rep && rep.content_type === 'audio';
      if (isAudio) {
        if (loadedInPlace && side.player.selectAudioRendition(value)) { renderSideState(side); return; }
        loadAndPlay(side, loadedInPlace ? s.preferredVideoCodecs : [], { repId: value, isAudio: true });
        return;
      }
      var wantFamily = rep ? App.codecFamily(rep.codec) : '';
      var currentFamily = s.loadedVideoFamily || (s.preferredVideoCodecs && s.preferredVideoCodecs[0]) || null;
      if (loadedInPlace && wantFamily && currentFamily === wantFamily && side.player.selectRendition(value)) { renderSideState(side); return; }
      loadAndPlay(side, wantFamily ? [wantFamily] : [], { repId: value, isAudio: false });
    }

    function renditionMetadata(track) {
      return { rep_id: track.rep_id || track.id, content_type: track.content_type, codec: ({ h264: 'avc1', hevc: 'hvc1', aac: 'mp4a' }[track.codec] || track.codec), width: track.width, height: track.height, bandwidth: track.bandwidth, language: track.language };
    }
    function renderSummaryTable(oldSummary, newSummary) {
      function cell(id, value, changed) {
        var el = byId(id);
        UI.setText(el, value == null ? '—' : value);
        el.classList.toggle('changed', !!changed);
        el.classList.toggle('same', !changed && value != null);
      }
      var both = !!(oldSummary && newSummary);
      cell('sum-scheme-old', oldSummary && oldSummary.protection_scheme);
      cell('sum-pr-version-old', oldSummary && oldSummary.playready_version);
      cell('sum-pr-algid-old', oldSummary && oldSummary.playready_algid);
      cell('sum-kid-old', oldSummary && oldSummary.default_kid);
      cell('sum-scheme-new', newSummary && newSummary.protection_scheme, both && newSummary.protection_scheme !== oldSummary.protection_scheme);
      cell('sum-pr-version-new', newSummary && newSummary.playready_version, both && newSummary.playready_version !== oldSummary.playready_version);
      cell('sum-pr-algid-new', newSummary && newSummary.playready_algid, both && newSummary.playready_algid !== oldSummary.playready_algid);
      cell('sum-kid-new', newSummary && newSummary.default_kid); // KID is expected to stay the same
    }

    return {
      // Catalog: both manifests come from /api/assets/<id>.
      prepareCatalog: function (assetId, info) {
        if (!info.output_readiness || info.output_readiness.state !== 'verified' || !UI.safeOutputUrl(info.new_url)) { section.hidden = true; resetSide(sides.old); resetSide(sides.new); return; }
        var renditions = info.renditions || {};
        section.hidden = false;
        prepareSide(sides.old, { assetId: assetId, url: UI.safeOutputUrl(info.old_url), renditions: renditions.old || [] });
        prepareSide(sides.new, { assetId: assetId, url: UI.safeOutputUrl(info.new_url), renditions: renditions.new || [] });
        renderSummaryTable(info.old_summary, info.new_summary);
      },
      // Clear HLS: the source side streams through the playback session (clear), the packaged side through ClearKey.
      prepareHls: function (result, urls, sourceRenditions) {
        section.hidden = false;
        var renditions = (result.tracks || []).map(renditionMetadata);
        var source = (sourceRenditions || result.tracks || []).filter(function (track) { return !(track.content_type === 'audio' && track.muxed); }).map(renditionMetadata);
        prepareSide(sides.old, { assetId: result.asset_id, url: urls.oldUrl, renditions: source, clear: true });
        prepareSide(sides.new, { assetId: result.asset_id, url: urls.newUrl, renditions: renditions, licenseUrl: urls.licenseUrl });
        renderSummaryTable({ protection_scheme: 'clear' }, { protection_scheme: 'cbcs', playready_version: '4.3.0.0', playready_algid: 'AESCBC' });
      },
      hide: function () {
        section.hidden = true;
        resetSide(sides.old);
        resetSide(sides.new);
      },
      setMessage: function (message) { UI.setText(sides.old.stateEl, message); UI.setText(sides.new.stateEl, message); },
      rerender: function () { ['old', 'new'].forEach(function (key) { var side = sides[key]; if (side.ctx) { var value = side.selectEl.value; populateRenditionSelect(side); if (hasOption(side.selectEl, value)) side.selectEl.value = value; } renderSideState(side); }); },
      isVisible: function () { return !section.hidden; },
    };
  }

  App.Playback = { create: create };
})();
