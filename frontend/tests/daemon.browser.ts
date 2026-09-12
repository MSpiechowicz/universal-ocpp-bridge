import { test, expect } from '@playwright/test';

test('actual daemon serves compiled console and reports unavailable reads honestly', async ({ page, request }) => {
  const base = 'http://127.0.0.1:39193';
  const response = await request.get(`${base}/api/v1/identity`);
  expect(response.status()).toBe(200);
  const identity = await response.json();
  expect(identity.bridge_id).toBe('browser-daemon-demo');
  expect(identity.runtime.environment).toBe('demo');
  const requests: string[] = [];
  page.on('request', incoming => requests.push(incoming.url()));
  await page.goto(base);
  await expect(page.getByText('browser-daemon-demo', { exact: true }).first()).toBeVisible();
  await expect(page.getByRole('button', { name: 'Connect to demo', exact: true })).toBeVisible();
  await expect(page.locator('.status')).toHaveText('disconnected');
  expect(requests.some(url => url.endsWith('/ui/assets/console.js'))).toBe(true);
  expect(requests.some(url => url.endsWith('/ui/assets/console.css'))).toBe(true);
  // This minimal daemon has no authenticated query source composed. The UI must
  // show that real 503, not claim an authenticated/live session from fixture data.
  await page.getByLabel('Management read credential').fill('uob1.demo.daemon-smoke-invalid');
  await page.getByRole('button', { name: 'Connect to demo', exact: true }).click();
  await expect(page.getByRole('alert')).toContainText('Management data is unavailable');
  await expect(page.locator('.status')).toHaveText('disconnected');
  await expect(page.getByLabel('Management read credential')).toHaveValue('');
  expect(requests.every(url => url.startsWith(base + '/'))).toBe(true);
  expect(requests.some(url => /diagnostics\/capture|\/commands|\/events/.test(url))).toBe(false);
});
