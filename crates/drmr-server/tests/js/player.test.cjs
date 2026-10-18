'use strict';

const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const test = require('node:test');
const vm = require('node:vm');

const source = fs.readFileSync(path.join(__dirname, '../../static/js/player.js'), 'utf8');
const flush = () => new Promise(setImmediate);

function deferred() {
  let resolve, reject;
  const promise = new Promise((yes, no) => { resolve = yes; reject = no; });
  return { promise, resolve, reject };
}

function harness() {
  const requests = new Map();
  const instances = [];
  const timers = new Set();
  const deadlines = new Map();
  let now = 0, nextTimer = 0;
  const videoListeners = new Map();
  const video = {
    style: {},
    addEventListener(name, callback) {
      if (!videoListeners.has(name)) videoListeners.set(name, new Set());
      videoListeners.get(name).add(callback);
    },
    removeEventListener(name, callback) { videoListeners.get(name)?.delete(callback); },
  };
  class FakeShakaPlayer {
    static isBrowserSupported() { return true; }
    constructor() {
      this.listeners = new Map();
      this.loads = [];
      this.keys = [];
      instances.push(this);
    }
    configure(option) { if (option.drm) this.keys.push(option.drm.clearKeys); }
    addEventListener(name, callback) { this.listeners.set(name, callback); }
    emit(name, detail) { this.listeners.get(name)?.({ detail }); }
    getVariantTracks() { return this.tracks || []; }
    selectVariantTrack(track) { this.selected = track; }
    getTextTracks() { return this.textTracks || []; }
    selectTextTrack(track) { this.selectedText = track; }
    setTextTrackVisibility(visible) { this.textVisible = visible; }
    load(url) {
      this.loads.push(url);
      return this.pendingLoad ? this.pendingLoad.promise : Promise.resolve();
    }
    destroy() { return this.pendingDestroy ? this.pendingDestroy.promise : Promise.resolve(); }
  }
  const sandbox = {
    window: {},
    console: { log() {}, error() {} },
    atob: value => Buffer.from(value, 'base64').toString('binary'),
    performance: { now: () => now },
    AbortController,
    setTimeout(callback, delay) { const id = ++nextTimer; deadlines.set(id, { callback, at: now + delay }); return id; },
    clearTimeout(id) { deadlines.delete(id); },
    setInterval(callback) { timers.add(callback); return callback; },
    clearInterval(callback) { timers.delete(callback); },
    fetch(url, options) {
      const request = deferred();
      request.signal = options?.signal;
      requests.set(url, request);
      return request.promise;
    },
    shaka: {
      polyfill: { installAll() {} },
      Player: FakeShakaPlayer,
      util: { Error: { Category: { DRM: 6 } } },
    },
  };
  vm.runInNewContext(source, sandbox);
  const player = sandbox.window.App.createShakaPlayer(video);
  const events = [];
  player.onEvent(event => events.push(event));
  function license(assetId) {
    requests.get('/clearkey/license?asset_id=' + assetId).resolve({
      ok: true,
      json: () => Promise.resolve({ keys: [{ kid: 'AA', k: assetId === 'a' ? 'AA' : 'AQ' }] }),
    });
  }
  function advance(milliseconds) {
    now += milliseconds;
    for (const [id, timer] of deadlines) {
      if (timer.at <= now) { deadlines.delete(id); timer.callback(); }
    }
  }
  return { player, instances, timers, deadlines, videoListeners, requests, events, license, advance, sandbox, video };
}

test('stop during the license fetch leaves the player idle without rearming listeners or timers', async () => {
  const h = harness();
  const loading = h.player.load('a', '/a').catch(() => {});
  await flush();
  await h.player.stop();
  assert.equal(h.requests.get('/clearkey/license?asset_id=a').signal.aborted, true);
  h.license('a');
  await loading;
  assert.equal(h.player.getState().status, 'idle');
  assert.equal(h.player.getState().url, null);
  assert.equal(h.instances[0].loads.length, 0);
  assert.equal(h.timers.size, 0);
  assert.equal(h.videoListeners.get('playing')?.size || 0, 0);
});

test('a late A license cannot load A or install A keys on the replacement B player', async () => {
  const h = harness();
  const oldLoad = h.player.load('a', '/a').catch(() => {});
  await flush();
  await h.player.stop();
  const newLoad = h.player.load('b', '/b');
  await flush();
  h.license('b');
  await newLoad;
  const replacement = h.instances[1];
  const keyBefore = JSON.stringify(replacement.keys);
  h.license('a');
  await oldLoad;
  assert.deepEqual(replacement.loads, ['/b']);
  assert.equal(JSON.stringify(replacement.keys), keyBefore);
  assert.equal(h.player.getState().url, '/b');
});

test('a replacement load waits for the pending destruction of the previous instance', async () => {
  const h = harness();
  const oldLoad = h.player.load('a', '/a');
  await flush();
  h.license('a');
  await oldLoad;
  const destruction = h.instances[0].pendingDestroy = deferred();
  const stopping = h.player.stop();
  const newLoad = h.player.load('b', '/b');
  await flush();
  assert.equal(h.instances.length, 1, 'the old instance still owns the video during destroy');
  destruction.resolve();
  await stopping;
  await flush();
  h.license('b');
  await newLoad;
  assert.equal(h.instances.length, 2);
  assert.equal(h.player.getState().url, '/b');
});

test('a stale license rejection cannot overwrite the replacement state or emit an error', async () => {
  const h = harness();
  const oldLoad = h.player.load('a', '/a').catch(() => {});
  await flush();
  await h.player.stop();
  const newLoad = h.player.load('b', '/b');
  await flush();
  h.license('b');
  await newLoad;
  const before = JSON.stringify(h.player.getState());
  const eventCount = h.events.length;
  h.requests.get('/clearkey/license?asset_id=a').reject(new Error('old request failed'));
  await oldLoad;
  assert.equal(JSON.stringify(h.player.getState()), before);
  assert.equal(h.events.length, eventCount);
});

test('stale Shaka events and a rejected load cannot mutate a replacement player', async () => {
  const h = harness();
  const oldLoad = h.player.load('a', '/a').catch(() => {});
  await flush();
  const oldInstance = h.instances[0];
  const pendingLoad = oldInstance.pendingLoad = deferred();
  h.license('a');
  await flush();
  await h.player.stop();
  const newLoad = h.player.load('b', '/b');
  await flush();
  h.license('b');
  await newLoad;
  const before = JSON.stringify(h.player.getState());
  const eventCount = h.events.length;
  oldInstance.emit('error', { category: 6, code: 6001 });
  oldInstance.emit('trackschanged');
  pendingLoad.reject(new Error('old load aborted'));
  await oldLoad;
  assert.equal(JSON.stringify(h.player.getState()), before);
  assert.equal(h.events.length, eventCount);
});

test('a stale successful Shaka load cannot rearm its playback listener or publish state', async () => {
  const h = harness();
  const oldLoad = h.player.load('a', '/a');
  await flush();
  const pendingLoad = h.instances[0].pendingLoad = deferred();
  h.license('a');
  await flush();
  await h.player.stop();
  pendingLoad.resolve();
  await oldLoad;
  assert.equal(h.player.getState().status, 'idle');
  assert.equal(h.timers.size, 0);
  assert.equal(h.videoListeners.get('playing')?.size || 0, 0);
});

test('a current license failure still rejects and reports an actionable key error', async () => {
  const h = harness();
  const loading = h.player.load('a', '/a');
  const rejected = assert.rejects(loading, /current request failed/);
  await flush();
  h.requests.get('/clearkey/license?asset_id=a').reject(new Error('current request failed'));
  await rejected;
  assert.equal(h.player.getState().status, 'error');
  assert.equal(h.player.getState().keyStatus, 'error');
  assert.equal(h.player.getState().lastError.url, '/a');
});

test('clear source preview loads without ever requesting a license', async () => {
  const h = harness();
  await h.player.load('source', '/preview/master.m3u8', [], {clear: true});
  assert.equal(h.requests.size, 0);
  assert.deepEqual(h.instances[0].loads, ['/preview/master.m3u8']);
  assert.equal(h.instances[0].keys.length, 0);
});

test('a result-scoped playback session uses its own license endpoint', async () => {
  const h = harness();
  const loading = h.player.load('result', '/result/manifest.mpd', [], {licenseUrl: '/api/hls/playback/session/license'});
  await flush();
  assert.equal(h.requests.has('/clearkey/license?asset_id=result'), false);
  h.requests.get('/api/hls/playback/session/license').resolve({
    ok: true, json: () => Promise.resolve({keys: [{kid: 'AA', k: 'AQ'}]}),
  });
  await loading;
  assert.deepEqual(h.instances[0].loads, ['/result/manifest.mpd']);
});


test('clear HLS rendition selection maps opaque Shaka ids by exact source metadata', async () => {
  const h = harness();
  await h.player.load('source', '/preview/master', [], {clear: true, sourceRenditions: [
    {rep_id: 'v000', content_type: 'video', codec: 'h264', width: 320, height: 180, bandwidth: 200000},
    {rep_id: 'v001', content_type: 'video', codec: 'h264', width: 320, height: 180, bandwidth: 400000},
  ]});
  const a = {id: 10, originalVideoId: null, width: 320, height: 180, videoCodec: 'avc1.64001e', bandwidth: 200000};
  const b = {id: 11, originalVideoId: null, width: 320, height: 180, videoCodec: 'avc1.64001e', bandwidth: 400000};
  h.instances[0].tracks = [a, b];
  assert.equal(h.player.selectRendition('v001'), true);
  assert.equal(h.instances[0].selected, b);
  assert.equal(h.player.selectRendition('v000'), true);
  assert.equal(h.instances[0].selected, a);
  await h.player.stop();
});


test('subtitle selection uses manifest identity and off releases visibility', async () => {
  const h = harness();
  await h.player.load('source', '/preview/master', [], {clear: true});
  const text = {id: 7, originalTextId: 't000', language: 'en'};
  h.instances[0].textTracks = [text];
  assert.equal(h.player.selectTextRendition('t000'), true);
  assert.equal(h.instances[0].selectedText, text);
  assert.equal(h.instances[0].textVisible, true);
  assert.equal(h.player.selectTextRendition('missing'), false);
  assert.equal(h.player.selectTextRendition(null), true);
  assert.equal(h.instances[0].textVisible, false);
  await h.player.stop();
});

test('independent HLS audio from the sanitized master selects its stable ID and keeps the active video', async () => {
  const master = process.env.DRM_TEST_HLS_MASTER
    ? fs.readFileSync(process.env.DRM_TEST_HLS_MASTER, 'utf8')
    : '#EXTM3U\n#EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID="audio",NAME="a100",LANGUAGE="ja",URI="/opaque/audio"\n';
  const audioLine = master.split('\n').find(line => line.startsWith('#EXT-X-MEDIA:TYPE=AUDIO,'));
  assert.ok(audioLine, 'the sanitized source master advertises independent audio');
  const audioId = /(?:^|,)NAME="([^"]+)"/.exec(audioLine)?.[1];
  assert.ok(audioId);
  const h = harness();
  await h.player.load('source', '/preview/master', [], {clear: true});
  // Shaka uses HLS EXT-X-MEDIA NAME as originalAudioId. Video pairing is independent.
  const active = {id: 1, originalVideoId: 'current-video', originalAudioId: 'initial', active: true};
  const otherVideo = {id: 2, originalVideoId: 'other-video', originalAudioId: audioId};
  const keepingVideo = {id: 3, originalVideoId: 'current-video', originalAudioId: audioId};
  h.instances[0].tracks = [active, otherVideo, keepingVideo];
  assert.equal(h.player.selectAudioRendition(audioId), true);
  assert.equal(h.instances[0].selected, keepingVideo);
  assert.equal(h.player.getState().currentRenditionId, audioId);
  assert.equal(h.player.getState().abrEnabled, false);
  await h.player.stop();
});


for (const stage of ['http', 'json', 'media']) {
  test('the setup deadline bounds an unresolved ' + stage + ' operation and ignores its late result', async () => {
    const h = harness();
    let outcome;
    const loading = h.player.load('a', '/a').then(() => { outcome = 'loaded'; }, error => { outcome = error; });
    await flush();
    const body = deferred();
    if (stage === 'json') h.requests.get('/clearkey/license?asset_id=a').resolve({ ok: true, json: () => body.promise });
    if (stage === 'media') { h.instances[0].pendingLoad = body; h.license('a'); }
    await flush();
    if (stage === 'media') assert.equal(h.player.getState().keyStatus, 'configured', 'a fetched test key is not yet usable');
    h.advance(19999);
    await flush();
    assert.equal(outcome, undefined);
    h.advance(1);
    await flush();
    assert.equal(h.player.getState().status, 'error');
    assert.equal(outcome.code, 'playback-setup-timeout');
    assert.equal(h.player.getState().lastError.code, 'playback-setup-timeout');
    assert.equal(h.requests.get('/clearkey/license?asset_id=a').signal.aborted, true);
    assert.equal(h.deadlines.size, 0);
    assert.equal(h.timers.size, 0);
    assert.equal(h.videoListeners.get('playing')?.size || 0, 0);
    const next = h.player.load('b', '/b');
    await flush();
    h.license('b');
    await next;
    const before = JSON.stringify(h.player.getState());
    if (stage === 'http') h.license('a');
    else body.resolve(stage === 'json' ? { keys: [{ kid: 'AA', k: 'AA' }] } : undefined);
    await flush();
    await loading;
    h.instances[0].emit('error', { category: 6, code: 6001 });
    h.instances[0].emit('trackschanged');
    assert.equal(JSON.stringify(h.player.getState()), before);
    assert.equal(h.player.getState().url, '/b');
    await h.player.stop();
  });
}

test('the same setup budget includes previous destruction and never overlaps an unconfirmed owner', async () => {
  const h = harness();
  await h.player.load('source', '/clear', [], { clear: true });
  const destruction = h.instances[0].pendingDestroy = deferred();
  let outcome;
  h.player.load('b', '/b').catch(error => { outcome = error; });
  await flush();
  h.advance(20000);
  await flush();
  assert.equal(h.player.getState().status, 'error');
  assert.equal(outcome.code, 'playback-cleanup-unconfirmed');
  assert.equal(h.instances.length, 1);
  let retryError;
  h.player.load('c', '/c').catch(error => { retryError = error; });
  await flush();
  h.advance(20000);
  await flush();
  assert.equal(retryError.code, 'playback-cleanup-unconfirmed');
  assert.equal(h.instances.length, 1, 'a timed-out destroy still owns the video');
  destruction.resolve();
  await flush();
  const recovered = h.player.load('d', '/d');
  await flush();
  h.license('d');
  await recovered;
  assert.equal(h.instances.length, 2);
  assert.equal(h.player.getState().url, '/d');
  await h.player.stop();
});

test('time spent waiting for teardown is not added to the key-fetch budget', async () => {
  const h = harness();
  await h.player.load('source', '/clear', [], { clear: true });
  const destruction = h.instances[0].pendingDestroy = deferred();
  let outcome;
  h.player.load('b', '/b').catch(error => { outcome = error; });
  await flush();
  h.advance(15000);
  destruction.resolve();
  await flush();
  assert.equal(h.instances.length, 2);
  h.advance(5000);
  await flush();
  assert.equal(outcome?.code, 'playback-setup-timeout');
  assert.equal(h.player.getState().status, 'error');
});

test('a rejected destroy remains owned and cannot authorize a replacement player', async () => {
  const h = harness();
  await h.player.load('source', '/clear', [], { clear: true });
  h.instances[0].pendingDestroy = deferred();
  let outcome;
  h.player.load('b', '/b').catch(error => { outcome = error; });
  await flush();
  h.instances[0].pendingDestroy.reject(new Error('native teardown failed'));
  await flush();
  assert.equal(outcome?.code, 'playback-cleanup-unconfirmed');
  await assert.rejects(h.player.load('c', '/c'), error => error.code === 'playback-cleanup-unconfirmed');
  assert.equal(h.instances.length, 1);
  assert.equal(h.requests.has('/clearkey/license?asset_id=b'), false);
});


function playbackHarness(language) {
  const h = harness();
  const elements = new Map();
  function element(id) {
    if (!elements.has(id)) {
      const listeners = new Map();
      elements.set(id, {
        children: [], value: '', textContent: '', style: {}, disabled: false,
        classList: { toggle() {} },
        get options() { return this.children; },
        addEventListener(name, callback) { listeners.set(name, callback); },
        removeEventListener(name, callback) { if (listeners.get(name) === callback) listeners.delete(name); },
        dispatch(name) { listeners.get(name)?.(); },
        play() { return Promise.resolve(); },
      });
    }
    return elements.get(id);
  }
  const App = h.sandbox.window.App;
  App.UI = {
    safeOutputUrl: value => value,
    h: (_tag, options) => options,
    replace: (target, ...values) => { target.children = values.flat(Infinity); },
    setText: (target, value) => { target.textContent = value; },
  };
  h.sandbox.localStorage = { getItem: () => language };
  for (const file of ['i18n.js', 'playback.js']) {
    vm.runInNewContext(fs.readFileSync(path.join(__dirname, '../../static/js', file), 'utf8'), h.sandbox);
  }
  const playback = App.Playback.create(element);
  playback.prepareCatalog('a', { old_url: '/a', new_url: '/b', renditions: {}, output_readiness: { state: 'verified' } });
  return { ...h, element, playback, i18n: App.I18n };
}

for (const language of ['en', 'ja']) {
  test(language + ': a stalled local playback releases the actual controls and an old result cannot end the next busy state', async () => {
    const h = playbackHarness(language);
    const play = h.element('play-old-btn'), select = h.element('old-rendition-select');
    play.dispatch('click');
    await flush();
    assert.equal(play.disabled, true);
    const oldRequest = h.requests.get('/clearkey/license?asset_id=a');
    h.advance(20000);
    await flush();
    assert.equal(play.disabled, false);
    assert.equal(select.disabled, false);
    assert.notEqual(h.i18n.t('playbackSetupTimeout'), 'playbackSetupTimeout');
    assert.ok(h.element('old-state').textContent.includes(h.i18n.t('playbackSetupTimeout')));
    play.dispatch('click');
    await flush();
    assert.equal(play.disabled, true);
    oldRequest.resolve({ ok: true, json: () => Promise.resolve({ keys: [{ kid: 'AA', k: 'AA' }] }) });
    await flush();
    assert.equal(play.disabled, true, 'a stale source cannot clear the new attempt');
    h.license('a');
    await flush();
    assert.equal(play.disabled, false);
    assert.ok(h.element('old-state').textContent.includes(h.i18n.t('keyConfigured')));
    h.playback.hide();
    await flush();
    assert.equal(h.deadlines.size, 0);
    assert.equal(h.timers.size, 0);
  });
}


test('a configured local test key becomes usable only after the video actually plays', async () => {
  const h = harness();
  const loading = h.player.load('a', '/a');
  await flush();
  assert.equal(h.player.getState().phase, 'key');
  assert.equal(h.player.getState().keyStatus, 'requesting');
  h.license('a');
  await loading;
  assert.equal(h.player.getState().phase, 'ready');
  assert.equal(h.deadlines.size, 0);
  assert.equal(h.player.getState().keyStatus, 'configured');
  for (const callback of h.videoListeners.get('playing')) callback();
  assert.equal(h.player.getState().status, 'playing');
  assert.equal(h.player.getState().keyStatus, 'usable');
  await h.player.stop();
});

test('an expired native teardown restores controls with reload guidance and retains its owner', async () => {
  const h = playbackHarness('en');
  const play = h.element('play-old-btn');
  play.dispatch('click');
  await flush();
  h.license('a');
  await flush();
  const destruction = h.instances[0].pendingDestroy = deferred();
  play.dispatch('click');
  await flush();
  assert.equal(play.disabled, true);
  h.advance(20000);
  await flush();
  assert.equal(play.disabled, false);
  assert.ok(h.element('old-state').textContent.includes(h.i18n.t('playbackCleanupUnconfirmed')));
  assert.equal(h.instances.length, 1);
  destruction.resolve();
  await flush();
  h.playback.hide();
  await flush();
});


test('a Shaka DRM error event settles a still-pending setup and retains its safe numeric code', async () => {
  const h = harness();
  let outcome;
  h.player.load('a', '/a').catch(error => { outcome = error; });
  await flush();
  h.instances[0].pendingLoad = deferred();
  h.license('a');
  await flush();
  h.instances[0].emit('error', { category: 6, code: 6001 });
  await flush();
  assert.equal(outcome?.code, 6001);
  assert.equal(h.player.getState().keyStatus, 'error');
  assert.match(h.player.getState().lastError.message, /6001/);
  assert.equal(h.deadlines.size, 0);
});
