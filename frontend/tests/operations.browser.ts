import { test, expect } from '@playwright/test';

test('real health route separates local safety, exporter outage and recorded remote commits', async ({ page }) => {
  const writes: string[] = [];
  page.on('request', request => { if (request.method() !== 'GET') writes.push(request.url()); });
  await page.goto('/#debug');
  const panel = page.getByRole('region', { name: 'Connections, resources and export', exact: true });
  await panel.getByRole('button', { name: 'Show connections, resources and export' }).click();
  await expect(panel).toContainText('ems-scada.http');
  await expect(panel).toContainText('canonical-read');
  await expect(panel.locator('dt').filter({ hasText: /^Local storage safety$/ }).locator('+ dd')).toHaveText('safe');
  const exporter = panel.getByRole('region', { name: 'External database export', exact: true });
  await expect(exporter).toContainText('Remote export is degraded');
  await expect(exporter).toContainText('test-export-revision-1');
  for (const [name, value] of [['Remote committed records', '12'], ['Quarantined records', '3'], ['Export data gaps', '1'], ['Duplicates handled', '0']]) {
    await expect(exporter.locator('dt').filter({ hasText: new RegExp(`^${name}$`) }).locator('+ dd')).toHaveText(value);
  }
  await expect(panel).toContainText('Unavailable');
  expect(writes).toEqual([]);
  await page.setViewportSize({ width: 390, height: 844 });
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true);
  if (!process.env.UOB_BROWSER_REPORT_ONLY) await page.screenshot({ path: 'test-results/operations-mobile.png', fullPage: true });
});

test('actual daemon disabled export stays passive and hidden panels stop polling', async ({ page }) => {
  const base = 'http://127.0.0.1:39193';
  const calls: string[] = [];
  page.on('request', request => { if (request.url().includes('/api/')) calls.push(request.url()); });
  await page.goto(base + '/#debug');
  const panel = page.getByRole('region', { name: 'Connections, resources and export', exact: true });
  expect(calls.some(url => url.endsWith('/health'))).toBe(false);
  await panel.getByRole('button', { name: 'Show connections, resources and export' }).click();
  await expect(panel).toContainText('Exporter disabled');
  await expect(panel.getByRole('region', { name: 'Exporter connectivity', exact: true })).toContainText('0');
  expect(calls.every(url => /\/api\/v1\/(identity|health)$/.test(url))).toBe(true);
  await panel.getByRole('button', { name: 'Hide connections, resources and export' }).click();
  const count = calls.length;
  await page.waitForTimeout(10500);
  expect(calls).toHaveLength(count);
});

test('failed refresh marks retained data stale without reflecting errors; identity changes clear it', async ({ page }) => {
  await page.goto('/#debug');
  const panel = page.getByRole('region', { name: 'Connections, resources and export', exact: true });
  await panel.getByRole('button', { name: 'Show connections, resources and export' }).click();
  await expect(panel).toContainText('test-export-revision-1');
  await page.route('**/api/v1/health', route => route.fulfill({ status: 503, json: { error: 'postgres://user:secret@example/db' } }));
  await panel.getByRole('button', { name: 'Refresh operational snapshot' }).click();
  await expect(panel).toContainText('Refresh failed; retained snapshot is stale');
  await expect(panel).not.toContainText('postgres://');
  await expect(panel).toContainText('test-export-revision-1');
  await page.route('**/api/v1/identity', async route => {
    const response = await route.fetch();
    const value = await response.json();
    value.runtime.process_instance_id = 'replacement-process';
    await route.fulfill({ json: value });
  });
  await panel.getByRole('button', { name: 'Refresh operational snapshot' }).click();
  await expect(panel).not.toContainText('test-export-revision-1');
});
