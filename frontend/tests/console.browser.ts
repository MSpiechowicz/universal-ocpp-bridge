import { test, expect } from '@playwright/test';

test('real router authenticates, resumes SSE and renders untrusted event text inertly', async ({ page }) => {
  const urls: string[] = [];
  const logs: string[] = [];
  const eventRequests: Record<string, string>[] = [];
  page.on('request', request => {
    urls.push(request.url());
    if (request.url().includes('/api/v1/events')) eventRequests.push(request.headers());
  });
  page.on('console', message => logs.push(message.text()));
  page.on('dialog', () => { throw new Error('Untrusted text executed'); });
  await page.goto('/');
  await expect(page.getByText('production', { exact: true })).toBeVisible();
  await expect(page.getByText('bridge-browser-fixture', { exact: true })).toBeVisible();
  expect(eventRequests).toHaveLength(0);
  await page.getByLabel('Management read credential').fill('uob1.production.browser-fixture-reader-production-secret');
  await page.getByRole('button', { name: 'Connect to production' }).click();
  await expect(page.getByText('Connection interrupted.', { exact: false })).toBeVisible();
  await expect(page.locator('.status')).toHaveText('live', { timeout: 10000 });
  await expect(page.getByText('<img src=x onerror=alert(1)>', { exact: true })).toBeVisible();
  expect(await page.locator('img').count()).toBe(0);
  await expect(page.getByLabel('Management read credential')).toHaveValue('');
  expect(eventRequests.some(headers => headers['last-event-id'] === 'uob:event:1')).toBe(true);
  expect(eventRequests.every(headers => headers.authorization === 'Bearer uob1.production.browser-fixture-reader-production-secret')).toBe(true);
  expect(urls.every(url => !url.includes('uob1.production.browser-fixture-reader-production-secret'))).toBe(true);
  expect(logs.every(log => !log.includes('uob1.production.browser-fixture-reader-production-secret'))).toBe(true);
  expect(urls.some(url => /diagnostics|debug|mqtt|websocket/.test(url))).toBe(false);
  expect(await page.evaluate(() => [localStorage.length, sessionStorage.length])).toEqual([0, 0]);
  if (!process.env.UOB_BROWSER_REPORT_ONLY) await page.screenshot({ path: 'test-results/console-desktop.png', fullPage: true });
  await page.setViewportSize({ width: 390, height: 844 });
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true);
  if (!process.env.UOB_BROWSER_REPORT_ONLY) await page.screenshot({ path: 'test-results/console-mobile.png', fullPage: true });
  await page.getByRole('button', { name: 'Disconnect and clear credential' }).click();
  await expect(page.getByRole('button', { name: 'Connect to production' })).toBeVisible();
  await page.reload();
  await expect(page.locator('.status')).toHaveText('disconnected');
});

test('invalid credentials are denied and staging never inherits production login', async ({ page, context }) => {
  await page.goto('/');
  await page.getByLabel('Management read credential').fill('wrong-fixture-credential');
  await page.getByRole('button', { name: 'Connect to production' }).click();
  await expect(page.getByRole('alert')).toContainText('Access denied');
  await page.getByLabel('Management read credential').fill('uob1.production.browser-fixture-reader-production-secret');
  await page.getByRole('button', { name: 'Connect to production' }).click();
  await expect(page.locator('.status')).toHaveText('live', { timeout: 10000 });
  const staging = await context.newPage();
  const authenticated: string[] = [];
  staging.on('request', request => { if (request.headers().authorization) authenticated.push(request.url()); });
  await staging.goto('http://127.0.0.1:39190');
  await expect(staging.getByRole('button', { name: 'Connect to staging' })).toBeVisible();
  await expect(staging.getByLabel('Management read credential')).toHaveValue('');
  expect(authenticated).toEqual([]);
  await page.goto('http://127.0.0.1:39190');
  await expect(page.locator('.status')).toHaveText('disconnected');
});

test('no-ui disables all compiled assets but preserves real authenticated reads and SSE', async ({ request }) => {
  const base = 'http://127.0.0.1:39191';
  for (const path of ['/', '/ui/assets/console.js', '/ui/assets/console.css']) {
    expect((await request.get(base + path)).status()).toBe(404);
  }
  expect((await request.get(`${base}/api/v1/identity`)).status()).toBe(200);
  expect((await request.get(`${base}/api/v1/stations`)).status()).toBe(401);
  expect((await request.get(`${base}/api/v1/stations`, { headers: { Authorization: 'Bearer uob1.production.browser-fixture-reader-production-secret' } })).status()).toBe(200);
  const stream = await request.get(`${base}/api/v1/events`, { headers: { Authorization: 'Bearer uob1.production.browser-fixture-reader-production-secret' } });
  expect(stream.status()).toBe(200);
  expect(await stream.text()).toContain('event: durable');
});
