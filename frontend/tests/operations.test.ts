import { test } from 'node:test';
import assert from 'node:assert/strict';
import { parseOperations, displayMetric, latestCommit, stationObservations } from '../src/debug/operations';
import { TraceBuffer } from '../src/debug/buffer';
import { ApiClient, ApiError } from '../src/http';
import { parseIdentity } from '../src/identity';

test('outage, explicit zeros, missing capacity and invalid counters stay distinct', () => {
  const snapshot = parseOperations({ readiness: 'ready', storage: 'safe', accepts_new_sessions: true,
    runtime: { queues: { exporter_batches: 0, database_work: -1 } },
    components: { external_exporter: { state: 'reconnecting', backlog_items: 7, active_connections: 0 } },
    storage_latency: { samples: 0, maximum_ms: 0 },
  });
  assert.equal(snapshot.storage, 'safe');
  assert.equal(snapshot.exporter.state, 'reconnecting');
  assert.equal(displayMetric(snapshot.exporter.connections), '0');
  assert.equal(displayMetric(snapshot.exporter.inFlight), 'Unavailable');
  assert.equal(snapshot.queues.find(row => row.name === 'exporter batches')?.used, 0);
  assert.equal(snapshot.queues.find(row => row.name === 'exporter batches')?.capacity, undefined);
  assert.equal(snapshot.queues.find(row => row.name === 'database work')?.used, undefined);
  assert.equal(snapshot.resources.find(([key]) => key === 'Database latency maximum (ms)')?.[1], undefined);
});

test('projection excludes raw reasons, addresses, unknown strings and unsafe integers', () => {
  const endpoint = 'postgres://user:password@example/db?token=secret';
  const snapshot = parseOperations({ readiness: 'not_ready', storage: endpoint, accepts_new_sessions: false,
    components: { target: { state: endpoint, reason: endpoint, backlog_items: Number.MAX_SAFE_INTEGER + 1 } },
    export_observation: { provider: endpoint, destination_revision: endpoint, age_ms: 30001, gap_count: 0, quarantined_records: 3, remote_committed_records: 12, record_classes: [endpoint] },
    arbitrary_extension: endpoint,
  });
  assert.ok(!JSON.stringify(snapshot).includes(endpoint));
  assert.equal(snapshot.admission, 'Refused');
  assert.deepEqual(snapshot.exportFields.find(([key]) => key === 'Observation age (ms at snapshot)'), ['Observation age (ms at snapshot)', '30001 · stale']);
  assert.deepEqual(snapshot.exportFields.find(([key]) => key === 'Export data gaps'), ['Export data gaps', '0']);
  assert.deepEqual(snapshot.exportFields.find(([key]) => key === 'Quarantined records'), ['Quarantined records', '3']);
  assert.deepEqual(snapshot.exportFields.find(([key]) => key === 'Remote committed records'), ['Remote committed records', '12']);
  assert.throws(() => parseOperations({ error: endpoint }));
});

test('local commit evidence uses exact stage, latest outcome and paused ceiling', () => {
  const buffer = new TraceBuffer();
  ['storage.commit', 'target.report', 'storage.commit'].forEach((stage, sequence) => buffer.append(JSON.stringify({
    schema_version: { major: 1 }, process_instance_id: 'process', trace_sequence: sequence,
    observed_at: '2026-09-13T00:00:00Z', stage, direction: 'internal', outcome: { status: sequence === 2 ? 'failed' : 'succeeded' },
  }), 'process'));
  assert.equal(latestCommit(buffer.rows, Infinity)?.outcome, 'failed');
  assert.equal(latestCommit(buffer.rows, 1)?.sequence, 0);
  buffer.clear();
  assert.equal(latestCommit(buffer.rows, Infinity), undefined);
});

test('valid health 503 is readable while other 503 responses remain errors; reads create no provider work', async () => {
  const identity = parseIdentity({ bridge_id: 'b', runtime: { environment: 'demo', release_id: 'r', release_digest: 'sha256:r', process_instance_id: 'p' } });
  const calls: string[] = [];
  const transport: typeof fetch = async (url, init) => {
    assert.equal(init?.method ?? 'GET', 'GET');
    calls.push(new URL(String(url)).pathname);
    if (String(url).endsWith('/identity')) return Response.json(identity);
    return Response.json({ readiness: 'not_ready', components: { external_exporter: { state: 'disabled' } } }, { status: 503 });
  };
  assert.equal(parseOperations(await ApiClient.healthSnapshot('http://127.0.0.1:8080', identity, new AbortController().signal, transport)).exporter.state, 'disabled');
  const api = new ApiClient('http://127.0.0.1:8080', identity, 'fixture', transport);
  await assert.rejects(api.stations(), (error: unknown) => error instanceof ApiError && error.status === 503);
  assert.deepEqual(calls, ['/api/v1/identity', '/api/v1/health', '/api/v1/identity', '/api/v1/identity', '/api/v1/stations']);
  api.close();
});


test('captured station observations are bounded and honor the paused window', () => {
  const buffer = new TraceBuffer();
  for (let sequence = 0; sequence < 20; sequence++) buffer.append(JSON.stringify({
    schema_version: { major: 1 }, process_instance_id: 'p', trace_sequence: sequence,
    observed_at: '2026-09-13T00:00:00Z', stage: 'ocpp.receive', direction: 'inbound', outcome: { status: 'succeeded' },
    redacted_details: { fields: { station_id: `station-${sequence}`, protocol: 'ocpp2.0.1', action: 'Heartbeat' } },
  }), 'p');
  assert.equal(stationObservations(buffer.rows, Infinity).length, 10);
  const paused = stationObservations(buffer.rows, 3);
  assert.equal(paused.length, 4);
  assert.equal(paused[0].station, 'station-3');
  assert.equal(paused[0].protocol, 'ocpp2.0.1');
  assert.equal(paused[0].heartbeat, '2026-09-13T00:00:00Z');
});

test('health snapshots crossing a process change are rejected', async () => {
  const identity = parseIdentity({ bridge_id: 'b', runtime: { environment: 'demo', release_id: 'r', release_digest: 'sha256:r', process_instance_id: 'p' } });
  let calls = 0;
  const transport: typeof fetch = async url => {
    if (String(url).endsWith('/health')) return Response.json({ readiness: 'ready' });
    return Response.json(++calls === 1 ? identity : { ...identity, runtime: { ...identity.runtime, process_instance_id: 'replacement' } });
  };
  await assert.rejects(ApiClient.healthSnapshot('http://127.0.0.1:8080', identity, new AbortController().signal, transport), (error: unknown) => error instanceof ApiError && error.kind === 'identity');
});
