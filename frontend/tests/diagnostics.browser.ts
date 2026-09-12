import { test, expect } from '@playwright/test';

const correlation = '12345678-1234-1234-1234-123456789abc';
test('failed API request links safe correlation to existing traces without starting capture', async ({ page }) => {
  const captures: string[] = [];
  page.on('request', request => { if (request.url().includes('/diagnostics/capture')) captures.push(request.url()); });
  await page.route('**/api/v1/stations?limit=10', route => route.fulfill({ status: 503,
    headers: { 'content-type': 'application/json', 'x-correlation-id': correlation }, body: '{"error":"password=private-body"}' }));
  await page.goto('/');
  await page.getByLabel('Management read credential').fill('uob1.production.browser-fixture-reader-production-secret');
  await page.getByRole('button', { name: 'Connect to production' }).click();
  const panel = page.getByRole('region', { name: 'Browser and API diagnostics' });
  await expect(panel).toContainText('inventory · 503');
  await expect(panel).toContainText(correlation);
  await expect(panel).not.toContainText('private-body');
  await page.getByRole('button', { name: 'Find in retained traces' }).click();
  await expect(page.getByLabel('Filter correlation', { exact: true })).toHaveValue(correlation);
  expect(captures).toEqual([]);
  expect(await page.evaluate(() => [localStorage.length, sessionStorage.length])).toEqual([0, 0]);
});

test('exception storm records bounded categories and panel does not recursively update', async ({ page }) => {
  await page.goto('/');
  await page.evaluate(() => {
    for (let i = 0; i < 10000; i++) window.dispatchEvent(new ErrorEvent('error', {
      message: 'password=private-exception', error: new Error('private-stack'), filename: 'https://user:secret@host',
    }));
    window.dispatchEvent(new PromiseRejectionEvent('unhandledrejection', { promise: Promise.resolve(), reason: 'private-rejection' }));
  });
  const panel = page.getByRole('region', { name: 'Browser and API diagnostics' });
  await expect(panel).toContainText('10001 · rejection');
  await expect(panel).not.toContainText('private');
  await expect(panel).not.toContainText('secret');
  const updates = panel.locator('dd').nth(2);
  await expect(updates).toHaveText('2 / 1');
  const before = await updates.textContent();
  await page.waitForTimeout(2200);
  await expect(updates).toHaveText(before!);
  await page.setViewportSize({ width: 390, height: 844 });
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true);
  await page.screenshot({ path: 'test-results/diagnostics-mobile.png', fullPage: true });
});

test('severed actual router SSE exposes stale state and counted reconnects', async ({ page }) => {
  await page.goto('/');
  await page.getByLabel('Management read credential').fill('uob1.production.browser-fixture-reader-production-secret');
  await page.getByRole('button', { name: 'Connect to production' }).click();
  await expect(page.getByText('Connection interrupted.', { exact: false })).toBeVisible();
  const panel = page.getByRole('region', { name: 'Browser and API diagnostics' });
  await expect(panel).toContainText('Event stream: stale');
  await expect(panel.locator('dd').nth(3)).toContainText('1 / 0');
  await expect(page.locator('.status')).toHaveText('live', { timeout: 10000 });
  await expect(panel.locator('dd').nth(4)).not.toHaveText('Unavailable');
});
