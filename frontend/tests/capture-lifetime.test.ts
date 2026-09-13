import { test } from 'node:test';
import assert from 'node:assert/strict';
import { ApiClient } from '../src/http';
import { parseIdentity } from '../src/identity';
import { TraceBuffer } from '../src/debug/buffer';
import { parseCapture, traceStream } from '../src/debug/capture';
import type { TraceState } from '../src/debug/capture';

const identity = parseIdentity({ bridge_id: 'bridge', runtime: { environment: 'production', release_id: 'r1', release_digest: 'sha256:1', process_instance_id: 'p1' } });
const settled = () => new Promise(resolve => setImmediate(resolve));
const stopped = 'Capture stopped. Displayed traces are a finite, incomplete history.';

test('a late stream response cannot overwrite a completed capture control', async () => {
  const requested = Promise.withResolvers<void>();
  const response = Promise.withResolvers<Response>();
  let bodyCancelled = false;
  const api = new ApiClient('http://localhost', identity, 'fixture', async url => {
    if (String(url).endsWith('/identity')) return Response.json(identity);
    requested.resolve();
    return response.promise;
  });
  const state: TraceState = { message: '', gaps: 0, evicted: 0, dropped: 0, shed: 0, terminal: false };
  const capture = parseCapture({ identity, id: 1, station_id: null, target_id: null, level: 'metadata', remaining_seconds: 600 }, api);
  const stop = traceStream(api, capture, new TraceBuffer(), state);
  try {
    await requested.promise;
    stop();
    state.message = stopped;
    response.resolve(new Response(new ReadableStream({ cancel() { bodyCancelled = true; } }), { headers: { 'content-type': 'text/event-stream' } }));
    await settled();
    assert.equal(state.message, stopped);
    assert.equal(bodyCancelled, true);
  } finally { stop(); api.close(); }
});

test('a buffered read completed during cleanup cannot replace status', async () => {
  let stream!: ReadableStreamDefaultController<Uint8Array>;
  const body = new ReadableStream<Uint8Array>({ start(controller) { stream = controller; } });
  const api = new ApiClient('http://localhost', identity, 'fixture', async url => String(url).endsWith('/identity')
    ? Response.json(identity) : new Response(body, { headers: { 'content-type': 'text/event-stream' } }));
  const state: TraceState = { message: '', gaps: 0, evicted: 0, dropped: 0, shed: 0, terminal: false };
  const capture = parseCapture({ identity, id: 1, station_id: null, target_id: null, level: 'metadata', remaining_seconds: 600 }, api);
  const stop = traceStream(api, capture, new TraceBuffer(), state);
  try {
    await settled();
    assert.match(state.message, /Live/);
    stream.enqueue(new TextEncoder().encode('event: trace_gap\ndata: {"reason":"expiry"}\n\n'));
    stop();
    state.message = stopped;
    await settled();
    assert.equal(state.message, stopped);
    assert.equal(state.gaps, 0);
    assert.equal(state.lastActivity, undefined);
  } finally { stop(); api.close(); }
});
