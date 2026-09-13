import { test } from 'node:test';
import assert from 'node:assert/strict';
import { parseSimulator, SimulatorReader, simulatorOrigin } from '../src/debug/simulator';
import { parseIdentity } from '../src/identity';

const identity = parseIdentity({ bridge_id: 'demo-bridge', runtime: { environment: 'demo', release_id: 'r1', release_digest: 'sha256:1', process_instance_id: 'p1' } });
const evidence = { schema_version: 1, run_id: '1', environment: 'demo', scenario: 'sample', seed: '18446744073709551615', status: 'failed',
  steps: [{ step_id: 'wait', station_id: 'demo-alpha', action: 'wait', status: 'failed', expectation: 'delay_elapsed', actual_event: 'delay_elapsed', assertion_passed: false, failure_code: 'unexpected_event_detail', fault_selected: false }], events: [] };

test('safe evidence preserves exact seed, false, missing correlation and rejects wrong identity or bounds', () => {
  const result = parseSimulator(evidence, 'demo', '1');
  assert.equal(result.seed, '18446744073709551615');
  assert.equal(result.steps[0].passed, false);
  assert.equal(result.steps[0].selected, false);
  assert.equal(result.steps[0].correlation, undefined);
  for (const changes of [{ seed: Number("18446744073709551615") }, { environment: 'production' }, { schema_version: 2 }, { run_id: '2' }, { steps: Array(257).fill(evidence.steps[0]) }, { events: Array(771).fill({}) }]) {
    assert.throws(() => parseSimulator({ ...evidence, ...changes }, 'demo', '1'));
  }
  const correlation = '12345678-1234-1234-1234-123456789abc';
  assert.equal(parseSimulator({ ...evidence, steps: [{ ...evidence.steps[0], correlation_id: correlation }] }, 'demo', '1').steps[0].correlation, correlation);
  assert.equal(parseSimulator({ ...evidence, steps: [{ ...evidence.steps[0], correlation_id: '<script>secret</script>' }] }, 'demo', '1').steps[0].correlation, undefined);
});

test('read credentials go only to validated simulator origin and closed readers cannot send', async () => {
  const calls: { url: string; init: RequestInit }[] = [];
  const transport: typeof fetch = async (url, init = {}) => {
    calls.push({ url: String(url), init });
    return Response.json(String(url).endsWith('/identity') ? identity : evidence);
  };
  const reader = new SimulatorReader('http://127.0.0.1:9001', '1', 'a'.repeat(64), identity, transport);
  await reader.read('http://127.0.0.1:8080');
  assert.equal(calls.length, 3);
  for (const call of calls) {
    assert.equal(call.init.credentials, 'omit'); assert.equal(call.init.redirect, 'error');
    assert.equal(new Headers(call.init.headers).get('Authorization'), call.url.includes(':9001/') ? `Bearer ${'a'.repeat(64)}` : null);
    assert.equal(call.init.method ?? 'GET', 'GET');
  }
  reader.close(); await assert.rejects(reader.read('http://127.0.0.1:8080')); assert.equal(calls.length, 3);
});

test('production, remote destinations and changed bridge identity block simulator reads', async () => {
  for (const origin of ['http://localhost:9001', 'http://127.0.0.1:9001/', 'http://user@127.0.0.1:9001', 'https://example.com']) assert.throws(() => simulatorOrigin(origin));
  assert.throws(() => new SimulatorReader('http://127.0.0.1:9001', '1', 'a'.repeat(64), { ...identity, runtime: { ...identity.runtime, environment: 'production' } }));
  let calls = 0;
  const reader = new SimulatorReader('http://127.0.0.1:9001', '1', 'a'.repeat(64), identity, async () => { calls++; return Response.json({ ...identity, bridge_id: 'other' }); });
  await assert.rejects(reader.read('http://127.0.0.1:8080'));
  assert.equal(calls, 1);
});
