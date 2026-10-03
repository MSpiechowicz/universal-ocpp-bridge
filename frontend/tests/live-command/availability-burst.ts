import { expect } from '@playwright/test';
import type { Page } from '@playwright/test';
import { SseParser } from '../../src/sse';
import type { Resource } from './effects';

type AvailabilitySnapshot = { resources?: { resource: Resource; availability: string }[] };

// The peer phase acknowledges every native StatusNotification, not page delivery.
// Replay the same unfiltered station stream from its beginning to the burst's final
// real snapshot, then wait for the page's own received counter and refresh lane.
// Only bounded counts escape this observer; no records, credentials or cursors do.
export async function availabilityBurstDelivered(
  page: Page, base: string, read: Record<string, string>, station: string, targets: Resource[],
) {
  expect(targets.length > 0 && targets.length <= 64).toBe(true);
  await expect(page.getByRole('heading', { name: `Authorized commands · ${station}`, exact: true })).toBeVisible();
  // HTTP snapshots serialize typed resources; event payloads pass through JSON
  // values with a different object-key order. Compare canonical identity fields.
  const resourceIdentity = (resource: Resource) => JSON.stringify([
    resource.bridge_id, resource.station_id, resource.resource?.kind,
    resource.resource?.kind === 'evse' ? resource.resource.evse_id : undefined,
    resource.resource?.connector_id,
  ]);
  const expected = new Set(targets.map(resourceIdentity));
  const controller = new AbortController();
  let timedOut = false;
  const timeout = setTimeout(() => { timedOut = true; controller.abort(); }, 10000);
  const byteLimit = 8 * 1024 * 1024;
  let reader: ReadableStreamDefaultReader<Uint8Array> | undefined;
  let bytes = 0;
  let durable = 0;
  let complete = false;
  let category = 'stream_ended';
  try {
    const params = new URLSearchParams({ station_id: station });
    const response = await fetch(`${base}/api/v1/events?${params}`, { headers: read, signal: controller.signal });
    if (response.status !== 200 || !response.body) {
      category = 'stream_unavailable';
    } else {
      reader = response.body.getReader();
      const parser = new SseParser(record => {
        if (complete || category !== 'stream_ended') return;
        if (record.event === 'error' || record.event === 'gap') {
          category = 'stream_error';
          return;
        }
        if (record.event !== 'durable') return;
        durable++;
        if (durable > 1000) { category = 'event_limit'; return; }
        const event = JSON.parse(record.data) as {
          resource?: Resource; event_type?: string; payload?: AvailabilitySnapshot;
        };
        if (event.resource?.station_id !== station) { category = 'station_mismatch'; return; }
        if (event.event_type !== 'station.availability.observed' || !Array.isArray(event.payload?.resources)) return;
        const unavailable = new Set(event.payload.resources
          .filter(item => item.availability === 'unavailable').map(item => resourceIdentity(item.resource)));
        complete = [...expected].every(target => unavailable.has(target));
      });
      while (!complete && category === 'stream_ended') {
        const chunk = await reader.read();
        if (chunk.done) break;
        bytes += chunk.value.byteLength;
        if (bytes > byteLimit) { category = 'byte_limit'; break; }
        try { parser.push(chunk.value); }
        catch { category = 'invalid_framing'; }
      }
    }
  } catch {
    category = timedOut ? 'timeout' : 'transport_error';
  } finally {
    clearTimeout(timeout);
    controller.abort();
    reader?.releaseLock();
  }
  if (!complete || category !== 'stream_ended') {
    throw new Error(`Availability burst incomplete: ${category}; durable=${Math.min(durable, 1000)}, bytes=${Math.min(bytes, byteLimit)}`);
  }

  const metric = (name: string) => page.locator('.stream-panel dl div')
    .filter({ has: page.getByText(name, { exact: true }) }).locator('dd');
  // These counters belong to this selected station's subscription and reset on
  // station switches. No replay/recovery may duplicate receipts in this ordinal.
  await expect(metric('Reconnect attempts')).toHaveText('0');
  await expect(metric('History gaps')).toHaveText('0');
  await expect.poll(async () => Number(await metric('Events received').textContent()), { timeout: 15000 })
    .toBeGreaterThanOrEqual(durable);
  await expect(page.locator('.status')).toHaveText('live');
  await expect(page.getByRole('heading', { name: `Authorized commands · ${station}`, exact: true })).toBeVisible();
  await expect(metric('Reconnect attempts')).toHaveText('0');
  await expect(metric('History gaps')).toHaveText('0');
  await expect(page.locator('#stations').getByText('Snapshot stale or refreshing. Do not treat displayed observations as live.')).toBeHidden();
}
