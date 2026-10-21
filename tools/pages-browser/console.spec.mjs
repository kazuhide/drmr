import { test, expect } from '@playwright/test';
import AxeBuilder from '@axe-core/playwright';
import { readFile, writeFile, readdir, lstat, unlink } from 'node:fs/promises';
import { spawn } from 'node:child_process';
import { randomBytes } from 'node:crypto';
import { join } from 'node:path';
import { allowedConsoleRequest, consoleOrigin, fixtureOrigin, digest, consoleAssets } from './console-source.mjs';

const id = () => randomBytes(16).toString('hex');
const privateValues = ['00112233445566778899aabbccddeeff', 'ffeeddccbbaa99887766554433221100',
  '102132435465768798a9bacbdcedfe0f', 'private-browser-marker', 'private-browser-auth-marker'];
let fixture, caps;
const pages = new WeakMap();
async function api(request, path, { method = 'GET', data, expected = 200 } = {}) {
  const response = await request.fetch(consoleOrigin + path, { method, data,
    headers: { Origin: consoleOrigin, ...(data === undefined ? {} : { 'Content-Type': 'application/json' }) } });
  expect(response.status(), path).toBe(expected);
  if (expected === 204) return null;
  return response.json();
}
async function cli(arguments_, expected = 0) {
  const started = Date.now();
  const child = spawn(process.env.DRMR_CONSOLE_BINARY, arguments_, { stdio: ['ignore', 'pipe', 'pipe'] });
  const chunks = [], errors = [];
  child.stdout.on('data', bytes => chunks.push(bytes)); child.stderr.on('data', bytes => errors.push(bytes));
  const timer = setTimeout(() => child.kill('SIGTERM'), 60_000);
  const status = await new Promise((resolve, reject) => {
    child.once('error', reject); child.once('exit', (code, signal) => resolve({ code, signal }));
  }).finally(() => clearTimeout(timer));
  expect(status).toEqual({ code: expected, signal: null });
  const bytes = Buffer.concat(chunks), stderr = Buffer.concat(errors).toString('utf8');
  for (const key of privateValues.slice(0, 2)) expect(bytes.toString('utf8') + stderr).not.toContain(key);
  return { report: JSON.parse(bytes), sha256: digest(bytes), elapsedMs: Date.now() - started };
}
function checks(report) {
  expect(report.passed).toBe(true); expect(report.media_verified).toBe(true);
  expect(report.license_playback_verified).toBe(false);
  expect(report.checks).toEqual({ stream_metadata: 'pass', packet_payloads_equal: true,
    packet_timing_equal: true, decoded_content_equal: true });
}
async function selectedSession(page) {
  const sessions = await api(page.request, '/api/common/sessions');
  expect(sessions.sessions).toHaveLength(1);
  return sessions.sessions[0];
}
async function observedJob(page, selector, operation, { expected = 'succeeded', wait = true } = {}) {
  const submitted = page.waitForResponse(response => response.request().method() === 'POST' &&
    new URL(response.url()).pathname === '/api/common/jobs');
  await expect(page.locator(selector)).toBeEnabled(); await page.locator(selector).click();
  const response = await submitted;
  expect(response.status()).toBe(202);
  let job = await response.json();
  expect(job.operation).toBe(operation);
  if (!wait) return job;
  job = await terminalJob(page, job, expected);
  await renderedJob(page, job);
  return job;
}
async function terminalJob(page, initial, expected = 'succeeded') {
  let job = initial;
  await expect.poll(async () => {
    job = await api(page.request, '/api/common/jobs/' + job.job_id); return job.state;
  }, { timeout: 60_000 }).not.toBe('running');
  expect(job.state).toBe(expected); expect(job.cleanup_confirmed).toBe(true);
  expect(job.supervision_confirmed).toBe(true); expect(job.exit_code).not.toBeNull();
  if (expected === 'succeeded') expect(job.exit_code).toBe(0);
  pages.get(page).jobs.push({ ...job });
  if (job.report_ref) {
    const report = await api(page.request, '/api/common/reports/' + job.report_ref);
    expect(report.job_id).toBe(job.job_id); expect(report.operation).toBe(job.operation);
    expect(report.request_id).toBe(job.request_id); expect(report.session_id).toBe(job.session_id);
    expect(report.plan_id).toBe(job.plan_id); job.observedReport = report;
  }
  return job;
}
function booleanState(value) { return value === true ? 'pass' : value === false ? 'fail' : 'unknown'; }
function nativeState(report) {
  if (!report) return 'not-checked';
  if (report.errors > 0) return 'fail';
  if (report.incomplete !== false || report.truncated !== false) return 'incomplete';
  return report.warnings > 0 ? 'warning' : 'pass';
}
async function renderedJob(page, job, { timeout = 15_000 } = {}) {
  const status = page.locator('#common-selected-status');
  for (const [name, value] of Object.entries({ 'job-id': job.job_id, 'request-id': job.request_id,
    operation: job.operation, 'job-state': job.state, 'plan-id': job.plan_id, 'plan-ref': job.plan_ref })) {
    if (value === null) await expect(status).not.toHaveAttribute('data-common-' + name, /./, { timeout });
    else await expect(status).toHaveAttribute('data-common-' + name, value, { timeout });
  }
  await expect(status).toHaveAttribute('data-common-outcome', job.state, { timeout });
  await expect(page.locator('#common-owner')).not.toBeVisible({ timeout });
  if (job.operation === 'plan' && job.state === 'succeeded') {
    await expect(page.locator('#common-plan-select')).toHaveValue(job.plan_ref, { timeout });
    await expect(page.locator('[aria-labelledby="common-plan-title"]')).toContainText(job.plan_id, { timeout });
    return;
  }
  const envelope = job.observedReport;
  if (!envelope) return;
  const host = page.locator('#common-' + ({ analyze: 'analysis', convert: 'conversion', verify: 'verification' }[job.operation]) + '-report');
  await expect(host).toBeVisible({ timeout });
  for (const [name, value] of Object.entries({ 'job-id': job.job_id, 'request-id': job.request_id,
    operation: job.operation, 'report-kind': envelope.report_kind, 'plan-id': job.plan_id, 'plan-ref': job.plan_ref })) {
    if (value === null) await expect(host).not.toHaveAttribute('data-common-' + name, /./, { timeout });
    else await expect(host).toHaveAttribute('data-common-' + name, value, { timeout });
  }
  const check = async (parent, name, state) => {
    const element = parent.locator('[data-common-check="' + name + '"]');
    await expect(element).toHaveCount(1, { timeout }); await expect(element).toBeVisible({ timeout });
    await expect(element).toHaveAttribute('data-common-state', state, { timeout });
  };
  const report = envelope.report;
  if (envelope.report_kind === 'analysis') await check(host, 'input_admitted', booleanState(report.input_admitted));
  if (['conversion', 'verification'].includes(envelope.report_kind)) {
    if (envelope.report_kind === 'verification') await check(host, 'source_unchanged', booleanState(report.source_unchanged));
    const rows = report.outputs || [report];
    await expect(host.locator('[data-common-output-index]')).toHaveCount(rows.length, { timeout });
    for (let index = 0; index < rows.length; index++) {
      const row = rows[index], block = host.locator('[data-common-output-index="' + index + '"]');
      if (row.id !== undefined) await expect(block).toHaveAttribute('data-common-output-id', row.id, { timeout });
      const verification = (row.conversion || row).verification;
      if (envelope.report_kind === 'verification') await check(block, 'output_matches_plan', booleanState(row.output_matches_plan));
      await check(block, 'media_verified', booleanState(verification.media_verified));
      await check(block, 'stream_metadata', verification.checks.stream_metadata === 'pass' ? 'pass' : verification.checks.stream_metadata === 'fail' ? 'fail' : 'unknown');
      for (const name of ['packet_payloads_equal', 'packet_timing_equal', 'decoded_content_equal']) await check(block, name, booleanState(verification.checks[name]));
      await check(block, 'license_playback_verified', 'not-checked');
      await check(block, 'native_manifest', nativeState(verification.native_manifest));
      if (Object.hasOwn(row, 'signaling')) await check(block, 'signaling', !row.signaling.systems.length ? 'not-requested' : booleanState(row.signaling.verified));
      if (Object.hasOwn(row, 'conformance')) await check(block, 'conformance', row.conformance === null ? 'not-requested' : booleanState(row.conformance.passed));
    }
  }
  if (report.error) await expect(host.locator('code').filter({ hasText: report.error.code })).toHaveCount(1, { timeout });
}
async function selectionFor(request, role, filename) {
  for (const root of caps.roots.filter(root => root.roles.includes(role))) {
    const query = new URLSearchParams({ root_ref: root.root_ref, role });
    const page = await api(request, '/api/common/files?' + query);
    if (filename === null) return { root, entry: null, parent: page.output_parent_ref };
    if (['keys', 'fetch-policy'].includes(role)) {
      // The fixture owns exactly these sorted files; private names never become UI labels.
      const files = fixture.configurationFiles;
      const index = files.indexOf(filename);
      expect(index).toBeGreaterThanOrEqual(0);
      expect(page.next_cursor).toBeNull();
      expect(page.entries.map(entry => [entry.kind, entry.label])).toEqual(
        files.map((_, ordinal) => ['file', 'Configuration ' + (ordinal + 1)]));
      return { root, entry: page.entries[index] };
    }
    const entry = page.entries.find(entry => entry.kind === 'file' && entry.label === filename);
    if (entry) return { root, entry };
  }
  throw new Error('Expected fixture selection was not exposed in its permitted root');
}
async function pick(page, role, filename) {
  const { root, entry, parent } = await selectionFor(page.request, role, filename);
  await page.locator('[data-common-pick="' + role + '"]').click();
  const dialog = page.locator('dialog.common-picker'); await expect(dialog).toBeVisible();
  await page.locator('#common-picker-root').selectOption(root.root_ref);
  if (filename === null) await page.locator('#common-picker-choose-folder').click();
  else await dialog.getByRole('button', { name: entry.label, exact: true }).click();
  await expect(dialog).not.toBeVisible();
  pages.get(page).selections[role] = filename === null ? parent : entry.file_ref;
}
async function openSession(page, kind = 'local', filename = 'source.mp4') {
  await page.locator('#common-new').click();
  await page.locator('#common-source-kind').selectOption(kind);
  if (kind === 'hls-url') {
    await page.locator('#common-url').fill(fixtureOrigin + '/media.m3u8?private-browser-auth-marker');
    await pick(page, 'fetch-policy', 'fetch.json');
  } else await pick(page, kind === 'local' ? 'media' : 'saved-plan', filename);
  await page.locator('#common-open').click();
  await expect(page.locator('#common-url')).toHaveValue('');
  await expect(page.locator('#common-workers')).toBeVisible();
  return selectedSession(page);
}
async function outputRow(page, index, { name, format, target = 'clear', drms = [], conformance = 'none' }) {
  const row = page.locator('tr[data-output-row]').nth(index);
  await row.locator('input[id$="-id"]').fill(name);
  await row.locator('select[id$="-format"]').selectOption(format);
  await row.locator('select[id$="-target"]').selectOption(target);
  const labels = { widevine: 'Widevine', playready: 'PlayReady', fairplay: 'FairPlay' };
  for (const drm of drms) await row.getByRole('checkbox', { name: new RegExp(labels[drm]) }).check();
  await row.locator('select[id$="-conformance"]').selectOption(conformance);
}
async function configure(page, destination, encrypted = false, conformance = 'none') {
  await outputRow(page, 0, { name: 'dash', format: 'dash', target: encrypted ? 'cbcs' : 'clear',
    drms: encrypted ? ['widevine', 'playready'] : [], conformance });
  await page.locator('#common-add-output').click();
  await outputRow(page, 1, { name: 'hls', format: 'hls', target: encrypted ? 'cbcs' : 'clear', drms: encrypted ? ['fairplay'] : [] });
  await pick(page, 'output-parent', null); await page.locator('#common-destination').fill(destination);
}
async function plan(page) {
  const job = await observedJob(page, '#common-save-plan', 'plan');
  await expect(page.locator('#common-export')).toBeVisible();
  return { job, value: await api(page.request, '/api/common/plans/' + job.plan_ref) };
}
async function reportAttachment(info, name, value) {
  const path = info.outputPath(name + '.json'); await writeFile(path, JSON.stringify(value, null, 2) + '\n');
  await info.attach(name, { path, contentType: 'application/json' });
}
async function retainedPlanBytes(planId, expectedBytes) {
  const matches = [];
  let count = 0;
  async function walk(directory, depth) {
    expect(depth).toBeLessThan(12);
    for (const name of await readdir(directory)) {
      expect(++count).toBeLessThan(8192);
      const path = join(directory, name), metadata = await lstat(path);
      expect(metadata.isSymbolicLink()).toBe(false);
      if (metadata.isDirectory()) await walk(path, depth + 1);
      else if (metadata.isFile() && name.endsWith('.json') && metadata.size < 32 * 1024 * 1024) {
        const bytes = await readFile(path);
        let value; try { value = JSON.parse(bytes.toString('utf8')); } catch { continue; }
        if (value.plan_id === planId && value.source && [1, 2].includes(value.schema_version)) {
          expect(bytes.equals(expectedBytes)).toBe(true); matches.push(digest(bytes));
        }
      }
    }
  }
  await walk(fixture.output, 0); expect(matches.length).toBeGreaterThan(0);
  return matches;
}
test.beforeAll(async ({ request }) => {
  fixture = JSON.parse(await readFile(join(process.env.PW_REPORT_ROOT, 'fixture.json'), 'utf8'));
  caps = await api(request, '/api/common/capabilities');
  expect(caps.schema_version).toBe(1); expect(caps.worker_policy).toEqual({ maximum: 4, automatic: true });
  const saved = join(fixture.input, 'cli-v1.plan.json'), destination = join(fixture.output, 'cli-v1.mp4');
  const planned = await cli(['plan', fixture.progressive, '--to', 'clear', '--format', 'mp4', '--output', destination, '--save', saved, '--json']);
  expect(planned.report.schema_version).toBe(1);
  const converted = await cli(['convert', '--plan', saved, '--json']); checks(converted.report.verification);
  const named = join(fixture.root, 'cli-v2.outputs.json'), savedV2 = join(fixture.input, 'cli-v2.plan.json');
  await writeFile(named, JSON.stringify({ schema_version: 1, outputs: [
    { id: 'dash', format: 'dash', target: 'cbcs', drms: ['widevine', 'playready'], conformance: 'none' },
    { id: 'hls', format: 'hls', target: 'cbcs', drms: ['fairplay'], conformance: 'none' },
  ] }), { flag: 'wx', mode: 0o600 });
  const keys = join(fixture.config, 'keys.json');
  const plannedV2 = await cli(['plan', join(fixture.source, 'media.m3u8'), '--outputs', named,
    '--output', join(fixture.output, 'cli-v2'), '--save', savedV2, '--keys-file', keys, '--json']);
  expect(plannedV2.report.schema_version).toBe(2);
  const convertedV2 = await cli(['convert', '--plan', savedV2, '--keys-file', keys, '--json']);
  for (const output of convertedV2.report.outputs) checks(output.conversion.verification);
  await writeFile(join(process.env.PW_REPORT_ROOT, 'cli-import-setup.json'), JSON.stringify({
    sourceHead: fixture.sourceHead, binarySha256: fixture.binarySha256, planId: planned.report.plan_id,
    plannedSha256: planned.sha256, convertedSha256: converted.sha256, checks: converted.report.verification.checks,
    v2: { planId: plannedV2.report.plan_id, plannedSha256: plannedV2.sha256, convertedSha256: convertedV2.sha256 } }, null, 2));
});
test.beforeEach(async ({ page, context }) => {
  const record = { errors: [], console: [], assets: {}, responseErrors: [], privacyLeaks: [], jobs: [], submitted: [], selections: {} }, pending = [];
  pages.set(page, record);
  page.on('pageerror', error => record.errors.push(error.message));
  page.on('console', message => { if (['error', 'warning'].includes(message.type())) record.console.push(message.text()); });
  page.on('request', request => {
    if (request.method() === 'POST' && new URL(request.url()).pathname === '/api/common/jobs') {
      const body = request.postDataJSON();
      expect(body.runtime.keys_ref).toBe(record.selections.keys || null);
      if (body.operation.kind === 'plan') expect(body.operation.output_parent_ref).toBe(record.selections['output-parent']);
      record.submitted.push({ requestId: body.request_id, sessionId: body.session_id, operation: body.operation.kind });
    }
    if (request.method() === 'POST' && new URL(request.url()).pathname === '/api/common/sessions') {
      const body = request.postDataJSON();
      if (body.source.kind === 'hls-url') expect(body.source.fetch_policy_ref).toBe(record.selections['fetch-policy'] || null);
    }
  });
  page.on('response', response => {
    const url = new URL(response.url());
    const asset = url.pathname === '/' ? 'index.html' : url.pathname.startsWith('/static/') ? url.pathname.slice(8) : null;
    if (consoleAssets.includes(asset)) pending.push(response.body().then(bytes => { record.assets[asset] = digest(bytes); })
      .catch(() => record.responseErrors.push('asset-body-unavailable')));
    if (url.pathname.startsWith('/api/common/')) pending.push(response.body().then(bytes => {
      const text = bytes.toString('utf8');
      for (const value of privateValues) if (text.includes(value)) record.privacyLeaks.push(value);
    }).catch(() => record.responseErrors.push('common-response-body-unavailable')));
  });
  record.pending = pending;
  await context.route('**/*', route => allowedConsoleRequest(route.request().url()) ? route.continue() : route.abort('blockedbyclient'));
  await page.goto('/'); await expect(page.locator('#common-title')).toBeVisible();
  await expect(page.locator('#common-open')).toBeDisabled();
});
test.afterEach(async ({ page }, info) => {
  const record = pages.get(page);
  try {
    await Promise.all(record.pending);
    expect(record.errors).toEqual([]);
    expect(record.responseErrors).toEqual([]); expect(record.privacyLeaks).toEqual([]);
    const consoleErrors = record.expectedLostResponse ? record.console.filter(message => message !== 'Failed to load resource: net::ERR_FAILED') : record.console;
    expect(consoleErrors).toEqual([]);
    if (record.expectedLostResponse) expect(record.console.length).toBeLessThanOrEqual(1);
    expect(record.assets).toEqual(info.config.metadata.assetHashes);
    const text = await page.locator('body').innerText();
    const storage = await page.evaluate(() => JSON.stringify({ local: { ...localStorage }, session: { ...sessionStorage } }));
    for (const value of privateValues) expect(text + storage + record.console.join('\n')).not.toContain(value);
  } finally {
    const sessions = await api(page.request, '/api/common/sessions');
    for (const session of sessions.sessions) {
      for (const requestId of session.request_ids) {
        const receipt = await api(page.request, '/api/common/sessions/' + session.session_id + '/requests/' + requestId);
        if (receipt.job_id) {
          let job = await api(page.request, '/api/common/jobs/' + receipt.job_id);
          if (job.state === 'running') {
            await api(page.request, '/api/common/jobs/' + job.job_id + '/stop', { method: 'POST', data: {}, expected: 202 });
            await expect.poll(async () => { job = await api(page.request, '/api/common/jobs/' + job.job_id); return job.cleanup_confirmed; }, { timeout: 15_000 }).toBe(true);
          }
        }
      }
      await api(page.request, '/api/common/sessions/' + session.session_id, { method: 'DELETE', expected: 204 });
    }
    await reportAttachment(info, 'console-observation', { ...record, pending: undefined,
      sourceHead: fixture.sourceHead, sourceTree: fixture.sourceTree, binarySha256: fixture.binarySha256 });
  }
});

for (const language of ['en', 'ja']) for (const width of [320, 599, 600, 601, 768, 899, 900, 901, 1024, 1440]) {
  test('common forms, keyboard and accessibility / ' + language + ' / ' + width, async ({ page }, info) => {
    await page.setViewportSize({ width, height: 1000 });
    await page.locator('#language-select').selectOption(language);
    await openSession(page);
    const polled = page.waitForResponse(response => new URL(response.url()).pathname === '/api/common/capabilities' && response.request().method() === 'GET');
    const input = page.locator('#common-destination'); await input.fill('draft-in-progress'); await input.focus();
    await input.evaluate(element => { window.__commonFocusedInput = element; });
    await polled;
    expect(await input.evaluate(element => element === window.__commonFocusedInput && document.activeElement === element)).toBe(true);
    await page.locator('#language-select').selectOption(language === 'en' ? 'ja' : 'en');
    await input.focus(); await page.keyboard.press('End'); await page.keyboard.type('-kept');
    await expect(input).toHaveValue('draft-in-progress-kept');
    await page.keyboard.press('Enter'); expect(pages.get(page).submitted).toEqual([]);
    expect(await input.evaluate(element => element === window.__commonFocusedInput && document.activeElement === element)).toBe(true);
    const layout = await page.evaluate(() => ({ viewport: innerWidth, document: document.documentElement.scrollWidth,
      width: document.documentElement.clientWidth, input: document.querySelector('#common-destination').getBoundingClientRect().toJSON() }));
    expect(layout.document).toBeLessThanOrEqual(layout.width + 1); expect(layout.input.width).toBeGreaterThan(100);
    await page.keyboard.press('Tab'); expect(await input.evaluate(element => document.activeElement !== element)).toBe(true);
    const accessibility = await new AxeBuilder({ page }).analyze(); expect(accessibility.violations).toEqual([]);
    await page.screenshot({ path: info.outputPath('common-' + language + '-' + width + '.png'), fullPage: true });
    await reportAttachment(info, 'layout-and-axe', { layout, violations: accessibility.violations, language, width });
  });
}

for (const language of ['en', 'ja']) {
  test('actual four-step media workflow and exact plan export / ' + language, async ({ page }, info) => {
    const encrypted = language === 'en';
    await page.locator('#language-select').selectOption(language);
    await openSession(page, encrypted ? 'hls-url' : 'local');
    if (encrypted) await pick(page, 'keys', 'keys.json');
    const analysis = await observedJob(page, '#common-analyze', 'analyze');
    expect(analysis.observedReport.report.input_admitted).toBe(true);
    await configure(page, 'browser-' + language, encrypted);
    const planned = await plan(page); expect(planned.value.plan_schema_version).toBe(2);
    expect(planned.value.outputs).toHaveLength(2);
    const downloadPromise = page.waitForEvent('download'); await page.locator('#common-export').click();
    const download = await downloadPromise, bytes = await readFile(await download.path());
    const saved = join(fixture.input, 'browser-' + language + '.plan.json');
    await writeFile(saved, bytes, { flag: 'wx', mode: 0o600 });
    const parsed = JSON.parse(bytes.toString('utf8')); expect(parsed.plan_id).toBe(planned.value.plan_id);
    const retainedHashes = await retainedPlanBytes(planned.value.plan_id, bytes);
    const converted = await observedJob(page, '#common-convert', 'convert');
    const verified = await observedJob(page, '#common-verify', 'verify');
    expect(converted.plan_id).toBe(planned.value.plan_id); expect(verified.plan_id).toBe(planned.value.plan_id);
    expect(verified.observedReport.report.source_unchanged).toBe(true);
    for (const output of verified.observedReport.report.outputs) {
      checks(output.verification); expect(output.output_matches_plan).toBe(true);
      expect(output.conformance).toBeNull();
      expect(output.signaling).toEqual(encrypted ? { systems: output.id === 'dash' ? ['widevine', 'playready'] : ['fairplay'], verified: true } : { systems: [], verified: false });
    }
    const outside = await cli(['verify', '--plan', saved, ...(encrypted ? ['--keys-file', join(fixture.config, 'keys.json')] : []), '--json']);
    expect(outside.report.plan_id).toBe(planned.value.plan_id); expect(outside.report.passed).toBe(true);
    await reportAttachment(info, 'real-media-and-cli-plan', { planId: planned.value.plan_id,
      exportSha256: digest(bytes), retainedHashes, outsideCliSha256: outside.sha256, verified: verified.observedReport });
    await page.screenshot({ path: info.outputPath('verified-' + language + '.png'), fullPage: true });
  });
}

test('CLI version-one saved plan can be independently verified without conversion', async ({ page }) => {
  await openSession(page, 'saved-plan', 'cli-v1.plan.json');
  await expect(page.locator('#common-analyze')).not.toBeVisible();
  await expect(page.locator('#common-convert')).toBeDisabled();
  const verified = await observedJob(page, '#common-verify', 'verify');
  checks(verified.observedReport.report.verification);
  expect(pages.get(page).submitted.map(item => item.operation)).toEqual(['verify']);
});

test('a lost launch response is recovered with the original request ID without resubmission', async ({ page }) => {
  await openSession(page);
  const record = pages.get(page); record.expectedLostResponse = true;
  let original;
  await page.route('**/api/common/jobs', async route => {
    if (route.request().method() !== 'POST') { await route.continue(); return; }
    expect(original).toBeUndefined(); original = route.request().postDataJSON();
    const result = await route.fetch(); expect(result.status()).toBe(202);
    await route.abort('failed');
  });
  await page.locator('#common-analyze').click();
  await expect(page.locator('#common-global-check')).toBeVisible();
  await page.locator('#common-global-check').click();
  let recovered;
  await expect.poll(async () => {
    if (!original) return false;
    const receipt = await api(page.request, '/api/common/sessions/' + original.session_id + '/requests/' + original.request_id);
    recovered = await api(page.request, '/api/common/jobs/' + receipt.job_id); return recovered.state === 'succeeded' && recovered.cleanup_confirmed;
  }, { timeout: 30_000 }).toBe(true);
  expect(record.submitted).toHaveLength(1); expect(record.submitted[0].requestId).toBe(original.request_id);
  recovered = await terminalJob(page, recovered);
  await renderedJob(page, recovered);
});

for (const failure of ['delayed', 'refused']) {
  test('browser success requires its own bound result artifact / ' + failure, async ({ page }, info) => {
    await openSession(page);
    let release, intercepted = false;
    const held = new Promise(resolve => { release = resolve; });
    if (failure === 'refused') pages.get(page).expectedLostResponse = true;
    await page.route('**/api/common/reports/*', async route => {
      const actual = await route.fetch(); expect(actual.status()).toBe(200);
      intercepted = true;
      if (failure === 'delayed') { await held; await route.fulfill({ response: actual }); }
      else await route.abort('failed');
    });
    try {
      const launched = await observedJob(page, '#common-analyze', 'analyze', { wait: false });
      const job = await terminalJob(page, launched);
      await expect.poll(() => intercepted, { timeout: 15_000 }).toBe(true);
      await expect(page.locator('#common-selected-status')).toHaveAttribute('data-common-outcome', failure === 'delayed' ? 'collecting' : 'unavailable');
      await expect(page.locator('#common-analysis-report')).not.toHaveAttribute('data-common-job-id', /./);
      await expect(renderedJob(page, job, { timeout: 250 })).rejects.toThrow();
      if (failure === 'delayed') release();
      else {
        await page.unroute('**/api/common/reports/*');
        await page.locator('#common-result-refresh').click();
      }
      await renderedJob(page, job);
      expect(pages.get(page).submitted).toHaveLength(1);
      await reportAttachment(info, 'artifact-negative-control', { failure, jobId: job.job_id,
        requestId: job.request_id, rejectedPrematureSuccess: true, recoveredWithoutResubmission: true });
    } finally { release(); }
  });
}

test('changed analyzed source is refused before a new plan can be used', async ({ page }) => {
  await openSession(page); await observedJob(page, '#common-analyze', 'analyze');
  await configure(page, 'changed-source');
  const original = await readFile(fixture.progressive);
  try {
    await writeFile(fixture.progressive, Buffer.concat([original, Buffer.from([0])]));
    const pending = page.waitForResponse(response => response.request().method() === 'POST' && new URL(response.url()).pathname === '/api/common/jobs');
    await page.locator('#common-save-plan').click(); const response = await pending;
    if (response.status() === 202) {
      let job = await response.json();
      await expect.poll(async () => { job = await api(page.request, '/api/common/jobs/' + job.job_id); return job.state; }, { timeout: 30_000 }).not.toBe('running');
      expect(job.state).toBe('failed'); expect(job.cleanup_confirmed).toBe(true);
      expect(job.plan_ref).toBeNull(); expect(job.error.code).toBe('analysis-source-changed');
    } else {
      expect(response.status()).toBe(409); expect((await response.json()).error.code).toBe('analysis-source-changed');
    }
    await expect(page.locator('#common-export')).not.toBeVisible();
    await expect(page.locator('#common-convert')).toBeDisabled();
  } finally { await writeFile(fixture.progressive, original); }
});

test('an imported version-two plan verifies current media and refuses a wrong key', async ({ page }) => {
  await openSession(page, 'saved-plan', 'cli-v2.plan.json');
  await expect(page.locator('#common-convert')).toBeDisabled();
  await pick(page, 'keys', 'wrong-keys.json');
  const failed = await observedJob(page, '#common-verify', 'verify', { expected: 'failed' });
  expect(failed.exit_code).not.toBe(0); expect(failed.observedReport.report.passed).not.toBe(true);
  await pick(page, 'keys', 'keys.json');
  const verified = await observedJob(page, '#common-verify', 'verify');
  expect(verified.plan_id).toBe(failed.plan_id);
  for (const output of verified.observedReport.report.outputs) checks(output.verification);
  expect(pages.get(page).submitted.map(item => item.operation)).toEqual(['verify', 'verify']);
});

test('a required unavailable manifest validator fails conversion without publication', async ({ page }) => {
  await openSession(page); await observedJob(page, '#common-analyze', 'analyze');
  await configure(page, 'required-validator');
  await page.locator('tr[data-output-row]').nth(1).locator('select[id$="-conformance"]').selectOption('required');
  await plan(page);
  const failed = await observedJob(page, '#common-convert', 'convert', { expected: 'failed' });
  expect(failed.observedReport.report.error.code).toBe('manifest-validator-unavailable');
  await expect(lstat(join(fixture.output, 'required-validator'))).rejects.toHaveProperty('code', 'ENOENT');
});

test('a full finished session closes before a fresh verify-only import', async ({ page }) => {
  const session = await openSession(page);
  for (let ordinal = 0; ordinal < 32; ordinal++) {
    const requestId = id();
    const response = await page.request.post(consoleOrigin + '/api/common/jobs', { headers: { Origin: consoleOrigin },
      data: { schema_version: 1, server_id: caps.server_id, session_id: session.session_id, request_id: requestId,
        operation: { kind: 'verify', plan_ref: id() }, runtime: { keys_ref: null, max_workers: null } } });
    expect(response.status()).toBe(404); expect((await response.json()).error.code).toBe('reference-not-found');
    const receipt = await api(page.request, '/api/common/sessions/' + session.session_id + '/requests/' + requestId);
    expect(receipt.state).toBe('rejected'); expect(receipt.job_id).toBeNull();
  }
  const full = await api(page.request, '/api/common/sessions/' + session.session_id);
  expect(full.requests_used).toBe(32);
  await page.reload(); await page.locator('#common-sessions .row-main').click();
  await expect(page.locator('#common-analyze')).toBeDisabled();
  await page.locator('#common-close').click(); await expect(page.locator('#common-open')).toBeVisible();
  await openSession(page, 'saved-plan', 'cli-v1.plan.json');
  const verified = await observedJob(page, '#common-verify', 'verify'); checks(verified.observedReport.report.verification);
  expect(pages.get(page).submitted.filter(item => item.operation === 'convert')).toEqual([]);
});

for (const operation of ['analyze', 'plan', 'convert', 'verify']) {
  test('Stop retains ownership until actual cleanup / ' + operation, async ({ page }, info) => {
    if (operation === 'verify') await openSession(page, 'saved-plan', 'cli-v1.plan.json');
    else {
      await openSession(page);
      if (operation !== 'analyze') {
        await observedJob(page, '#common-analyze', 'analyze'); await configure(page, 'stop-' + operation);
        if (operation === 'convert') await plan(page);
      }
    }
    const hold = join(fixture.tools, 'hold.json'), observed = join(fixture.tools, 'observed.json'), token = id();
    await unlink(observed).catch(error => { if (error.code !== 'ENOENT') throw error; });
    await writeFile(hold, JSON.stringify({ token }), { flag: 'wx', mode: 0o600 });
    let job, observer;
    try {
      job = await observedJob(page, '#common-' + ({ plan: 'save-plan' }[operation] || operation), operation, { wait: false });
      await expect.poll(async () => {
        try { observer = JSON.parse(await readFile(observed, 'utf8')); return observer.token === token; } catch { return false; }
      }, { timeout: 15_000 }).toBe(true);
      await expect(page.locator('#common-global-stop')).toBeVisible();
      await page.locator('#common-global-stop').click();
      await expect.poll(async () => { job = await api(page.request, '/api/common/jobs/' + job.job_id); return job.cleanup_confirmed; }, { timeout: 15_000 }).toBe(true);
      expect(job.stop_requested).toBe(true); expect(job.state).toBe('interrupted');
      expect(job.operation).toBe(operation); expect(job.supervision_confirmed).toBe(true);
      expect(job.exit_code).not.toBeNull(); expect(job.exit_code).not.toBe(0);
      let processError;
      try { process.kill(observer.pid, 0); } catch (error) { processError = error; }
      expect(processError?.code).toBe('ESRCH');
      job = await terminalJob(page, job, 'interrupted');
      await renderedJob(page, job);
      await reportAttachment(info, 'native-stop', { operation, observer, job });
    } finally { await unlink(hold); }
  });
}
