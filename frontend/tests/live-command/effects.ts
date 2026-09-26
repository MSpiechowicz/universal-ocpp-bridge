import { expect } from '@playwright/test';
import { SseParser } from '../../src/sse';

export type CanonicalResource = { kind: 'connector'; connector_id: string } | { kind: 'evse'; evse_id: string; connector_id?: string };
export type Resource = { bridge_id: string; station_id: string; resource?: CanonicalResource; native_protocol_reference?: unknown };

export async function streamEffect(
  base: string,
  read: Record<string, string>,
  station: string,
  resource: Resource,
  eventId: string,
  eventType: string,
) {
  expect(resource.station_id).toBe(station);
  const params = new URLSearchParams({ station_id: resource.station_id });
  if (resource.resource?.kind === 'connector') {
    params.set('connector_id', resource.resource.connector_id);
  } else if (resource.resource?.kind === 'evse') {
    params.set('evse_id', resource.resource.evse_id);
    if (resource.resource.connector_id !== undefined) params.set('connector_id', resource.resource.connector_id);
  }
  const controller = new AbortController();
  let timedOut = false;
  const timeout = setTimeout(() => { timedOut = true; controller.abort(); }, 10000);
  let reader: ReadableStreamDefaultReader<Uint8Array> | undefined;
  let category = 'stream_ended';
  let bytes = 0;
  let durable = 0;
  let cursor = 0;
  let errors = 0;
  let other = 0;
  let wrongType = 0;
  let matched = false;
  try {
    const response = await fetch(`${base}/api/v1/events?${params}`, { headers: read, signal: controller.signal });
    if (response.status !== 200) {
      category = 'http_status';
    } else if (!response.body) {
      category = 'missing_body';
    } else {
      reader = response.body.getReader();
      const parser = new SseParser(record => {
        if (matched || category !== 'stream_ended') return;
        if (record.event === 'error' || record.event === 'gap') {
          errors++;
          category = 'stream_error';
          return;
        }
        if (record.event !== 'durable') {
          if (record.id !== undefined) cursor++;
          else other++;
          return;
        }

        durable++;
        let event: unknown;
        try { event = JSON.parse(record.data); }
        catch { category = 'invalid_durable'; return; }
        if (!event || typeof event !== 'object' || Array.isArray(event)) {
          category = 'invalid_durable';
          return;
        }
        if ('event_id' in event && event.event_id === eventId) {
          if ('event_type' in event && event.event_type === eventType) matched = true;
          else wrongType++;
        }
      });

      while (bytes < 512 * 1024 && !matched && category === 'stream_ended') {
        const chunk = await reader.read();
        if (chunk.done) break;
        bytes += chunk.value.byteLength;
        if (bytes > 512 * 1024) { category = 'byte_limit'; break; }
        try { parser.push(chunk.value); }
        catch { category = 'invalid_framing'; }
      }
      if (bytes >= 512 * 1024 && !matched && category === 'stream_ended') category = 'byte_limit';
    }
  } catch {
    category = timedOut ? 'timeout' : 'transport_error';
  } finally {
    clearTimeout(timeout);
    controller.abort();
    reader?.releaseLock();
  }
  if (timedOut && !matched && category === 'stream_ended') category = 'timeout';
  if (!matched && category === 'stream_ended' && wrongType > 0) category = 'id_type_mismatch';
  if (!matched) {
    throw new Error(`Linked event absent from resource-scoped durable stream: ${category}; durable=${durable}, cursor=${cursor}, errors=${errors}, other=${other}, wrong_type=${wrongType}, bytes=${Math.min(bytes, 512 * 1024)}`);
  }
}
