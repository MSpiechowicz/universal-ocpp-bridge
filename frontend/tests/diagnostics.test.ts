import { test } from 'node:test';
import assert from 'node:assert/strict';
import { ClientDiagnostics, correlationId, diagnostics, requestArea } from '../src/diagnostics/store';
import { ApiClient } from '../src/http';
import { parseIdentity } from '../src/identity';

const correlation = '12345678-1234-1234-1234-123456789abc';
const identity = parseIdentity({ bridge_id: 'bridge', runtime: { environment: 'production', release_id: 'r1', release_digest: 'sha256:1', process_instance_id: 'p1' } });

test('bounded observations retain categories, not raw request or exception data', () => {
  const store = new ClientDiagnostics();
  for (let n = 0; n < 100000; n++) { store.fail('station', 503, 'Bearer secret'); store.exception('render'); }
  assert.equal(store.failures, 100000);
  assert.equal(store.exceptions, 100000);
  assert.equal(JSON.stringify(store).includes('secret'), false);
  assert.ok(JSON.stringify(store).length < 400);
  store.failures = Number.MAX_SAFE_INTEGER; store.fail('events', -1);
  assert.equal(store.failures, Number.MAX_SAFE_INTEGER);
  assert.equal(store.lastFailure?.status, 0);
  store.clear(); assert.equal(store.lastFailure, undefined); assert.equal(store.exceptions, 0);
  assert.equal(requestArea('/api/v1/stations/private-id?token=secret'), 'station');
  assert.equal(correlationId(correlation), correlation);
  for (const value of ['https://user:secret@host', '<script>secret</script>', 'secret', correlation + '\n']) assert.equal(correlationId(value), undefined);
});

test('HTTP failure preserves only safe server correlation and route category', async () => {
  diagnostics.clear();
  const api = new ApiClient('http://localhost', identity, 'private-token', async url => {
    if (String(url).endsWith('/identity')) return Response.json(identity);
    return Response.json({ error: 'private-token secret payload' }, { status: 503, headers: { 'x-correlation-id': correlation } });
  });
  await assert.rejects(api.station('private-station'));
  assert.equal(diagnostics.requests, 2);
  assert.equal(diagnostics.failures, 1);
  assert.equal(diagnostics.lastFailure?.correlation, correlation);
  assert.equal(diagnostics.lastFailure?.area, 'station');
  assert.doesNotMatch(JSON.stringify(diagnostics), /private|payload|token/);
  api.close();
});

test('network failures and hostile response headers do not retain secrets', async () => {
  diagnostics.clear();
  await assert.rejects(ApiClient.identify('http://localhost', undefined, async () => { throw new Error('password=secret'); }));
  assert.equal(diagnostics.failures, 1);
  const api = new ApiClient('http://localhost', identity, 'private-token', async url => String(url).endsWith('/identity')
    ? Response.json(identity) : new Response('secret', { status: 403, headers: { 'x-correlation-id': 'private-token' } }));
  await assert.rejects(api.stations());
  assert.equal(diagnostics.lastFailure?.correlation, undefined);
  assert.doesNotMatch(JSON.stringify(diagnostics), /private|secret|password/);
  api.close();
});

test('deadline expiry counts as failure while deliberate cancellation does not', async () => {
  for (const name of ['TimeoutError', 'AbortError']) {
    diagnostics.clear();
    const controller = new AbortController();
    const reason = new DOMException('private abort reason', name);
    controller.abort(reason);
    await assert.rejects(ApiClient.identify('http://localhost', controller.signal, async () => { throw reason; }));
    assert.equal(diagnostics.failures, name === 'TimeoutError' ? 1 : 0);
    assert.doesNotMatch(JSON.stringify(diagnostics), /private abort/);
  }
});
