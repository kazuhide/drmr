import assert from 'node:assert/strict';
import { readFile, lstat } from 'node:fs/promises';
import { resolve, relative, isAbsolute } from 'node:path';
import { fileURLToPath } from 'node:url';
import { consoleIdentity, requireConsoleCi } from './console-source.mjs';

export function validateLifecycle(receipt, identity) {
  assert.equal(receipt.passed, true);
  for (const [key, value] of Object.entries(identity)) assert.deepEqual(receipt[key], value, key);
  assert.deepEqual(receipt.status, { code: 0, signal: null });
  assert.equal(receipt.serverReaped, true);
  assert.equal(receipt.ownedTemporaryRemoved, true);
  assert.ok(Array.isArray(receipt.fixtureRequests));
  assert.ok(receipt.fixtureRequests.length >= 2 && receipt.fixtureRequests.length <= 128);
  for (const request of receipt.fixtureRequests) {
    assert.deepEqual(Object.keys(request).sort(), ['authenticated', 'resource']);
    assert.equal(request.authenticated, true);
    assert.match(request.resource, /^(media\.m3u8|segment-[0-9]{3}\.ts)$/);
  }
  assert.ok(receipt.fixtureRequests.some(request => request.resource === 'media.m3u8'));
  assert.ok(receipt.fixtureRequests.some(request => request.resource.endsWith('.ts')));
}

export async function requireAbsent(path) {
  try { await lstat(path); } catch (error) {
    if (error.code === 'ENOENT') return;
    throw error;
  }
  throw new Error('An owned qualification resource remains after shutdown');
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  requireConsoleCi();
  const identity = consoleIdentity();
  const report = resolve(process.env.PW_REPORT_ROOT);
  await requireAbsent(resolve(report, 'lifecycle-failed.json'));
  const receipt = JSON.parse(await readFile(resolve(report, 'lifecycle.json'), 'utf8'));
  const fixture = JSON.parse(await readFile(resolve(report, 'fixture.json'), 'utf8'));
  validateLifecycle(receipt, identity);
  for (const [key, value] of Object.entries(identity)) assert.deepEqual(fixture[key], value, key);
  const ownerPath = relative(resolve(process.env.RUNNER_TEMP), fixture.root);
  assert.ok(ownerPath && !ownerPath.startsWith('..') && !isAbsolute(ownerPath));
  await requireAbsent(fixture.root);
  assert.ok(Number.isSafeInteger(fixture.serverPid) && fixture.serverPid > 0);
  let processError;
  try { process.kill(fixture.serverPid, 0); } catch (error) { processError = error; }
  assert.equal(processError?.code, 'ESRCH');
  console.log('Console lifecycle: exact source and binary, authenticated retrieval, server exit and owned cleanup confirmed');
}
