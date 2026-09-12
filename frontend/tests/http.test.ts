import { test } from 'node:test';
import assert from 'node:assert/strict';
import { ApiClient, ApiError, readJson } from '../src/http';
import { parseIdentity } from '../src/identity';

const identity = parseIdentity({ bridge_id: 'bridge-a', runtime: { environment: 'production', release_id: 'r1', release_digest: 'sha256:1', process_instance_id: 'p1' } });
const origin = 'http://127.0.0.1:8080';

test('same-origin authenticated JSON and SSE use headers, no cookies or redirects', async () => {
  const calls: { url: string; options: RequestInit }[] = [];
  const transport: typeof fetch = async (url, options = {}) => {
    calls.push({ url: String(url), options });
    return Response.json(String(url).endsWith('/identity') ? identity : { items: [] });
  };
  const api = new ApiClient(origin, identity, 'read-fixture', transport);
  await api.stations();
  await api.openEvents('station / one', 'uob:event:2', new AbortController().signal);
  for (const call of calls) {
    assert.equal(new URL(call.url).origin, origin);
    assert.equal(call.options.credentials, 'omit');
    assert.equal(call.options.redirect, 'error');
    assert.equal(call.options.cache, 'no-store');
    assert.ok(!call.url.includes('fixture'));
    const headers = new Headers(call.options.headers);
    assert.equal(headers.get('authorization'), call.url.endsWith('/identity') ? null : 'Bearer read-fixture');
  }
  assert.equal(new Headers(calls.at(-1)!.options.headers).get('Last-Event-ID'), 'uob:event:2');
  await assert.rejects(api.request('https://other.example/api/v1/stations'));
  await assert.rejects(api.request('//other.example/api/v1/stations'));
  await assert.rejects(api.request('/admin'));
  api.close();
});

test('environment or process changes block credential reuse and command submission', async () => {
  let authorized = 0;
  const api = new ApiClient(origin, identity, 'read-fixture', async (_url, options) => {
    if (new Headers(options?.headers).has('authorization')) authorized++;
    return Response.json({ ...identity, runtime: { ...identity.runtime, environment: 'staging' } });
  });
  await assert.rejects(api.stations(), (error: ApiError) => error.kind === 'identity');
  assert.equal(authorized, 0);
});

test('commands require distinct explicit authority and preserve admission evidence', async () => {
  let sent: RequestInit | undefined;
  const admitted = { result: { lifecycle: { state: 'accepted' }, observed_effect: null } };
  const api = new ApiClient(origin, identity, 'read-fixture', async (url, options) => {
    if (String(url).endsWith('/identity')) return Response.json(identity);
    sent = options;
    return Response.json(admitted, { status: 202 });
  });
  const request = { request_id: 'request-1', expires_at: '2026-09-12T16:00:00Z', resource: { bridge_id: identity.bridge_id }, operation: { kind: 'start' } };
  await assert.rejects(api.submitCommand(request, undefined as unknown as string, api.destinationKey));
  assert.equal(sent, undefined);
  assert.deepEqual(await api.submitCommand(request, 'control-fixture', api.destinationKey), admitted);
  assert.equal(sent?.method, 'POST');
  assert.equal(new Headers(sent?.headers).get('authorization'), 'Bearer control-fixture');
  await assert.rejects(api.submitCommand({ ...request, resource: { bridge_id: 'other' } }, 'control-fixture', api.destinationKey));
  api.close();
});

test('malformed identity and oversized JSON are rejected; raw errors never escape', async () => {
  assert.throws(() => parseIdentity({ ...identity, runtime: { ...identity.runtime, environment: 'unknown' } }));
  assert.throws(() => new ApiClient('http://pi.local', identity, 'read-fixture'));
  await assert.rejects(readJson(new Response('x'.repeat(1024 * 1024 + 1))));
  const api = new ApiClient(origin, identity, 'read-fixture', async url => String(url).endsWith('/identity')
    ? Response.json(identity) : new Response('secret malicious server exception', { status: 403 }));
  await assert.rejects(api.stations(), error => error instanceof ApiError && !error.message.includes('secret'));
  api.close();
});

test('every mutation requires this destination and rechecks identity before sending authority', async () => {
  let current = identity;
  let controls = 0;
  const api = new ApiClient(origin, identity, 'read-fixture', async (url, init) => {
    if (String(url).endsWith('/identity')) return Response.json(current);
    if (init?.method === 'POST') controls++;
    return new Response(null, { status: 204 });
  });
  const path = '/api/v1/diagnostics/capture';
  await assert.rejects(api.request(path, { method: 'POST' }), (e: ApiError) => e.kind === 'destination');
  const other = new ApiClient('http://localhost:8081', identity, 'other-fixture');
  await assert.rejects(api.request(path, { method: 'POST' }, undefined, other.destinationKey));
  assert.equal(controls, 0);
  current = { ...identity, runtime: { ...identity.runtime, environment: 'staging' } };
  await assert.rejects(api.request(path, { method: 'POST' }, undefined, api.destinationKey), (e: ApiError) => e.kind === 'identity');
  assert.equal(controls, 0);
  other.close();
});
