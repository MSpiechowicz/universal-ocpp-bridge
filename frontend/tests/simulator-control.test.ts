import { test } from 'node:test';
import assert from 'node:assert/strict';
import { parseIdentity } from '../src/identity';
import { availableControls, controlSeed, parseCatalog, parseControlRun, SimulatorController } from '../src/debug/simulator-control';

const identity = parseIdentity({ bridge_id: 'demo-bridge', runtime: { environment: 'demo', release_id: 'r1', release_digest: 'sha256:1', process_instance_id: 'p1' } });
const catalog = { environment: 'demo', scenarios: [{ id: 'live-alpha', seed: '18446744073709551615', stations: ['demo-alpha'],
  steps: [{ step_id: 'remote-start', station_id: 'demo-alpha', action: 'remote_start', eligible_controls: ['response_delay'] }] }] };
const status = { run_id: '18446744073709551615', environment: 'demo', scenario: 'live-alpha', seed: '18446744073709551615', status: 'running',
  steps: [{ ...catalog.scenarios[0].steps[0], status: 'pending', intervention: null, assertion_passed: null, fault_selected: null }], failure: null };

test('catalog and run preserve exact decimal and offer only pending, server-eligible interventions', () => {
  const scenarios = parseCatalog(catalog, 'demo');
  assert.equal(scenarios[0].seed, '18446744073709551615');
  assert.deepEqual(scenarios[0].stations, ['demo-alpha']);
  assert.deepEqual(availableControls(parseControlRun(status, 'demo', status.run_id).steps[0]), ['response_delay']);
  assert.deepEqual(availableControls(parseControlRun({ ...status, steps: [{ ...status.steps[0], status: 'running' }] }, 'demo', status.run_id).steps[0]), []);
  assert.deepEqual(availableControls(parseControlRun({ ...status, steps: [{ ...status.steps[0], intervention: { kind: 'fault', fault: 'response_delay', delay_ms: 150 } }] }, 'demo', status.run_id).steps[0]), []);
  assert.deepEqual(availableControls(parseControlRun({ ...status, steps: [{ ...status.steps[0], eligible_controls: ['constructor'] }] }, 'demo', status.run_id).steps[0]), []);
  assert.throws(() => parseCatalog({ ...catalog, environment: 'staging' }, 'demo'));
  assert.throws(() => parseCatalog({ ...catalog, scenarios: [{ ...catalog.scenarios[0], stations: ['demo-beta'] }] }, 'demo'));
  assert.throws(() => parseControlRun({ ...status, run_id: Number(status.run_id) }, 'demo', status.run_id));
  for (const seed of ['01', '-1', '18446744073709551616', '1e3']) assert.throws(() => controlSeed(seed));
});

test('control credential never goes to bridge identity, no implicit retry and closed client cannot write', async () => {
  const calls: { url: string; init: RequestInit }[] = [];
  const transport: typeof fetch = async (url, init = {}) => {
    calls.push({ url: String(url), init });
    if (String(url).endsWith('/identity')) return Response.json(identity);
    if (String(url).endsWith('/scenarios')) return Response.json(catalog);
    if (String(url).endsWith('/controls')) throw new TypeError('simulated lost response');
    return Response.json({ run_id: '18446744073709551615' });
  };
  const client = new SimulatorController('http://127.0.0.1:39194', 'b'.repeat(64), identity, transport);
  const scenario = (await client.catalog('http://127.0.0.1:39193'))[0];
  assert.equal(await client.start('http://127.0.0.1:39193', scenario, scenario.seed), status.run_id);
  const step = parseControlRun(status, 'demo', status.run_id).steps[0];
  await assert.rejects(client.intervene('http://127.0.0.1:39193', status.run_id, step, { kind: 'fault', fault: 'response_delay', delay_ms: 150 }));
  assert.equal(calls.filter(item => item.url.endsWith('/controls')).length, 1);
  assert.deepEqual(JSON.parse(String(calls.find(item => item.url.endsWith('/runs'))?.init.body)), { scenario: 'live-alpha', seed: scenario.seed });
  for (const call of calls) {
    assert.equal(call.init.credentials, 'omit');
    assert.equal(call.init.redirect, 'error');
    assert.equal(new Headers(call.init.headers).get('Authorization'), call.url.includes(':39194/') ? `Bearer ${'b'.repeat(64)}` : null);
  }
  client.close();
  await assert.rejects(client.start('http://127.0.0.1:39193', scenario));
  assert.equal(calls.filter(item => item.url.endsWith('/runs')).length, 1);
});

test('production, remote origin and changed bridge identity block control requests', async () => {
  assert.throws(() => new SimulatorController('http://localhost:39194', 'b'.repeat(64), identity));
  assert.throws(() => new SimulatorController('http://127.0.0.1:39194', 'b'.repeat(64), { ...identity, runtime: { ...identity.runtime, environment: 'production' } }));
  let calls = 0;
  const client = new SimulatorController('http://127.0.0.1:39194', 'b'.repeat(64), identity, async () => {
    calls++;
    return Response.json({ ...identity, bridge_id: 'other' });
  });
  await assert.rejects(client.catalog('http://127.0.0.1:39193'));
  assert.equal(calls, 1);
  assert.equal(client.closed, true);
});
