import { expect, test } from '@playwright/test';
import { startLiveDaemon } from '../scripts/live-daemon-fixture.mjs';

// A real uob service and two authenticated WebSocket peers. No route interception,
// fixtures returning management JSON, or token-bearing URLs/screenshots/traces.
test('live daemon renders isolated OCPP editions and recovers from peer changes', async ({ page, request }) => {
  const fixture = await startLiveDaemon();
  const { base } = fixture;
  const credential = fixture.grant();
  const headers = { Authorization: `Bearer ${credential}` };
  const detail = (station: string) => page.getByRole('article', { name: `Station ${station} detail` });
  const refresh = async () => { await page.getByRole('button', { name: 'Refresh snapshot' }).click(); };
  const connect = async () => {
    await page.getByLabel('Management read credential').fill(credential);
    await page.getByRole('button', { name: 'Connect to demo' }).click();
    await expect(page.getByRole('button', { name: 'Disconnect and clear credential' })).toBeVisible();
  };
  try {
    expect((await request.get(`${base}/api/v1/identity`)).status()).toBe(200);
    expect((await request.get(`${base}/api/v1/stations`)).status()).toBe(401);
    expect((await request.get(`${base}/api/v1/events?station_id=station-a`)).status()).toBe(401);
    expect((await request.get(`${base}/api/v1/stations`, { headers: { Authorization: 'Bearer uob1.demo.not-the-grant' } })).status()).toBe(401);
    expect((await request.get(`${base}/api/v1/events?station_id=not-in-roster`, { headers })).status()).toBe(403);
    expect((await request.get(`${base}/api/v1/stations/not-in-roster`, { headers })).status()).toBe(403);
    const stationsResponse = await request.get(`${base}/api/v1/stations`, { headers });
    expect(stationsResponse.status()).toBe(200);
    const inventory = await stationsResponse.json();
    expect(inventory.items.map((item: { station: { station_id: string } }) => item.station.station_id)).toEqual(['station-a', 'station-b']);
    const alpha = await (await request.get(`${base}/api/v1/stations/station-a`, { headers })).json();
    const bravo = await (await request.get(`${base}/api/v1/stations/station-b`, { headers })).json();
    expect(alpha.station.station_id).toBe('station-a');
    expect(alpha.connectivity.protocol).toBe('ocpp16j');
    expect(alpha.resources.map((row: { resource: { native_protocol_reference: { connector_id: number } } }) => row.resource.native_protocol_reference.connector_id)).toEqual([1]);
    expect(alpha.current_values.some((point: { point_id: string; value?: { value: string } }) => point.point_id.includes('connector-0/status/status') && point.value?.value === 'Unavailable')).toBe(true);
    expect(alpha.current_values.some((point: { point_id: string; value?: { value: string } }) => point.point_id.includes('connector-0/') && point.value?.value === '0')).toBe(true);
    expect(alpha.resources[0].availability).toBe('available');
    expect(alpha.resources[0].current_values.some((point: { quality: { level: string; reason?: string }; value?: unknown }) => point.quality.level === 'bad' && point.quality.reason === 'invalid_decimal' && point.value == null)).toBe(true);
    expect(alpha.transactions.length).toBeGreaterThan(0);
    expect(bravo.station.station_id).toBe('station-b');
    expect(bravo.connectivity.protocol).toBe('ocpp201');
    expect(bravo.resources.map((row: { resource: { native_protocol_reference: { evse_id: number; connector_id?: number } } }) => row.resource.native_protocol_reference)).toEqual([
      { protocol: 'ocpp201', evse_id: 1 }, { protocol: 'ocpp201', evse_id: 1, connector_id: 1 },
      { protocol: 'ocpp201', evse_id: 2 }, { protocol: 'ocpp201', evse_id: 2, connector_id: 1 },
    ]);
    expect(bravo.transactions.map((tx: { resource: { native_protocol_reference: { evse_id: number } } }) => tx.resource.native_protocol_reference.evse_id).sort()).toEqual([1, 2]);
    expect(bravo.resources[0].current_values.some((point: { value?: { value: string } }) => point.value?.value === '7.5')).toBe(true);
    expect(bravo.resources[2].current_values.some((point: { value?: { value: string } }) => point.value?.value === '0')).toBe(true);
    expect(bravo.resources.every((resource: { resource: { station_id: string } }) => resource.resource.station_id === 'station-b')).toBe(true);

    const stream = new AbortController();
    const timeout = setTimeout(() => stream.abort(), 5000);
    try {
      const response = await fetch(`${base}/api/v1/events?station_id=station-b`, { headers, signal: stream.signal });
      expect(response.status).toBe(200);
      expect(response.headers.get('content-type')).toContain('text/event-stream');
      const reader = response.body!.getReader();
      let received = '';
      while (!received.includes('event: durable') || !received.includes('data:')) {
        const part = await reader.read();
        if (part.done) break;
        received += new TextDecoder().decode(part.value);
        if (received.length > 65536) throw new Error('event stream exceeded expected evidence bound');
      }
      expect(received).toContain('event: durable');
      expect(received).toContain('data:');
    } finally { clearTimeout(timeout); stream.abort(); }

    const urls: string[] = [];
    page.on('request', incoming => urls.push(incoming.url()));
    await page.goto(base);
    await expect(page.getByText('live-browser-demo', { exact: true }).first()).toBeVisible();
    await page.getByLabel('Management read credential').fill('uob1.demo.not-the-grant');
    await page.getByRole('button', { name: 'Connect to demo' }).click();
    await expect(page.getByRole('alert')).toContainText('Access denied');
    await expect(page.locator('.status')).toHaveText('disconnected');
    await connect();
    await expect(page.locator('.status')).toHaveText('live', { timeout: 15000 });
    await expect(detail('station-a')).toContainText('OCPP 1.6 connector 1');
    await expect(detail('station-a')).toContainText('Connector connector-1 · available');
    await expect(detail('station-a')).toContainText('Original: not-a-number');
    await expect(detail('station-a')).toContainText('Quality: bad · invalid_decimal');
    await expect(detail('station-a')).toContainText('ocpp16/connector-0/status/status');
    await expect(detail('station-a')).toContainText('Original: 0');
    await expect(detail('station-a')).toContainText('Freshness: unknown');
    await expect(detail('station-a').getByRole('button')).toHaveCount(0);
    await page.getByRole('button', { name: 'station-b', exact: true }).click();
    await expect(detail('station-b')).toContainText('EVSE evse-2 / connector connector-2');
    await expect(detail('station-b')).toContainText('OCPP 2.0.1 EVSE 2 / connector 1');
    await expect(detail('station-b')).toContainText('browser-bravo-tx-2');
    await expect(detail('station-b')).toContainText('Charging resources (4)');
    await expect(detail('station-b')).not.toContainText('BROWSER-A');
    await expect(detail('station-b').getByText('Original: 0', { exact: false })).toBeVisible();
    const stationTraceLink = detail('station-b').getByRole('link', { name: 'Search retained Debug traces for station station-b (station-scoped only)' }).first();
    await expect(stationTraceLink).toHaveAttribute('href', '#debug');
    await stationTraceLink.click();
    await expect(page).toHaveURL(/#debug$/);
    await expect(page.getByLabel('Filter station')).toHaveValue('station-b');
    await expect(page.getByLabel('Filter transaction')).toHaveValue('');
    await expect(page.getByRole('button', { name: 'Start capture on demo' })).toHaveCount(0);
    await page.evaluate(() => { Object.defineProperty(document, 'hidden', { configurable: true, value: true }); document.dispatchEvent(new Event('visibilitychange')); });
    await expect(page.getByText('Snapshot stale or refreshing. Do not treat displayed observations as live.')).toBeVisible();
    await page.evaluate(() => { Object.defineProperty(document, 'hidden', { configurable: true, value: false }); document.dispatchEvent(new Event('visibilitychange')); });

    await fixture.phase('disconnect', 'disconnected');
    await expect.poll(async () => (await (await request.get(`${base}/api/v1/stations/station-a`, { headers })).json()).connectivity.status).toBe('disconnected');
    await refresh();
    await expect(page.getByRole('button', { name: 'station-a', exact: true }).locator('..')).toContainText('disconnected');
    await page.getByRole('button', { name: 'station-a', exact: true }).click();
    await expect(detail('station-a')).toContainText('Connection: disconnected');
    await expect(detail('station-a')).toContainText('Connector connector-1 · unknown');
    await expect.poll(async () => (await (await request.get(`${base}/api/v1/stations/station-b`, { headers })).json()).connectivity.status).toBe('connected');
    await fixture.phase('reconnect', 'reconnected');
    await refresh();
    await expect.poll(async () => (await (await request.get(`${base}/api/v1/stations/station-a`, { headers })).json()).connectivity.status).toBe('connected');
    await expect(detail('station-a')).toContainText('Connection: connected');
    await expect(detail('station-a')).toContainText('Connector connector-1 · available');
    await page.getByRole('button', { name: 'Disconnect and clear credential' }).click();
    await expect(page.locator('.status')).toHaveText('disconnected');
    await expect(detail('station-a')).toHaveCount(0);
    await page.reload();
    await expect(page.getByLabel('Management read credential')).toHaveValue('');
    expect(urls.every(url => !url.includes(credential) && url.startsWith(base))).toBe(true);
    await fixture.phase('stop', 'stopped');
  } finally { await fixture.cleanup(); }
});
