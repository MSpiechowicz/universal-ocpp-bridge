import { test, expect } from '@playwright/test';

test('command panel separates exposure, responses, uncertain rejection and observed evidence', async ({ page, request }) => {
  const endpoint = '/api/v1/diagnostics/capture';
  const headers = { Authorization: 'Bearer uob1.production.browser-fixture-diagnostics-production-secret' };
  const active = await request.get(endpoint, { headers });
  if (active.ok()) {
    const capture = await active.json();
    await request.post(`${endpoint}/${capture.identity.runtime.process_instance_id}/${capture.id}/stop`, { headers });
  }
  const posts: string[] = [];
  page.on('request', request => { if (request.method() === 'POST') posts.push(request.url()); });
  await page.route('**/traces', async route => {
    const path = new URL(route.request().url()).pathname.split('/');
    const process = path.at(-3)!, capture = Number(path.at(-2));
    const samples = [
      ['r-api', 'management.delivery', 'locally_exposed', 'succeeded', ''],
      ['r-accepted', 'command.protocol_response', 'charger_accepted', 'succeeded', ''],
      ['r-uncertain', 'command.protocol_response', 'uncertain', 'uncertain', ''],
      ['r-disconnected', 'application', 'not_transmitted', 'failed', 'StationDisconnected'],
      ['r-denied', 'command.authorization', 'rejected', 'failed', 'ResourceDenied'],
      ['r-observed', 'command.observed_effect', 'observed_effect', 'succeeded', ''],
      ['r-target', 'target.report', 'peer_acknowledged', 'succeeded', ''],
    ];
    const body = samples.map(([id, stage, evidence, status, reason], sequence) => {
      const record = { schema_version: { major: 1, revision: 0 }, trace_id: `t-${sequence}`, process_instance_id: process, trace_sequence: sequence,
        stage, direction: 'internal', observed_at: '2026-09-12T12:00:00Z', correlation_id: 'shared-correlation', outcome: { status },
        duration_micros: 1234, redacted_details: { fields: { station_id: 'station-browser-fixture', evidence, reason_code: reason,
          'command.request_id': id, 'command.origin': '<img src=x onerror=alert(1)>', ...(id === 'r-observed' ? { 'command.event_id': 'event-observed' } : {}) } } };
      return `event: trace\nid: ${JSON.stringify([process, capture, sequence])}\ndata: ${JSON.stringify(record)}\n\n`;
    }).join('');
    await route.fulfill({ contentType: 'text/event-stream', body });
  });
  await page.goto('/#debug');
  await page.getByLabel('Diagnostic credential', { exact: true }).fill('uob1.production.browser-fixture-diagnostics-production-secret');
  await page.getByRole('button', { name: 'Inspect capture status' }).click();
  await page.getByRole('checkbox', { name: /Confirm next control destination/ }).check();
  await page.getByRole('button', { name: 'Start capture on production' }).click();
  const panel = page.getByRole('region', { name: 'Command evidence', exact: true });
  async function select(id: string) {
    await page.getByLabel('Search retained payload text').fill(id);
    await page.locator('.trace-open').first().click();
    await expect(panel).toContainText(id);
  }
  await select('r-api');
  await expect(panel.getByRole('row', { name: /^Charger response/ })).toContainText('Not evidenced');
  await expect(panel.getByRole('row', { name: /^Observed charging effect/ })).toContainText('Not evidenced');
  await expect(panel.getByRole('row', { name: /^API exposure/ })).toContainText('client consumption are not established');
  await expect(panel).toContainText('These links may include other requests');
  await expect(panel.locator('img, script, iframe')).toHaveCount(0);
  await select('r-accepted');
  await expect(panel.getByRole('row', { name: /^Charger response/ })).toContainText('Charger accepted');
  await expect(panel.getByRole('row', { name: /^Observed charging effect/ })).toContainText('Not evidenced');
  await select('r-uncertain');
  await expect(panel.getByRole('row', { name: /^Charger response/ })).toContainText('Automatic replay is unsafe');
  await select('r-disconnected');
  await expect(panel.locator('.command-links').first()).toContainText('StationDisconnected');
  await select('r-denied');
  await expect(panel.getByRole('row', { name: /^Authorization/ })).toContainText('ResourceDenied');
  await select('r-observed');
  await expect(panel.getByRole('row', { name: /^Observed charging effect/ })).toContainText('explicitly linked and persisted');
  await expect(panel.locator('.command-links').first()).toContainText('event-observed');
  if (!process.env.UOB_BROWSER_REPORT_ONLY) await page.screenshot({ path: 'test-results/command-desktop.png', fullPage: true });
  await page.setViewportSize({ width: 390, height: 844 });
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true);
  if (!process.env.UOB_BROWSER_REPORT_ONLY) await page.screenshot({ path: 'test-results/command-mobile.png', fullPage: true });
  await panel.getByRole('button', { name: 'Inspect trace 0', exact: true }).click();
  await expect(panel.getByRole('row', { name: /^Observed charging effect/ })).toContainText('Not evidenced');
  expect(posts).toHaveLength(1); // Capture only: inspection creates no charging or polling work.
  await page.getByRole('button', { name: 'Clear display buffer' }).click();
  await expect(panel).toHaveCount(0);
  await page.getByRole('checkbox', { name: /Confirm next control destination/ }).check();
  await page.getByRole('button', { name: 'Stop capture on production' }).click();
});
