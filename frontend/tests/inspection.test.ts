import { test } from 'node:test';
import assert from 'node:assert/strict';
import { inspectRecord, jsonTokens, decisionExplanation } from '../src/debug/inspection';
import { TraceBuffer, DETAIL_LIMIT } from '../src/debug/buffer';

const record = (sequence: number, fields: Record<string, unknown> = {}) => ({
  schema_version: { major: 1, revision: 0 }, process_instance_id: 'p1', trace_sequence: sequence,
  stage: 'validation', direction: 'inbound', observed_at: '2026-09-12T12:00:00Z',
  outcome: { status: 'failed' }, correlation_id: 'call-1', parent_trace_id: 'receive-1',
  target: { kind: 'mqtt', instance_id: 'main' }, redacted_details: { fields },
});

test('inspection keeps field paths, mappings, native units and unsupported data explicit', () => {
  const inspection = inspectRecord(record(1, {
    'source.value': '12', 'canonical.value': '12.0', 'target.value': '12.0',
    'validation.field_path': '/meterValue/0/sampledValue/0/value', 'validation.code': 'invalid_decimal',
    'mapping.topic': 'site/station/meter', 'mapping.api': '/api/v1/stations',
    unit: 'Wh', quality: 'invalid', 'opaque.vendor': '[omitted]', surprise: '<script>alert(1)</script>',
    'resources.0.availability': 'Available -> Unavailable', evidence: 'rejected', 'decision.reason': 'invalid_value',
    'details.original_size': '90000', 'details.omitted_fields': '2', state_details_omitted: 'true',
  }));
  assert.deepEqual(inspection.validation[0], ['validation.field_path', '/meterValue/0/sampledValue/0/value']);
  assert.equal(inspection.mappings.length, 2); assert.equal(inspection.measurements.length, 2);
  assert.ok(inspection.unsupported.some(([key, value]) => key === 'surprise' && value.includes('<script>')));
  assert.deepEqual(inspection.changes, [{ path: 'resources.0.availability', before: 'Available', after: 'Unavailable' }]);
  assert.equal(inspection.trigger, 'receive-1'); assert.equal(inspection.correlation, 'call-1');
  assert.equal(inspection.reason, 'invalid_value'); assert.equal(inspection.omitted, true);
  assert.ok(inspection.sizes.some(([key, value]) => key === 'details.original_size' && value === '90000'));
});

test('missing and unknown evidence is not invented; malformed transitions remain unsupported', () => {
  const inspection = inspectRecord({ redacted_details: { fields: { 'resources.0.availability': 'arbitrary', evidence: 'constructor' } } });
  assert.equal(inspection.trigger, ''); assert.equal(inspection.changes.length, 0);
  assert.equal(inspection.unsupported.length, 1); assert.equal(inspection.omitted, true);
  assert.match(decisionExplanation(inspection.evidence), /no further decision/);
  for (const evidence of ['rejected', 'duplicate', 'stale', 'reconciled', 'uncertain']) assert.ok(decisionExplanation(evidence).length > 30);
});

test('field, value, changed-field and syntax node bounds retain explicit partial evidence', () => {
  const fields = Object.fromEntries(Array.from({ length: 150 }, (_, i) => [`resources.${i}.availability`, 'Available -> Unavailable']));
  const inspection = inspectRecord(record(1, { 'source.large': 'x'.repeat(5000), ...fields }));
  assert.equal(inspection.changes.length, 16); assert.ok(inspection.omitted);
  assert.match(inspection.source[0][1], /display truncated/);
  const text = JSON.stringify(Array(10000).fill('<img src=x onerror=alert(1)>'));
  const tokens = jsonTokens(text);
  assert.ok(tokens.length <= 1024); assert.equal(tokens.map(token => token.text).join(''), text);
});

test('details parse only selected rows once per cached entry; caches clear and cap jointly', () => {
  const buffer = new TraceBuffer();
  for (let i = 0; i < 20; i++) buffer.append(JSON.stringify(record(i, { 'source.value': 'x'.repeat(2000) })), 'p1');
  const original = JSON.parse;
  let parses = 0;
  JSON.parse = (...args) => { parses++; return original(...args); };
  try {
    const first = buffer.inspection(0);
    for (let i = 0; i < 20; i++) { buffer.detail(0); assert.equal(buffer.inspection(0), first); }
    assert.equal(parses, 1);
    for (let i = 1; i < 20; i++) { buffer.inspection(i); assert.ok(buffer.detailBytes <= DETAIL_LIMIT); }
    assert.equal(parses, 20);
    buffer.inspection(0); assert.equal(parses, 21); // FIFO entry cap evicted it.
    buffer.clear(); assert.equal(buffer.detailBytes, 0); assert.equal(buffer.inspection(0), undefined);
  } finally { JSON.parse = original; }
});

test('deep or expansive unknown JSON is refused once without repeatedly formatting it', () => {
  const buffer = new TraceBuffer();
  const raw = JSON.stringify(record(0)).replace('"stage":"validation"', '"extension":' + '['.repeat(1000) + '0' + ']'.repeat(1000) + ',"stage":"validation"');
  buffer.append(raw, 'p1');
  assert.match(buffer.detail(0)!, /safe formatting depth/);
  assert.equal(buffer.inspection(0), undefined);
  assert.ok(buffer.detailBytes < 100);
});
