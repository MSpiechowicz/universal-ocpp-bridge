import { test, expect } from '@playwright/test';

const endpoint = '/api/v1/diagnostics/capture';
const headers = { Authorization: 'Bearer uob1.production.browser-fixture-diagnostics-production-secret' };
test.beforeEach(async ({ request }) => {
  const active = await request.get(endpoint, { headers });
  if (active.ok()) {
    const capture = await active.json();
    await request.post(`${endpoint}/${capture.identity.runtime.process_instance_id}/${capture.id}/stop`, { headers });
  }
});

test('opening Debug is inert; explicit scoped capture streams, pauses, expires and clears', async ({ page, request }) => {
  const controls: string[] = [];
  page.on('request', request => { if (request.method() === 'POST') controls.push(request.url()); });
  await page.goto('/#debug');
  await expect(page.getByRole('heading', { name: 'Debug timeline' })).toBeVisible();
  expect((await request.get(endpoint, { headers })).status()).toBe(410);
  expect(controls).toEqual([]);
  await page.getByLabel('Diagnostic credential', { exact: true }).fill('uob1.production.browser-fixture-diagnostics-production-secret');
  await page.getByRole('button', { name: 'Inspect capture status' }).click();
  await expect(page.getByRole('button', { name: 'Start capture on production' })).toBeDisabled();
  expect(controls).toEqual([]);
  await page.getByLabel('Capture station', { exact: true }).fill('station-browser-fixture');
  await page.getByLabel('Capture seconds', { exact: true }).fill('4');
  await page.getByRole('checkbox', { name: /Confirm next control destination/ }).check();
  await page.getByRole('button', { name: 'Start capture on production' }).click();
  await expect(page.locator('.trace-row').first()).toBeVisible();
  await expect(page.locator('.debug-destination')).toContainText('PRODUCTION');
  await expect(page.locator('.debug-destination')).toContainText('release-browser-fixture');
  await page.getByRole('button', { name: 'Pause display', exact: true }).click();
  await expect(page.getByText('Display paused at trace', { exact: false })).toBeVisible();
  await expect(page.getByText('Capture deadline reached.', { exact: false })).toBeVisible({ timeout: 10000 });
  expect((await request.get(endpoint, { headers })).status()).toBe(410);
  await page.getByRole('button', { name: 'Resume display', exact: true }).click();
  expect(await page.locator('.trace-row').count()).toBeLessThanOrEqual(10);
  await page.locator('.trace-open').first().click();
  await expect(page.getByRole('region', { name: 'Trace detail' }).locator('pre')).toContainText('browser-synthetic-correlation');
  await page.getByRole('button', { name: 'Clear display buffer' }).click();
  await expect(page.locator('.trace-row')).toHaveCount(0);
  await page.getByRole('button', { name: 'Refresh capture status' }).click();
  await expect(page.getByRole('button', { name: 'Start capture on production' })).toBeDisabled();
  expect(controls).toHaveLength(1);
  await page.getByRole('button', { name: 'Disconnect diagnostics' }).click();
  expect(await page.evaluate(() => [localStorage.length, sessionStorage.length])).toEqual([0, 0]);
});

test('diagnostic reader cannot start and stop is explicit through the real capture API', async ({ page, request }) => {
  await page.goto('/#debug');
  await page.getByLabel('Diagnostic credential', { exact: true }).fill('uob1.production.browser-fixture-diagnostics-reader-production-secret');
  await page.getByRole('button', { name: 'Inspect capture status' }).click();
  await page.getByRole('checkbox', { name: /Confirm next control destination/ }).check();
  await page.getByRole('button', { name: 'Start capture on production' }).click();
  await expect(page.getByRole('alert')).toContainText('Access denied');
  expect((await request.get(endpoint, { headers })).status()).toBe(410);
  await page.getByRole('button', { name: 'Disconnect diagnostics' }).click();
  await page.getByLabel('Diagnostic credential', { exact: true }).fill('uob1.production.browser-fixture-diagnostics-production-secret');
  await page.getByRole('button', { name: 'Inspect capture status' }).click();
  await page.getByRole('checkbox', { name: /Confirm next control destination/ }).check();
  await page.getByRole('button', { name: 'Start capture on production' }).click();
  await expect(page.locator('.trace-row').first()).toBeVisible();
  await page.getByRole('checkbox', { name: /Confirm next control destination/ }).check();
  await page.getByRole('button', { name: 'Stop capture on production' }).click();
  await expect(page.getByText('Capture stopped. Displayed traces', { exact: false })).toBeVisible();
  await expect(page.getByRole('button', { name: 'Start capture on production' })).toBeDisabled();
  expect((await request.get(endpoint, { headers })).status()).toBe(410);
  await expect(page.getByRole('button', { name: /replay|fault|remote start/i })).toHaveCount(0);
  await page.screenshot({ path: 'test-results/debug-desktop.png', fullPage: true });
  await page.setViewportSize({ width: 390, height: 844 });
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true);
  await page.screenshot({ path: 'test-results/debug-mobile.png', fullPage: true });
});

test('high-volume hidden display retains bounded inert rows and virtualizes on visibility', async ({ page }) => {
  // Stress the actual compiled browser with a synthetic bounded-protocol stream; capture controls
  // still use the real server. This complements the real-producer tests above.
  await page.route('**/traces', async route => {
    const path = new URL(route.request().url()).pathname.split('/');
    const process = path.at(-3)!;
    const capture = Number(path.at(-2));
    const rows = Array.from({ length: 2600 }, (_, sequence) => {
      const record = { schema_version: { major: 1, revision: 0 }, trace_id: `stress-${sequence}`, process_instance_id: process, trace_sequence: sequence, stage: 'stress', direction: 'inbound', observed_at: '2026-09-12T12:00:00Z', outcome: { status: 'uncertain' }, redacted_details: { fields: { evidence: '<img src=x onerror=alert(1)>' } } };
      return `event: trace\nid: ${JSON.stringify([process, capture, sequence])}\ndata: ${JSON.stringify(record)}\n\n`;
    }).join('');
    await route.fulfill({ contentType: 'text/event-stream', body: rows });
  });
  await page.goto('/#debug');
  await page.getByLabel('Diagnostic credential', { exact: true }).fill('uob1.production.browser-fixture-diagnostics-production-secret');
  await page.getByRole('button', { name: 'Inspect capture status' }).click();
  await page.evaluate(() => { Object.defineProperty(document, 'hidden', { configurable: true, value: true }); document.dispatchEvent(new Event('visibilitychange')); });
  await page.getByRole('checkbox', { name: /Confirm next control destination/ }).check();
  await page.getByRole('button', { name: 'Start capture on production' }).click();
  // Ensure the event-loop has consumed the intentionally large response while render ticks skip.
  await page.waitForTimeout(1200);
  await page.evaluate(() => { Object.defineProperty(document, 'hidden', { configurable: true, value: false }); document.dispatchEvent(new Event('visibilitychange')); });
  await expect(page.getByText(/2000 retained.*600 browser evictions/)).toBeVisible();
  expect(await page.locator('.trace-row').count()).toBeLessThanOrEqual(10);
  await expect(page.locator('.trace-row').first()).toContainText('<img src=x onerror=alert(1)>');
  expect(await page.locator('img').count()).toBe(0);
  await page.getByRole('checkbox', { name: /Confirm next control destination/ }).check();
  await page.getByRole('button', { name: 'Stop capture on production' }).click();
  await expect(page.getByText('Capture stopped. Displayed traces', { exact: false })).toBeVisible();
});
