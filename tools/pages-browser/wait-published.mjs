// Only publication propagation is polled. Browser assertions never retry.
import { mkdir, writeFile } from 'node:fs/promises';
import { resolve, join } from 'node:path';
import { pathToFileURL } from 'node:url';
import { assets, publicBase, source, digest, liveMode, requireCi } from './source.mjs';

export function matchesPublication(rows, hashes) {
  return rows.length === assets.length && assets.every(path => {
    const found = rows.filter(row => row.path === path);
    return found.length === 1 && found[0].status === 200 && found[0].sha256 === hashes[path];
  });
}

async function inspect(path, remaining) {
  try {
    const response = await fetch(publicBase + (path === 'index.html' ? '' : path), {
      redirect: 'error', cache: 'no-store', signal: AbortSignal.timeout(Math.max(1, Math.min(5000, remaining))),
      headers: { 'Cache-Control': 'no-cache', Pragma: 'no-cache' },
    });
    const reader = response.body.getReader();
    const chunks = [];
    let size = 0;
    try {
      for (;;) {
        const next = await reader.read();
        if (next.done) break;
        size += next.value.length;
        if (size > 1024 * 1024) throw new Error('Publication response exceeds the byte limit');
        chunks.push(next.value);
      }
    } finally {
      await reader.cancel();
    }
    return { path, status: response.status, sha256: digest(Buffer.concat(chunks)), bytes: size,
      content_type: response.headers.get('content-type'), cache_control: response.headers.get('cache-control') };
  } catch {
    return { path, status: null, sha256: null, error: 'publication-response-unavailable-or-invalid' };
  }
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  requireCi(true);
  if (!liveMode(process.env.PAGES_LIVE)) throw new Error('Publication polling requires the published mode');
  const identity = source();
  if (identity.sourceHead !== process.env.GITHUB_SHA) throw new Error('Checkout does not match the workflow source');
  const started = Date.now();
  const budget = 120_000;
  const history = [];
  let success = false;
  for (let pass = 1; pass <= 30 && Date.now() - started < budget; pass++) {
    const rows = await Promise.all(assets.map(path => inspect(path, budget - (Date.now() - started))));
    history.push({ pass, rows });
    if (matchesPublication(rows, identity.assetHashes)) { success = true; break; }
    await new Promise(resolve => setTimeout(resolve, Math.min(2000, Math.max(0, budget - (Date.now() - started)))));
  }
  const directory = resolve(process.env.PW_REPORT_ROOT);
  await mkdir(directory, { recursive: true });
  await writeFile(join(directory, 'published-bytes.json'), JSON.stringify({ schema_version: 1,
    ...identity, public_base: publicBase, success, budget_ms: budget, max_passes: 30,
    elapsed_ms: Date.now() - started, history }, null, 2) + '\n');
  if (!success) throw new Error('Published bytes did not match the workflow source within the bounded wait');
}
