import { test, expect } from '@playwright/test';

const environments = [
  { name: 'production', port: 39189 },
  { name: 'staging', port: 39190 },
  { name: 'demo', port: 39192 },
];
const token = (environment: string, role: string) => `uob1.${environment}.browser-fixture-${role}-${environment}-secret`;
const capture = '/api/v1/diagnostics/capture';

test('real listeners reject foreign credentials for reads and controls in every direction', async ({ request }) => {
  for (const destination of environments) {
    const base = `http://127.0.0.1:${destination.port}`;
    for (const source of environments) {
      const headers = { Authorization: `Bearer ${token(source.name, 'reader')}` };
      const read = await request.get(`${base}/api/v1/stations`, { headers });
      expect(read.status()).toBe(source === destination ? 200 : 401);
      if (source === destination) continue;
      const control = await request.post(base + capture, {
        headers: { Authorization: `Bearer ${token(source.name, 'diagnostics')}` },
        data: { level: 'metadata', duration_seconds: 10 },
      });
      expect(control.status()).toBe(401);
      // Altering an audience label is not authentication of the entire provisioned token.
      const rewritten = token(source.name, 'diagnostics').replace(`uob1.${source.name}.`, `uob1.${destination.name}.`);
      expect((await request.get(base + capture, { headers: { Authorization: `Bearer ${rewritten}` } })).status()).toBe(401);
    }
    expect((await request.get(base + capture, { headers: { Cookie: `token=${token(destination.name, 'diagnostics')}` } })).status()).toBe(401);
  }
});

test('navigation, new tabs and history never transfer diagnostic credentials or confirmation', async ({ page, context }) => {
  await page.goto('/');
  await page.getByLabel('Diagnostic credential', { exact: true }).fill(token('production', 'diagnostics'));
  await page.getByRole('button', { name: 'Inspect capture status' }).click();
  const confirmation = page.getByRole('checkbox', { name: /Confirm next control destination/ });
  await expect(confirmation).toBeVisible();
  await expect(confirmation).not.toBeChecked();
  await expect(confirmation).toHaveAccessibleName(/PRODUCTION.*bridge-browser-fixture.*release-browser-fixture.*target none selected.*39189/);
  await confirmation.check();
  const other = await context.newPage();
  const authenticated: string[] = [];
  other.on('request', req => { if (req.headers().authorization) authenticated.push(req.url()); });
  await other.goto('http://127.0.0.1:39190');
  await expect(other.getByLabel('Diagnostic credential', { exact: true })).toHaveValue('');
  expect(authenticated).toEqual([]);
  await page.goto('http://127.0.0.1:39192');
  await expect(page.getByLabel('Diagnostic credential', { exact: true })).toHaveValue('');
  await page.goBack();
  await expect(page.getByLabel('Diagnostic credential', { exact: true })).toHaveValue('');
  await expect(page.getByRole('checkbox', { name: /Confirm next control destination/ })).toHaveCount(0);
  expect(await page.evaluate(async () => [localStorage.length, sessionStorage.length, (await indexedDB.databases()).length])).toEqual([0, 0, 0]);
  expect(await context.cookies()).toEqual([]);
});
