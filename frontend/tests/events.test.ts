import { test } from 'node:test';
import assert from 'node:assert/strict';
import { setTimeout as delay } from 'node:timers/promises';
import { ApiClient } from '../src/http';
import { parseIdentity } from '../src/identity';
import { subscribe } from '../src/events';
import type { ConnectionState } from '../src/events';

const identity = parseIdentity({ bridge_id: 'bridge-a', runtime: { environment: 'production', release_id: 'r1', release_digest: 'sha256:1', process_instance_id: 'p1' } });
const origin = 'http://127.0.0.1:8080';
const encoder = new TextEncoder();

async function until(predicate: () => boolean) {
  for (let attempt = 0; attempt < 100; attempt++) { if (predicate()) return; await delay(50); }
  assert.fail('Expected connection state did not arrive');
}

test('cursor gap fetches a scoped snapshot and resumes with visible history loss', async () => {
  const seen: string[] = [];
  let streamCalls = 0;
  const api = new ApiClient(origin, identity, 'read-fixture', async (url, options) => {
    seen.push(String(url));
    if (String(url).endsWith('/identity')) return Response.json(identity);
    if (String(url).includes('/stations/')) return Response.json({});
    streamCalls++;
    assert.equal(new Headers(options?.headers).get('Last-Event-ID'), null);
    if (streamCalls === 1) return Response.json({ recovery: { resource: { bridge_id: 'bridge-a', station_id: 'station-a' }, snapshot_url: 'https://untrusted.example/steal' } }, { status: 410 });
    return new Response(new ReadableStream({ start(controller) { controller.enqueue(encoder.encode(': keep-alive\n\n')); } }), { headers: { 'content-type': 'text/event-stream' } });
  });
  let state: ConnectionState | undefined;
  const stop = subscribe(api, 'station-a', value => { state = value; });
  try {
    await until(() => state?.status === 'live');
    assert.equal(state?.gaps, 1);
    assert.ok(state?.message.includes('history gap'));
    assert.ok(seen.includes(`${origin}/api/v1/stations/station-a`));
    assert.ok(seen.every(url => new URL(url).origin === origin));
  } finally { stop(); api.close(); }
});

test('oversized stream stops and cancellation prevents later browser publications', async () => {
  const api = new ApiClient(origin, identity, 'read-fixture', async url => String(url).endsWith('/identity')
    ? Response.json(identity)
    : new Response(new ReadableStream({ start(controller) { controller.enqueue(encoder.encode(`data: ${'x'.repeat(265 * 1024)}`)); } }), { headers: { 'content-type': 'text/event-stream' } }));
  const states: ConnectionState[] = [];
  const stop = subscribe(api, '', value => states.push(value));
  try {
    await until(() => states.at(-1)?.status === 'stopped');
    assert.ok(states.at(-1)?.message.includes('safety limits'));
    const count = states.length;
    stop();
    await delay(1100);
    assert.equal(states.length, count);
  } finally { stop(); api.close(); }
});
