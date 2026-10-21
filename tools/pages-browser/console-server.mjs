import { access, chmod, copyFile, mkdir, mkdtemp, open, readFile, realpath, rm, stat, writeFile } from 'node:fs/promises';
import { constants } from 'node:fs';
import { spawn, execFileSync } from 'node:child_process';
import { createServer } from 'node:http';
import { join, resolve } from 'node:path';
import { once } from 'node:events';
import { consoleIdentity, consoleOrigin, fixtureOrigin, requireConsoleCi } from './console-source.mjs';

requireConsoleCi();
const identity = consoleIdentity();
const reportRoot = resolve(process.env.PW_REPORT_ROOT);
await mkdir(reportRoot, { recursive: true, mode: 0o700 });
const root = await mkdtemp(join(resolve(process.env.RUNNER_TEMP), 'drmr-console-'));
await chmod(root, 0o700);
const owned = await stat(root);
let child, ended, fixtureServer, log;
const requests = [];
try {
const input = join(root, 'input'), output = join(root, 'output'), config = join(root, 'config');
const tools = join(root, 'tools'), source = join(input, 'hls');
for (const path of [input, output, config, tools, source]) await mkdir(path, { mode: 0o700 });
const privateJson = async (path, value) => writeFile(path, JSON.stringify(value, null, 2) + '\n', { flag: 'wx', mode: 0o600 });
async function tool(name) {
  for (const folder of process.env.PATH.split(':')) {
    const path = join(folder, name);
    try { await access(path, constants.X_OK); return await realpath(path); } catch { /* Try the next PATH entry. */ }
  }
  throw new Error('A required real media tool is unavailable');
}
const realTools = Object.fromEntries(await Promise.all(['ffmpeg', 'ffprobe', 'mp4dump', 'packager'].map(async name => [name, await tool(name)])));
const fixture = resolve('../../crates/drmr/tests/fixtures/bbb/clip.mp4');
const progressive = join(input, 'source.mp4');
await copyFile(fixture, progressive);
await chmod(progressive, 0o600);
execFileSync(realTools.ffmpeg, ['-v', 'error', '-i', fixture, '-map', '0:v:0', '-map', '0:a:0', '-c', 'copy',
  '-f', 'hls', '-hls_time', '2', '-hls_playlist_type', 'vod', '-hls_segment_filename', join(source, 'segment-%03d.ts'),
  join(source, 'media.m3u8')], { timeout: 20_000, stdio: ['ignore', 'pipe', 'pipe'] });

// A test-only observer pauses before a real tool; it never fabricates media or results.
await privateJson(join(tools, 'real-tools.json'), realTools);
const wrapper = `#!/usr/bin/env node
const fs = require('node:fs');
const path = require('node:path');
const cp = require('node:child_process');
const base = path.dirname(process.argv[1]);
const name = path.basename(process.argv[1]);
const actual = JSON.parse(fs.readFileSync(path.join(base, 'real-tools.json'), 'utf8'))[name];
const gate = path.join(base, 'hold.json');
const observed = path.join(base, 'observed.json');
let stopped = false, child;
function stop(signal) { stopped = true; if (child) child.kill(signal); else process.exit(130); }
process.on('SIGTERM', () => stop('SIGTERM'));
process.on('SIGINT', () => stop('SIGINT'));
async function main() {
  if (fs.existsSync(gate)) {
    const control = JSON.parse(fs.readFileSync(gate, 'utf8'));
    const pending = observed + '.' + process.pid;
    fs.writeFileSync(pending, JSON.stringify({pid:process.pid,tool:name,token:control.token}), {mode:0o600});
    fs.renameSync(pending, observed);
    const until = Date.now() + 30000;
    while (!stopped && fs.existsSync(gate)) {
      if (Date.now() > until) process.exit(124);
      await new Promise(resolve => setTimeout(resolve, 20));
    }
  }
  if (stopped) return;
  child = cp.spawn(actual, process.argv.slice(2), {stdio:'inherit'});
  child.on('error', () => process.exit(127));
  child.on('exit', (code, signal) => { if (signal) process.exit(130); else process.exit(code ?? 1); });
}
main().catch(() => process.exit(1));
`;
for (const name of Object.keys(realTools)) {
  await writeFile(join(tools, name), wrapper, { flag: 'wx', mode: 0o700 });
}
const key = '00112233445566778899aabbccddeeff', kid = '102132435465768798a9bacbdcedfe0f';
const pssh = Buffer.alloc(52);
pssh.writeUInt32BE(52, 0); pssh.write('pssh', 4); pssh.writeUInt32BE(0x01000000, 8);
Buffer.from('edef8ba979d64acea3c827dcd51d21ed', 'hex').copy(pssh, 12);
pssh.writeUInt32BE(1, 28); Buffer.from(kid, 'hex').copy(pssh, 32);
await privateJson(join(config, 'keys.json'), { schema_version: 2, outputs: {
  dash: { kid, key, signaling: { system: 'multi-drm', widevine_pssh: pssh.toString('hex'), playready_license_url: 'https://license.example.invalid/private-browser-marker' } },
  hls: { kid, key, hls_key_uri: 'skd://private-browser-marker', signaling: { system: 'multi-drm', fairplay: true } },
} });
await privateJson(join(config, 'fetch.json'), { allowed_origins: [fixtureOrigin],
  headers_by_origin: { [fixtureOrigin]: { Authorization: 'Bearer private-browser-auth-marker' } },
  allow_http: true, allow_private_network: true, timeout: 3, connections: 2 });
await privateJson(join(config, 'wrong-keys.json'), { schema_version: 2, outputs: {
  dash: { kid, key: 'ffeeddccbbaa99887766554433221100', signaling: { system: 'multi-drm', widevine_pssh: pssh.toString('hex'), playready_license_url: 'https://license.example.invalid/private-browser-marker' } },
  hls: { kid, key: 'ffeeddccbbaa99887766554433221100', hls_key_uri: 'skd://private-browser-marker', signaling: { system: 'multi-drm', fairplay: true } },
} });
fixtureServer = createServer(async (request, response) => {
  try {
    const url = new URL(request.url, fixtureOrigin);
    const name = url.pathname.slice(1);
    if (request.method !== 'GET' || !/^(media\.m3u8|segment-[0-9]{3}\.ts)$/.test(name)) {
      response.writeHead(404).end(); return;
    }
    const authenticated = request.headers.authorization === 'Bearer private-browser-auth-marker';
    requests.push({ resource: name, authenticated });
    if (!authenticated) { response.writeHead(401).end(); return; }
    if (requests.length > 128) { response.writeHead(429).end(); return; }
    const bytes = await readFile(join(source, name));
    response.writeHead(200, { 'Content-Type': name.endsWith('m3u8') ? 'application/vnd.apple.mpegurl' : 'video/mp2t' });
    response.end(bytes);
  } catch { response.writeHead(500).end(); }
});
fixtureServer.listen(4175, '127.0.0.1');
await once(fixtureServer, 'listening');
log = await open(join(reportRoot, 'server.log'), 'wx', 0o600);
const binary = resolve(process.env.DRMR_CONSOLE_BINARY);
child = spawn(binary, ['serve'], { env: { ...process.env, PATH: tools + ':' + process.env.PATH,
  DRMR_CATALOG_ROOT: input, DRMR_OUTPUT_ROOT: output, DRMR_COMMON_CONFIG_ROOT: config,
  DRMR_BIND_HOST: '127.0.0.1', DRMR_BIND_PORT: '4174', DRMR_MAX_WORKERS: '4' },
  stdio: ['ignore', log.fd, log.fd] });
ended = new Promise(resolve => { child.once('exit', (code, signal) => resolve({ code, signal })); child.once('error', () => resolve({ code: null, signal: 'spawn-error' })); });
await privateJson(join(reportRoot, 'fixture.json'), { ...identity, root, input, output, config, tools,
  source, progressive, origin: consoleOrigin, fixtureOrigin, serverPid: child.pid,
  configurationFiles: ['fetch.json', 'keys.json', 'wrong-keys.json'],
  ownedIdentity: { uid: owned.uid, device: owned.dev, inode: owned.ino } });
let closing = false;
async function close(signal) {
  if (closing) return;
  closing = true;
  let status;
  try {
    child.kill(signal);
    status = await Promise.race([ended, new Promise((_, reject) => setTimeout(() => reject(new Error('Owned server did not confirm shutdown')), 25000).unref())]);
    if (status.code !== 0 || status.signal !== null) throw new Error('Owned server shutdown failed');
    await new Promise(resolve => fixtureServer.close(resolve));
    await log.close();
    const current = await stat(root);
    if (current.ino !== owned.ino || current.dev !== owned.dev || current.uid !== owned.uid || await realpath(root) !== root) {
      throw new Error('Fixture ownership changed');
    }
    await rm(root, { recursive: true });
    await privateJson(join(reportRoot, 'lifecycle.json'), { passed: true, ...identity, status,
      fixtureRequests: requests, ownedTemporaryRemoved: true, serverReaped: true });
    process.exit(0);
  } catch {
    await privateJson(join(reportRoot, 'lifecycle-failed.json'), { passed: false, ...identity,
      status: status || null, retainedOwnedTemporary: true });
    process.exit(1);
  }
}
process.on('SIGTERM', () => { void close('SIGTERM'); });
process.on('SIGINT', () => { void close('SIGTERM'); });
child.once('exit', () => { if (!closing) void close('SIGTERM'); });
console.log('Owned console qualification server started');
} catch {
  // A preparation failure also owns its resources; it never becomes a passing run.
  let status = null, serverReaped = !child, removed = false;
  try {
    if (child) {
      child.kill('SIGTERM');
      status = await Promise.race([ended, new Promise((_, reject) => setTimeout(() => reject(new Error('Startup child did not confirm exit')), 25000).unref())]);
      serverReaped = status.code === 0 && status.signal === null;
    }
    if (fixtureServer?.listening) await new Promise(resolve => fixtureServer.close(resolve));
    if (log) await log.close();
    const current = await stat(root);
    if (serverReaped && current.ino === owned.ino && current.dev === owned.dev &&
        current.uid === owned.uid && await realpath(root) === root) {
      await rm(root, { recursive: true }); removed = true;
    }
  } catch { /* Keep uncertain resources and identify the failure in the receipt. */ }
  await writeFile(join(reportRoot, 'lifecycle-failed.json'), JSON.stringify({ passed: false, ...identity,
    phase: 'preparation', status, serverReaped, ownedTemporaryRemoved: removed,
    retainedOwnedTemporary: !removed }, null, 2) + '\n', { flag: 'wx', mode: 0o600 });
  process.exit(1);
}
