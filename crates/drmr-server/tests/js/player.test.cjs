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
    setInterval(callback) { timers.add(callback); return callback; },
    clearInterval(callback) { timers.delete(callback); },
    fetch(url) {
      const request = deferred();
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
  return { player, instances, timers, videoListeners, requests, events, license };
}

test('stop during the license fetch leaves the player idle without rearming listeners or timers', async () => {
  const h = harness();
  const loading = h.player.load('a', '/a').catch(() => {});
  await flush();
  await h.player.stop();
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
