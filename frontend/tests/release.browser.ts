import { test, expect } from '@playwright/test';
import type { Locator, Page } from '@playwright/test';

const production = 'http://127.0.0.1:39189';
const releaseStatus = '/api/v1/release/status';
const releaseEvents = '/api/v1/release/events?after=0';
const capture = '/api/v1/diagnostics/capture';
const releaseCredential = 'uob1.production.browser-fixture-release-production-secret';
const managementCredential = 'uob1.production.browser-fixture-reader-production-secret';
const candidate = 'a'.repeat(64);
const previousGood = 'b'.repeat(64);
const evidence = 'c'.repeat(64);
const configuration = 'd'.repeat(64);

async function openRelease(page: Page, origin = production): Promise<Locator> {
  await page.goto(`${origin}/#debug`);
  const panel = page.getByRole('region', { name: 'Environments and releases', exact: true });
  await panel.getByRole('button', { name: 'Show environments and releases', exact: true }).click();
  return panel;
}

function value(panel: Locator, label: string): Locator {
  return panel.locator('dt').filter({ hasText: new RegExp(`^${label}$`) }).locator('+ dd');
}

async function inspect(panel: Locator): Promise<void> {
  await panel.getByLabel('Release read credential', { exact: true }).fill(releaseCredential);
  await panel.getByRole('button', { name: 'Inspect release status', exact: true }).click();
  await expect(panel.getByRole('status')).toContainText('Authorized release evidence received.');
}

test('actual release router authenticates a separate observer and renders authoritative rollback evidence', async ({ page, request }) => {
  expect((await request.get(`${production}${releaseStatus}`)).status()).toBe(401);
  expect((await request.get(`${production}${releaseStatus}`, { headers: { Authorization: `Bearer ${managementCredential}` } })).status()).toBe(401);
  expect((await request.get(`${production}${releaseStatus}`, { headers: { Authorization: `Bearer ${releaseCredential}` } })).status()).toBe(200);
  expect((await request.post(`${production}${releaseStatus}`, { headers: { Authorization: `Bearer ${releaseCredential}` } })).status()).toBe(405);
  expect((await request.post(`${production}${capture}`, { headers: { Authorization: `Bearer ${releaseCredential}` } })).status()).toBe(401);

  const releaseRequests: { path: string; authorization: string | undefined }[] = [];
  const mutations: string[] = [];
  page.on('request', request => {
    const url = new URL(request.url());
    if (url.pathname.startsWith('/api/v1/release/')) {
      releaseRequests.push({ path: url.pathname + url.search, authorization: request.headers().authorization });
    }
    if (!['GET', 'OPTIONS'].includes(request.method()) && (url.pathname.startsWith('/api/v1/release/') || url.pathname === capture)) {
      mutations.push(request.url());
    }
  });

  const panel = await openRelease(page);
  expect(releaseRequests).toEqual([]);
  await inspect(panel);
  await expect(panel.getByRole('button', { name: 'Refresh release evidence', exact: true })).toBeVisible();
  expect(releaseRequests).toEqual([
    { path: releaseStatus, authorization: `Bearer ${releaseCredential}` },
    { path: releaseEvents, authorization: `Bearer ${releaseCredential}` },
  ]);

  const activation = panel.getByRole('region', { name: 'Activation journal pointers', exact: true });
  await expect(value(activation, 'Production digest')).toHaveText(previousGood);
  await expect(value(activation, 'Production phase')).toHaveText('previous-good');
  await expect(value(activation, 'Previous good digest')).toHaveText(previousGood);
  await expect(value(activation, 'Candidate digest')).toHaveText(candidate);
  await expect(value(activation, 'Candidate phase')).toHaveText('quarantined');

  const qualification = panel.getByRole('region', { name: 'Qualification evidence', exact: true });
  await expect(qualification).toContainText('Current qualification unavailable.');

  const promotion = panel.getByRole('region', { name: 'Promotion and drain evidence', exact: true });
  await expect(value(promotion, 'Evidence digest')).toHaveText(evidence);
  await expect(value(promotion, 'Configuration digest')).toHaveText(configuration);
  await expect(value(promotion, 'Compatibility check')).toHaveText('accepted');
  await expect(value(promotion, 'Drain check')).toHaveText('granted');
  await expect(value(promotion, 'Health decision')).toHaveText('probation');
  await expect(value(promotion, 'Outcome')).toHaveText('continuing');

  const failure = panel.getByRole('region', { name: 'Failure and rollback evidence', exact: true });
  await expect(value(failure, 'Retained failure decision')).toHaveText('rollback_required');
  await expect(value(failure, 'Last retained failure signal')).toContainText('watchdog');
  await expect(value(failure, 'Rollback quarantined digest')).toHaveText(candidate);
  await expect(value(failure, 'Rollback previous good digest')).toHaveText(previousGood);
  await expect(value(failure, 'Rollback step')).toHaveText('restored');
  await expect(value(failure, 'Rollback reason')).toHaveText('eligible_failure');
  await expect(panel.getByRole('button', { name: /promote|rollback|capture/i })).toHaveCount(0);
  expect(mutations).toEqual([]);
});

test('release credentials and evidence never survive disconnect, reload, tabs, or history navigation', async ({ page, context }) => {
  const panel = await openRelease(page);
  await inspect(panel);
  await page.getByRole('button', { name: 'Disconnect release', exact: true }).click();
  await expect(panel.getByLabel('Release read credential', { exact: true })).toHaveValue('');
  await expect(panel).not.toContainText(previousGood);

  await inspect(panel);
  await page.reload();
  const afterReload = page.getByRole('region', { name: 'Environments and releases', exact: true });
  await afterReload.getByRole('button', { name: 'Show environments and releases', exact: true }).click();
  await expect(afterReload.getByLabel('Release read credential', { exact: true })).toHaveValue('');
  await expect(afterReload).not.toContainText(previousGood);

  const other = await context.newPage();
  const transferred: string[] = [];
  other.on('request', request => { if (request.headers().authorization === `Bearer ${releaseCredential}`) transferred.push(request.url()); });
  const isolated = await openRelease(other);
  await expect(isolated.getByLabel('Release read credential', { exact: true })).toHaveValue('');
  expect(transferred).toEqual([]);

  await inspect(afterReload);
  await page.goto('http://127.0.0.1:39190/#debug');
  await page.goBack();
  const afterHistory = page.getByRole('region', { name: 'Environments and releases', exact: true });
  const showAfterHistory = afterHistory.getByRole('button', { name: 'Show environments and releases', exact: true });
  if (await showAfterHistory.isVisible()) await showAfterHistory.click();
  await expect(afterHistory.getByLabel('Release read credential', { exact: true })).toHaveValue('');
  await expect(afterHistory).not.toContainText(previousGood);
  expect(await page.evaluate(async () => [localStorage.length, sessionStorage.length, (await indexedDB.databases()).length])).toEqual([0, 0, 0]);
  expect(await context.cookies()).toEqual([]);
});

test('staging and demo isolate production release reads and do not expose a release credential', async ({ page, request }) => {
  for (const origin of ['http://127.0.0.1:39190', 'http://127.0.0.1:39192']) {
    expect((await request.get(`${origin}${releaseStatus}`, { headers: { Authorization: `Bearer ${releaseCredential}` } })).status()).toBe(404);
    const sent: string[] = [];
    page.on('request', request => { if (request.headers().authorization === `Bearer ${releaseCredential}`) sent.push(request.url()); });
    const panel = await openRelease(page, origin);
    await expect(panel.getByLabel('Release read credential', { exact: true })).toHaveCount(0);
    await expect(panel).toContainText('Release read routes and production credentials are unavailable here');
    expect(sent).toEqual([]);
  }
});

test('unavailable release read gives an explicit independent CLI fallback without mutation', async ({ page }) => {
  await page.route(`**${releaseStatus}`, route => route.abort());
  const panel = await openRelease(page);
  await panel.getByLabel('Release read credential', { exact: true }).fill(releaseCredential);
  await panel.getByRole('button', { name: 'Inspect release status', exact: true }).click();
  await expect(panel.getByRole('alert')).toContainText('Release evidence unavailable');
  const authorization = panel.getByRole('region', { name: 'Release CLI authorization', exact: true });
  await expect(authorization.getByText('uob release status', { exact: true })).toBeVisible();
  await expect(authorization.getByText('uob release events', { exact: true })).toBeVisible();
  await expect(authorization).toContainText('separate local CLI permissions');
  await expect(panel.getByRole('button', { name: /promote|rollback|capture/i })).toHaveCount(0);
});
