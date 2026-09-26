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

test('a successful HTTP response without durable admission is not reported as submitted', async () => {
  const api = new ApiClient(origin, identity, 'read-fixture', async url =>
    String(url).endsWith('/identity') ? Response.json(identity) : Response.json({ result: { lifecycle: { stage: 'admitted' } } }));
  await assert.rejects(api.submitCommand({
    request_id: 'request-1', expires_at: '2099-01-01T00:00:00Z',
    resource: { bridge_id: identity.bridge_id, station_id: 'station-a' },
    operation: { kind: 'start', parameters: { authorization_reference: 'opaque' } },
  }, 'control-fixture', api.destinationKey), (error: ApiError) => error.kind === 'command.unexpected_status');
  api.close();
});

test('command rejection is bounded and sanitized; unknown POST is never replayed and status uses read authority', async () => {
  const station = { bridge_id: identity.bridge_id, station_id: 's1' };
  const headers: string[] = [];
  let posts = 0;
  const api = new ApiClient(origin, identity, 'read-only', async (url, options) => {
    if (String(url).endsWith('/identity')) return Response.json(identity);
    headers.push(new Headers(options?.headers).get('Authorization') ?? '');
    if (options?.method === 'POST') {
      posts++;
      return Response.json({ error: 'command.unsupported', confidential: 'private payload' }, { status: 422 });
    }
    return Response.json({ schema_version: { major: 1, revision: 0 }, resource: station,
      return_route: { request_id: 'request-1', origin: { principal_id: 'private' } },
      lifecycle: { stage: 'admitted' }, recorded_at: '2026-09-25T12:00:00Z', observed_effects: [] });
  });
  await assert.rejects(api.submitCommand({ request_id: 'request-1', expires_at: '2026-09-25T12:02:00Z', resource: station,
    operation: { kind: 'start', parameters: { authorization_reference: 'ref' } } }, 'separate-control', api.destinationKey),
  (error: ApiError) => error.kind === 'command.unsupported' && !error.message.includes('private'));
  assert.equal(posts, 1);
  assert.equal((await api.commandStatus('request-1', station)).lifecycle?.stage, 'admitted');
  assert.deepEqual(headers, ['Bearer separate-control', 'Bearer read-only']);
  api.close();
});

test('protected command options require explicitly supplied control credential and changed identity blocks lookup', async () => {
  const station = { bridge_id: identity.bridge_id, station_id: 's1' };
  const seen: string[] = [];
  let current = identity;
  const api = new ApiClient(origin, identity, 'read-only', async (url, options) => {
    if (String(url).endsWith('/identity')) return Response.json(current);
    seen.push(new Headers(options?.headers).get('Authorization') ?? '');
    return Response.json({ items: [], start: { resource: station, authorization_reference: 'protected-reference' } });
  });
  await assert.rejects(api.commandSchemas(station, ''), (error: ApiError) => error.status === 401);
  assert.equal(seen.length, 0);
  assert.equal((await api.commandSchemas(station, 'control-only')).start?.authorization_reference, 'protected-reference');
  assert.deepEqual(seen, ['Bearer control-only']);
  current = { ...identity, runtime: { ...identity.runtime, process_instance_id: 'another' } };
  await assert.rejects(api.commandSchemas(station, 'control-only'), (error: ApiError) => error.kind === 'identity');
  assert.deepEqual(seen, ['Bearer control-only']);
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

test('pagination uses opaque cursor without changing authority and rejects inconsistent station detail', async () => {
  const seen: string[] = [];
  const snapshot = { schema_version: { major: 1, revision: 0 }, station: { bridge_id: 'bridge-a', station_id: 's / 1' },
    observed_at: '2026-09-24T10:00:00Z', connectivity: { status: 'disconnected' },
    capabilities: { operations: [], optional: [], protocol_details: [] }, resources: [], transactions: [], current_values: [] };
  const api = new ApiClient(origin, identity, 'read-fixture', async url => {
    seen.push(String(url));
    if (String(url).endsWith('/identity')) return Response.json(identity);
    if (String(url).includes('/stations?')) return Response.json({ items: [snapshot], next_cursor: 'opaque+/==' });
    return Response.json({ ...snapshot, station: { ...snapshot.station, station_id: 'different' } });
  });
  assert.equal((await api.stations()).next_cursor, 'opaque+/==');
  await api.stations('opaque+/==');
  assert.equal(new URL(seen.find(url => url.includes('after='))!).searchParams.get('after'), 'opaque+/==');
  await assert.rejects(api.station('s / 1'), (error: ApiError) => error.kind === 'identity');
  api.close();
});

test('long request IDs keep command history pagination and status readable without widening station cursors', async () => {
  const station = { bridge_id: 'bridge-a', station_id: 's1' };
  const anchor = 'r'.repeat(5000);
  const cursor = `uob:command:${'a'.repeat(64)}:opaque-anchor`;
  const lastCursor = 'uob:command:last-page';
  const rows = Array.from({ length: 21 }, (_, index) => ({
    request_id: index === 19 ? anchor : `request-${index}`,
    resource: station,
    observed_effects: [],
  }));
  const api = new ApiClient(origin, identity, 'read-fixture', async url => {
    const path = new URL(String(url));
    if (path.pathname === '/api/v1/identity') return Response.json(identity);
    if (path.pathname === `/api/v1/commands/${anchor}`) {
      return Response.json({ ...rows[19], return_route: { request_id: anchor } });
    }
    if (path.searchParams.get('after') === lastCursor) return Response.json({ items: rows.slice(20) });
    if (path.searchParams.get('after') === cursor) return Response.json({ items: rows.slice(10, 20), next_cursor: lastCursor });
    if (path.searchParams.has('after') || path.searchParams.get('limit') !== '10') return new Response(null, { status: 400 });
    return Response.json({ items: rows.slice(0, 10), next_cursor: cursor });
  });

  const first = await api.commandHistory(station);
  assert.equal(first.items.length, 10);
  assert.equal(first.next_cursor, cursor);
  const second = await api.commandHistory(station, first.next_cursor);
  assert.equal(second.items[9].request_id, anchor);
  assert.equal(second.next_cursor, lastCursor);
  const third = await api.commandHistory(station, second.next_cursor);
  assert.deepEqual(third.items.map(item => item.request_id), ['request-20']);
  assert.equal((await api.commandStatus(second.items[9].request_id, station)).request_id, anchor);
  await assert.rejects(api.commandHistory(station, `uob:command:${'a'.repeat(8181)}`), /Invalid response/);
  await assert.rejects(api.commandHistory(station, `uob:command:${'é'.repeat(4090)}`), /Invalid response/);
  await assert.rejects(api.commandHistory(station, 'uob:command:bad\ncursor'), /Invalid response/);
  await assert.rejects(api.stations(cursor), /Invalid response/);
  api.close();
});

test('twenty large accepted IDs and a following page stay readable without widening other responses or authority', async () => {
  const station = { bridge_id: 'bridge-a', station_id: 's1' };
  const rows = Array.from({ length: 21 }, (_, index) => ({
    request_id: `${index}:` + 'r'.repeat(55_000),
    resource: station,
    observed_effects: [],
  }));
  const cursor = 'uob:command:next-page';
  const firstPage = JSON.stringify({ items: rows.slice(0, 20), next_cursor: cursor });
  assert.ok(Buffer.byteLength(firstPage) > 1024 * 1024);
  assert.ok(Buffer.byteLength(firstPage) < 2 * 1024 * 1024);
  const seen: { path: URL; authorization: string }[] = [];
  const api = new ApiClient(origin, identity, 'read-fixture', async (url, options) => {
    const path = new URL(String(url));
    if (path.pathname === '/api/v1/identity') return Response.json(identity);
    seen.push({ path, authorization: new Headers(options?.headers).get('Authorization') ?? '' });
    if (path.pathname === '/api/v1/commands' && path.searchParams.get('after') === cursor) {
      return Response.json({ items: rows.slice(20) });
    }
    if (path.pathname === '/api/v1/commands' && !path.searchParams.has('after')) return new Response(firstPage);
    if (path.pathname === `/api/v1/commands/${rows[0].request_id}`) {
      return Response.json({ ...rows[0], return_route: { request_id: rows[0].request_id }, ignored: 'x'.repeat(1024 * 1024) });
    }
    return new Response('x'.repeat(2 * 1024 * 1024 + 1));
  });

  const first = await api.commandHistory(station);
  assert.equal(first.items.length, 20);
  assert.equal(first.items[19].request_id, rows[19].request_id);
  const second = await api.commandHistory(station, first.next_cursor);
  assert.equal(second.items[0].request_id, rows[20].request_id);
  assert.equal(second.next_cursor, undefined);
  await assert.rejects(api.commandHistory(station, 'uob:command:oversized'), (error: ApiError) => error.kind === 'limit');
  await assert.rejects(api.commandStatus(rows[0].request_id, station), (error: ApiError) => error.kind === 'limit');
  assert.deepEqual(seen.slice(0, 3).map(({ path }) => [path.searchParams.get('limit'), path.searchParams.get('after')]),
    [['10', null], ['10', cursor], ['10', 'uob:command:oversized']]);
  assert.ok(seen.every(({ authorization }) => authorization === 'Bearer read-fixture'));
  api.close();
});

test('station JSON preserves full i64/u64 integer lexemes without rounding other fields', async () => {
  const raw = JSON.stringify({ schema_version: { major: 1, revision: 0 }, station: { bridge_id: 'bridge-a', station_id: 's1' },
    observed_at: '2026-09-24T10:00:00Z', connectivity: { status: 'disconnected' },
    capabilities: { operations: [], optional: [], protocol_details: [] }, resources: [], transactions: [],
    current_values: [{ point_id: 'energy', value: { type: 'unsigned_integer', value: 'VALUE' }, observed_at: '2026-09-24T10:00:00Z',
      quality: { level: 'good' }, freshness: { status: 'fresh' } }] }).replace('"VALUE"', '18446744073709551615');
  const api = new ApiClient(origin, identity, 'read-fixture', async url =>
    String(url).endsWith('/identity') ? Response.json(identity) : new Response(raw, { headers: { 'content-type': 'application/json' } }));
  assert.equal((await api.station('s1')).current_values[0].value?.value, '18446744073709551615');
  api.close();
});
