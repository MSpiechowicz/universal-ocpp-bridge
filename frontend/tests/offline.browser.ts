import { test, expect } from '@playwright/test';
import { captureLines, encodeCapture, trace } from './offline-fixture';

const headers = { Authorization: 'Bearer uob1.production.browser-fixture-diagnostics-production-secret' };
const endpoint = '/api/v1/diagnostics/capture';

test('real exported capture opens offline with provenance, correlations and no service requests', async ({ page, request, context }) => {
  const current = await request.get(endpoint, { headers });
  if (current.ok()) {
    const value = await current.json();
    await request.post(`${endpoint}/${value.identity.runtime.process_instance_id}/${value.id}/stop`, { headers });
  }
  const start = await request.post(endpoint, { headers, data: { station_id: 'station-browser-fixture', target_id: null, level: 'metadata', duration_seconds: 60 } });
  expect(start.ok()).toBe(true);
  const capture = await start.json();
  const path = `${endpoint}/${capture.identity.runtime.process_instance_id}/${capture.id}`;
  let exported = '';
  try {
    await expect.poll(async () => {
      const response = await request.get(`${path}/export`, { headers });
      expect(response.ok()).toBe(true); exported = await response.text();
      return exported.includes('"type":"trace"');
    }).toBe(true);
  } finally { await request.post(`${path}/stop`, { headers }); }
  const calls: string[] = [];
  page.on('request', request => { if (request.url().includes('/api/')) calls.push(request.url()); });
  await page.goto('/?offline=1');
  await context.setOffline(true);
  await page.getByLabel('Capture file', { exact: true }).setInputFiles({ name: 'capture.jsonl', mimeType: 'application/x-ndjson', buffer: Buffer.from(exported) });
  const view = page.getByRole('region', { name: 'Offline capture', exact: true });
  await expect(view).toContainText('PRODUCTION · OFFLINE');
  await expect(view).toContainText('sha256:browser-fixture');
  await expect(view).toContainText('station-browser-fixture');
  await expect(view.locator('.trace-row').first()).toBeVisible();
  await view.locator('.trace-open').first().click();
  await expect(view.getByRole('region', { name: 'Decision and trigger' })).toContainText('browser-synthetic-correlation');
  expect(calls).toEqual([]);
  expect(await page.evaluate(() => [localStorage.length, sessionStorage.length])).toEqual([0, 0]);
  expect(await page.getByRole('button', { name: /Start capture|Stop capture|Submit|Replay/ }).count()).toBe(0);
});

test('hostile content stays inert; filters, bookmarks, inspector, clear and mobile remain local', async ({ page }) => {
  const calls: string[] = [];
  page.on('request', request => { if (request.url().includes('/api/')) calls.push(request.url()); });
  page.on('dialog', () => { throw new Error('Imported text executed'); });
  await page.addInitScript(() => {
    // Fail any persistence attempt; imported data must remain in memory.
    Storage.prototype.setItem = () => { throw new Error('Persistence attempted'); };
    indexedDB.open = () => { throw new Error('IndexedDB attempted'); };
  });
  await page.goto('/?offline=1');
  await page.getByLabel('Capture file', { exact: true }).setInputFiles({ name: '<script>secret</script>.jsonl', mimeType: 'text/html', buffer: Buffer.from(encodeCapture()) });
  const view = page.getByRole('region', { name: 'Offline capture', exact: true });
  await expect(view).toContainText('STAGING · OFFLINE');
  await expect(view).toContainText('Missing sequences: 1');
  await expect(view).toContainText('Prior evictions: 2 · dropped: 3 · details shed: 4');
  await view.getByText('Filter retained traces', { exact: true }).click();
  await view.getByLabel('Filter correlation', { exact: true }).fill('offline-correlation');
  await expect(view.locator('.trace-row')).toHaveCount(2);
  await view.getByRole('button', { name: 'Bookmark trace 2', exact: true }).click();
  await view.getByRole('button', { name: 'Trace 2', exact: true }).click();
  await expect(view.getByRole('region', { name: 'Redacted source', exact: true })).toContainText('<img src=x onerror=alert(1)>');
  expect(await view.locator('img,script,iframe').count()).toBe(0);
  if (!process.env.UOB_BROWSER_REPORT_ONLY) await page.screenshot({ path: 'test-results/offline-desktop.png', fullPage: true });
  await page.setViewportSize({ width: 390, height: 844 });
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true);
  if (!process.env.UOB_BROWSER_REPORT_ONLY) await page.screenshot({ path: 'test-results/offline-mobile.png', fullPage: true });
  await page.getByRole('button', { name: 'Clear offline capture', exact: true }).click();
  await expect(view).toHaveCount(0);
  expect(calls).toEqual([]);
});

test('large valid imports virtualize; malformed replacement and cancellation clear prior data', async ({ page }) => {
  await page.goto('/?offline=1');
  const input = page.getByLabel('Capture file', { exact: true });
  await input.setInputFiles({ name: 'large.jsonl', mimeType: 'application/x-ndjson', buffer: Buffer.from(encodeCapture(captureLines(Array.from({ length: 2000 }, (_, i) => trace(i))))) });
  await expect(page.getByRole('region', { name: 'Offline capture', exact: true })).toBeVisible();
  expect(await page.locator('.trace-row').count()).toBeLessThanOrEqual(10);
  await input.setInputFiles({ name: 'bad.jsonl', mimeType: 'application/x-ndjson', buffer: Buffer.from('{"type":"manifest"}\n') });
  await expect(page.getByRole('alert')).toContainText('Capture rejected');
  await expect(page.locator('.trace-row')).toHaveCount(0);
  await expect(page.getByRole('region', { name: 'Offline capture', exact: true })).toHaveCount(0);
  await input.setInputFiles({ name: 'cancel.jsonl', mimeType: 'application/x-ndjson', buffer: Buffer.from(encodeCapture(captureLines(Array.from({ length: 180 }, (_, i) => trace(i, 'x'.repeat(48000)))))) });
  await page.getByRole('button', { name: /Clear offline capture/ }).click();
  await expect(page.getByRole('region', { name: 'Offline capture', exact: true })).toHaveCount(0);
  await expect(page.getByRole('alert')).toHaveCount(0);
});
