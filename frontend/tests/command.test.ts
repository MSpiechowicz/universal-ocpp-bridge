import { test } from 'node:test';
import assert from 'node:assert/strict';
import { parseRow, TraceBuffer, ROW_LIMIT } from '../src/debug/buffer';
import { commandEvidence, commandTrace, commandMetadata, COMMAND_LINK_LIMIT } from '../src/debug/command';

function raw(sequence: number, stage = 'command.ingress', evidence = 'completed', extra: Record<string, unknown> = {}) {
  return JSON.stringify({ schema_version: { major: 1 }, process_instance_id: 'p1', trace_sequence: sequence,
    stage, direction: 'internal', observed_at: '2026-09-12T12:00:00Z', outcome: { status: 'succeeded' },
    correlation_id: 'c1', redacted_details: { fields: { evidence, 'command.request_id': 'r1', station_id: 's1' } }, ...extra });
}
const row = (sequence: number, stage: string, evidence: string, extra = {}) => parseRow(raw(sequence, stage, evidence, extra), 'p1');

test('API-only acceptance, peer acknowledgement and charger acceptance never imply physical effect', () => {
  const api = row(1, 'management.delivery', 'locally_exposed');
  const trace = commandTrace([api], 1)!;
  assert.equal(trace.evidence.length, 1);
  assert.ok(!trace.evidence.some(row => row.stage === 'command.dispatch' || row.stage === 'command.observed_effect'));
  assert.match(commandEvidence(api), /client consumption are not established/);
  assert.match(commandEvidence(row(2, 'target.report', 'peer_acknowledged')), /physical effect remain separate/);
  assert.match(commandEvidence(row(3, 'command.protocol_response', 'charger_accepted')), /physical effect remains separate/);
  assert.match(commandEvidence(row(4, 'command.observed_effect', 'observed_effect')), /explicitly linked and persisted/);
  assert.match(commandEvidence(row(5, 'management.delivery', 'charger_accepted')), /Unclassified/);
  assert.match(commandEvidence(row(6, 'command.protocol_response', 'charger_accepted', { outcome: { status: 'failed' } })), /Unclassified/);
});

test('uncertainty, duplicates, disconnected rejection and safe reasons retain their semantics', () => {
  assert.match(commandEvidence(row(1, 'command.protocol_response', 'uncertain', { outcome: { status: 'uncertain' } })), /Automatic replay is unsafe/);
  assert.match(commandEvidence(row(2, 'command.deduplication', 'duplicate')), /does not prove another dispatch/);
  const disconnected = row(3, 'application', 'not_transmitted', { outcome: { status: 'failed' }, redacted_details: { fields: { evidence: 'not_transmitted', reason_code: 'StationDisconnected' } } });
  assert.equal(disconnected.command.reason, 'StationDisconnected');
  assert.match(commandEvidence(disconnected), /not transmitted/);
});

test('request joins require explicit identity; shared correlations are links, never other request outcomes', () => {
  const first = row(1, 'command.ingress', 'completed');
  const effect = row(2, 'command.observed_effect', 'observed_effect', { redacted_details: { fields: { evidence: 'observed_effect', 'command.request_id': 'r2', station_id: 's1' } } });
  const otherStation = row(3, 'command.dispatch', 'completed', { redacted_details: { fields: { 'command.request_id': 'r1', station_id: 's2' } } });
  const rows = [first, effect, otherStation, { ...first, sequence: 4, command: { ...first.command, process: 'p2' } }];
  const trace = commandTrace(rows, 1)!;
  assert.deepEqual(trace.evidence.map(row => row.sequence), [1]);
  assert.deepEqual(trace.related.map(row => row.sequence), [2]);
  const missing = row(5, 'command.dispatch', 'completed', { correlation_id: null, redacted_details: { fields: {} } });
  assert.equal(commandTrace([...rows, missing], 5)!.evidence.length, 1);
  assert.equal(commandTrace([...rows, missing], 5)!.related.length, 0);
  assert.equal(commandTrace(rows, 2, 1), undefined);
});

test('oversized identities are unavailable rather than truncated into false matches; timing is exact and local', () => {
  const oversized = commandMetadata({ correlation_id: 'c'.repeat(1025), duration_micros: Number.MAX_SAFE_INTEGER + 1,
    redacted_details: { truncated: true, fields: { 'command.request_id': 'r'.repeat(257) } } });
  assert.equal(oversized.correlation, ''); assert.equal(oversized.request, '');
  assert.equal(oversized.duration, ''); assert.equal(oversized.partial, true);
  assert.equal(row(2, 'command.dispatch', 'completed', { duration_micros: 0 }).command.duration, '0');
});

test('command links honor pause and eviction, remain bounded, and do not retain a second history', () => {
  const buffer = new TraceBuffer();
  for (let sequence = 0; sequence < ROW_LIMIT + 4; sequence++) buffer.append(raw(sequence, 'target.report', 'peer_acknowledged', {
    redacted_details: { fields: { evidence: 'peer_acknowledged', station_id: 's1' } },
  }), 'p1');
  assert.equal(commandTrace(buffer.rows, 0), undefined);
  const trace = commandTrace(buffer.rows, 4)!;
  assert.equal(trace.related.length, COMMAND_LINK_LIMIT); assert.ok(trace.omitted > 0);
  assert.equal(commandTrace(buffer.rows, 4, 5)!.related.length, 1);
  buffer.clear(); assert.equal(commandTrace(buffer.rows, 4), undefined);
});
