import { test } from 'node:test';
import assert from 'node:assert/strict';
import { BYTE_LIMIT, DETAIL_LIMIT, matches, parseRow, TraceBuffer } from '../src/debug/buffer';
import { ApiClient } from '../src/http';
import { parseIdentity } from '../src/identity';
import { capturePath, parseCapture, traceStream } from '../src/debug/capture';

const identity = parseIdentity({ bridge_id: 'bridge', runtime: { environment: 'production', release_id: 'r1', release_digest: 'sha256:1', process_instance_id: 'p1' } });
const raw = (sequence: number, payload = '') => JSON.stringify({ schema_version: { major: 1, revision: 0 }, trace_id: `trace-${sequence}`, process_instance_id: 'p1', trace_sequence: sequence, stage: 'target.delivery', direction: 'outbound', observed_at: '2026-09-12T12:00:00Z', target: { instance_id: 'main', kind: 'mqtt' }, outcome: { status: 'uncertain' }, redacted_details: { fields: { station_id: 'station-a', protocol: 'Ocpp16', action: 'RemoteStartTransaction', evidence: 'locally_exposed', payload } } });
const captureValue = { identity, id: 1, station_id: 'station-a', target_id: null, level: 'metadata', remaining_seconds: 600 };

test('high-volume retention stays bounded even without any display reads; paused bookmarks expire', () => {
  const buffer = new TraceBuffer();
  buffer.append(raw(0), 'p1'); buffer.bookmark(0); buffer.detail(0);
  const pausedAt = 0;
  for (let sequence = 1; sequence < 5000; sequence++) buffer.append(raw(sequence), 'p1');
  assert.equal(buffer.rows.length, 2000); assert.equal(buffer.evicted, 3000);
  assert.equal(buffer.rows.filter(row => row.sequence <= pausedAt).length, 0);
  assert.equal(buffer.expiredBookmarks, 1); assert.equal(buffer.bookmarks.size, 0);
  assert.equal(buffer.detail(0), undefined); assert.equal(buffer.detailBytes, 0);
  buffer.append(raw(4999), 'p1'); assert.equal(buffer.rows.length, 2000);
  buffer.clear(); buffer.append(raw(4999), 'p1'); assert.equal(buffer.rows.length, 0);
});

test('byte ceiling wins before row ceiling, cache is bounded and malicious data stays literal', () => {
  const buffer = new TraceBuffer();
  const payload = '<script>alert(1)</script>' + '界'.repeat(17000);
  for (let sequence = 0; sequence < 150; sequence++) buffer.append(raw(sequence, payload), 'p1');
  assert.ok(buffer.bytes <= BYTE_LIMIT); assert.ok(buffer.rows.length < 100);
  for (const row of buffer.rows) { assert.match(buffer.detail(row.sequence)!, /<script>/); assert.ok(buffer.detailBytes <= DETAIL_LIMIT); }
  assert.ok(buffer.detailBytes < 4 * 64 * 1024);
  assert.throws(() => parseRow(raw(151, 'x'.repeat(65536)), 'p1'));
  assert.throws(() => parseRow(raw(151), 'other-process'));
  assert.throws(() => parseRow(raw(Number.MAX_SAFE_INTEGER + 1), 'p1'));
  buffer.clear(); assert.equal(buffer.detailBytes, 0); assert.equal(buffer.bytes, 0);
});

test('filters use actual evidence without inferring physical success or missing dimensions', () => {
  const row = parseRow(raw(1, 'searchable'), 'p1');
  assert.equal(row.outcome, 'uncertain'); assert.equal(row.evidence, 'locally_exposed');
  assert.ok(matches(row, { target: 'main', kind: 'mqtt', station: 'station-a', protocol: 'Ocpp16', action: 'RemoteStart', direction: 'outbound', correlation: 'unavailable', evse: 'unavailable', connector: 'unavailable', transaction: 'unavailable', severity: 'unavailable', search: 'SEARCHABLE' }));
  assert.ok(!matches(row, { severity: 'success' }));
  assert.ok(!matches(row, { from: '2026-09-13T00:00' }));
  assert.ok(!matches(row, { until: '2026-09-11T00:00' }));
});

test('status rejects a different destination and controls accept an empty 204 response', async () => {
  const calls: string[] = [];
  const api = new ApiClient('http://localhost', identity, 'diagnostic-fixture', async (url, init) => {
    calls.push(String(url));
    if (String(url).endsWith('/identity')) return Response.json(identity);
    assert.equal(init?.credentials, 'omit');
    return new Response(null, { status: 204 });
  });
  assert.equal(await api.request(`${capturePath}/p1/1/stop`, { method: 'POST' }), undefined);
  assert.equal(calls.length, 2);
  assert.throws(() => parseCapture({ ...captureValue, identity: { ...identity, bridge_id: 'other' } }, api));
});

test('trace gap, cursor resume and expiry stay independent of browser rendering', async () => {
  const buffer = new TraceBuffer();
  const state = { message: '', gaps: 0, evicted: 0, dropped: 0, shed: 0, terminal: false };
  let streams = 0;
  const api = new ApiClient('http://localhost', identity, 'diagnostic-fixture', async (url, init) => {
    if (String(url).endsWith('/identity')) return Response.json(identity);
    streams++;
    if (streams === 2) {
      assert.equal(new Headers(init?.headers).get('Last-Event-ID'), '["p1",1,1]');
      return new Response('event: trace_gap\ndata: {"reason":"expiry","replay":"best_effort"}\n\n', { headers: { 'content-type': 'text/event-stream' } });
    }
    return new Response(`event: trace_window\ndata: ${JSON.stringify({ process_instance_id: 'p1', capture_id: 1, replay: 'best_effort', window: { evicted_records: 4, dropped_records: 2, shed_records: 1 } })}\n\nevent: trace\nid: ["p1",1,1]\ndata: ${raw(1)}\n\n`, { headers: { 'content-type': 'text/event-stream' } });
  });
  const stop = traceStream(api, parseCapture(captureValue, api), buffer, state);
  try {
    await new Promise(resolve => setTimeout(resolve, 1150));
    assert.equal(streams, 2); assert.equal(buffer.rows.length, 1); assert.equal(state.evicted, 4);
    assert.ok(state.gaps >= 2); assert.equal(state.terminal, true); assert.match(state.message, /expired/);
  } finally { stop(); api.close(); }
});
