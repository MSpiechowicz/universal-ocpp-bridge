import { test } from 'node:test';
import assert from 'node:assert/strict';
import { composeOperation, compareDecimal, newDraft, parseDetail, parseHistory, parseSchemas } from '../src/commands/model';
import type { ResourceRef, StationSnapshot } from '../src/stations/schema';

const station: StationSnapshot = {
  schema_version: { major: 1, revision: 0 }, station: { bridge_id: 'b', station_id: 's' }, observed_at: '2026-09-25T12:00:00Z',
  connectivity: { status: 'connected' }, current_values: [], resources: [],
  capabilities: { optional: [], protocol_details: [], operations: [
    { operation: { kind: 'start' }, parameters: [{ name: 'authorization_reference', value_type: 'text', required: true, constraints: { enum_values: [] } }] },
    { operation: { kind: 'stop' }, parameters: [{ name: 'transaction_id', value_type: 'text', required: true, constraints: { enum_values: [] } }] },
    { operation: { kind: 'set_charging_limit' }, parameters: [
      { name: 'value', value_type: 'decimal', required: true, constraints: { minimum: { type: 'decimal', value: '6.000000000000000001' }, maximum: { type: 'decimal', value: '100000000000000000000' }, enum_values: [] } },
      { name: 'unit', value_type: 'named_enum', required: true, constraints: { enum_values: ['ampere'] } },
    ] },
    { operation: { kind: 'protocol_action', protocol: 'ocpp16j', action: 'ChangeAvailability' }, parameters: [] },
  ] },
  transactions: [{ transaction_id: 'tx-1', resource: { bridge_id: 'b', station_id: 's' }, state: 'active', started_at: '2026-09-25T12:00:00Z' }],
};
const schema = { resource: station.station, protocol: 'ocpp16j', action: 'ChangeAvailability', payload_schema: 'urn:OCPP:1.6:2019:12:ChangeAvailabilityRequest',
  fields: [{ name: 'connectorId', value_type: 'unsigned_integer', required: true },
    { name: 'type', value_type: 'named_enum', required: true, enum_values: ['Operative', 'Inoperative'] }] };
const evseResource: ResourceRef = {
  ...station.station, resource: { kind: 'evse', evse_id: 'evse-7' },
  native_protocol_reference: { protocol: 'ocpp201', evse_id: 7 },
};
const connectorResource: ResourceRef = {
  ...station.station, resource: { kind: 'evse', evse_id: 'evse-7', connector_id: 'connector-2' },
  native_protocol_reference: { protocol: 'ocpp201', evse_id: 7, connector_id: 2 },
};

const trigger201Schema = {
  resource: station.station, protocol: 'ocpp201', action: 'TriggerMessage', payload_schema: 'urn:OCPP:Cp:2:2020:3:TriggerMessageRequest',
  fields: [
    { name: 'requestedMessage', value_type: 'named_enum', required: true, enum_values: ['Heartbeat', 'MeterValues', 'StatusNotification'] },
    { name: 'evse.id', value_type: 'unsigned_integer', required: false },
    { name: 'evse.connectorId', value_type: 'unsigned_integer', required: false },
  ],
};
const triggerStation: StationSnapshot = { ...station, capabilities: {
  ...station.capabilities, operations: [...station.capabilities.operations,
    { operation: { kind: 'protocol_action', protocol: 'ocpp201', action: 'TriggerMessage' }, parameters: [] },
    { operation: { kind: 'protocol_action', protocol: 'ocpp16j', action: 'TriggerMessage' }, parameters: [] }],
}, resources: [evseResource, connectorResource].map(resource => ({
  resource, availability: 'available', capabilities: {
    ...station.capabilities, operations: [{ operation: { kind: 'protocol_action', protocol: 'ocpp201', action: 'TriggerMessage' }, parameters: [] }],
  }, data_points: [], current_values: [],
})) };

test('201 trigger composes station-only, EVSE and exact connector scope as nested wire JSON', () => {
  const compose = (values: Record<string, string>, resource: ResourceRef = station.station) =>
    composeOperation(triggerStation, resource, 'ocpp', values, { ...trigger201Schema, resource });

  assert.deepEqual(compose({ requestedMessage: 'Heartbeat' }).parameters,
    { protocol: 'ocpp201', action: 'TriggerMessage', payload_schema: trigger201Schema.payload_schema, payload: { requestedMessage: 'Heartbeat' } });
  assert.deepEqual(compose({ requestedMessage: 'MeterValues', 'evse.id': ' 7 ' }, evseResource).parameters,
    { protocol: 'ocpp201', action: 'TriggerMessage', payload_schema: trigger201Schema.payload_schema, payload: { requestedMessage: 'MeterValues', evse: { id: 7 } } });
  assert.deepEqual(compose({ requestedMessage: 'StatusNotification', 'evse.id': '7', 'evse.connectorId': '2' }, connectorResource).parameters,
    { protocol: 'ocpp201', action: 'TriggerMessage', payload_schema: trigger201Schema.payload_schema, payload: { requestedMessage: 'StatusNotification', evse: { id: 7, connectorId: 2 } } });
});

test('201 trigger rejects incomplete or malformed EVSE scopes before submission', () => {
  const compose = (values: Record<string, string>) =>
    composeOperation(triggerStation, station.station, 'ocpp', values, trigger201Schema);

  assert.throws(() => compose({ requestedMessage: 'MeterValues', 'evse.connectorId': '2' }), /evse\.id is required/);
  assert.throws(() => compose({ requestedMessage: 'StatusNotification' }), /evse\.id is required/);
  assert.throws(() => compose({ requestedMessage: 'StatusNotification', 'evse.id': '7' }), /evse\.connectorId is required/);
  for (const invalid of ['0', '-1', '1.5', 'abc', '2147483648']) {
    assert.throws(() => compose({ requestedMessage: 'MeterValues', 'evse.id': invalid }), /evse\.id (must be an integer|is out of range)/);
    assert.throws(() => compose({ requestedMessage: 'StatusNotification', 'evse.id': '7', 'evse.connectorId': invalid }), /evse\.connectorId (must be an integer|is out of range)/);
  }
  assert.throws(() => compose({ requestedMessage: 'MeterValues', 'evse.id': '9007199254740992' }), /evse\.id is out of range/);
  assert.deepEqual(compose({ requestedMessage: 'MeterValues', 'evse.id': '2147483647', 'evse.connectorId': '2147483647' }).parameters,
    { protocol: 'ocpp201', action: 'TriggerMessage', payload_schema: trigger201Schema.payload_schema,
      payload: { requestedMessage: 'MeterValues', evse: { id: 2147483647, connectorId: 2147483647 } } });
});

test('only pinned 201 trigger paths nest, while 16 trigger fields stay flat', () => {
  const flatSchema = { ...trigger201Schema, protocol: 'ocpp16j', payload_schema: 'urn:OCPP:1.6:2019:12:TriggerMessageRequest',
    fields: [{ name: 'requestedMessage', value_type: 'named_enum', required: true, enum_values: ['StatusNotification'] },
      { name: 'connectorId', value_type: 'unsigned_integer', required: false }] };
  assert.deepEqual(composeOperation(triggerStation, station.station, 'ocpp', { requestedMessage: 'StatusNotification', connectorId: '0' }, flatSchema).parameters,
    { protocol: 'ocpp16j', action: 'TriggerMessage', payload_schema: flatSchema.payload_schema,
      payload: { requestedMessage: 'StatusNotification', connectorId: 0 } });
  for (const name of ['evse.extra', 'constructor', '__proto__']) {
    const hostileSchema = { ...trigger201Schema, fields: [...trigger201Schema.fields, { name, value_type: 'text', required: false }] };
    assert.throws(() => composeOperation(triggerStation, station.station, 'ocpp', { requestedMessage: 'Heartbeat', [name]: 'bad' }, hostileSchema), /Unsupported schema field/);
  }
  assert.throws(() => composeOperation(triggerStation, station.station, 'ocpp', { requestedMessage: 'StatusNotification', 'evse.id': '7' },
    { ...flatSchema, fields: [...flatSchema.fields, { name: 'evse.id', value_type: 'unsigned_integer', required: false }] }), /Unsupported schema field/);
  assert.throws(() => composeOperation(triggerStation, station.station, 'ocpp', { requestedMessage: 'Heartbeat', 'evse.id': '7' },
    { ...trigger201Schema, fields: trigger201Schema.fields.map(field => field.name === 'evse.id' ? { ...field, value_type: 'text' } : field) }),
  /Unsupported schema field/);
});

test('exact charging decimals honor declared bounds without floating-point rounding', () => {
  assert.equal(compareDecimal('9007199254740992.000000000000000001', '9007199254740992'), 1);
  assert.throws(() => composeOperation(station, station.station, 'set_charging_limit', { value: '6', unit: 'ampere' }), /minimum/);
  assert.deepEqual(composeOperation(station, station.station, 'set_charging_limit', { value: '6.000000000000000001', unit: 'ampere', phases: '3' }),
    { kind: 'set_charging_limit', parameters: { value: '6.000000000000000001', unit: 'ampere', phases: 3 } });
  assert.throws(() => composeOperation(station, station.station, 'set_charging_limit', { value: '1e1', unit: 'ampere' }), /exact decimal/);
  assert.throws(() => composeOperation(station, station.station, 'set_charging_limit', { value: '100000000000000000001', unit: 'ampere' }), /maximum/);
  assert.throws(() => composeOperation(station, station.station, 'set_charging_limit', { value: '7', unit: 'watt' }), /not allowed/);
});

test('unsupported resource, transaction and privileged schema cannot generate a command', () => {
  assert.deepEqual(composeOperation(station, station.station, 'stop', { transaction_id: 'tx-1' }), { kind: 'stop', parameters: { transaction_id: 'tx-1' } });
  assert.throws(() => composeOperation(station, station.station, 'stop', { transaction_id: 'unknown' }), /active transaction/);
  assert.throws(() => composeOperation(station, { ...station.station, resource: { kind: 'connector', connector_id: '1' } }, 'start', { authorization_reference: 'a' }), /Resource is not/);
  assert.deepEqual(composeOperation(station, station.station, 'ocpp', { connectorId: '0', type: 'Operative' }, schema),
    { kind: 'ocpp', parameters: { protocol: 'ocpp16j', action: 'ChangeAvailability', payload_schema: schema.payload_schema, payload: { connectorId: 0, type: 'Operative' } } });
  assert.throws(() => composeOperation(station, station.station, 'ocpp', { connectorId: '-1', type: 'Operative' }, schema), /out of range/);
  assert.throws(() => composeOperation(station, station.station, 'ocpp', { connectorId: '0', type: 'unknown' }, schema), /not allowed/);
  assert.throws(() => composeOperation(station, station.station, 'ocpp', { connectorId: '0', type: 'Operative' }, { ...schema, resource: { ...station.station, station_id: 'other' } }), /not advertised/);
});

test('new submissions receive independent UUID identities and exact UTC deadlines', () => {
  const operation = composeOperation(station, station.station, 'start', { authorization_reference: 'reference' });
  const submitted = newDraft(station.station, operation, Date.parse('2026-09-25T12:00:00Z'));
  const next = newDraft(station.station, operation, Date.parse('2026-09-25T12:00:30Z'));
  assert.match(submitted.request_id, /^[\da-f]{8}-[\da-f]{4}-4[\da-f]{3}-[89ab][\da-f]{3}-[\da-f]{12}$/i);
  assert.notEqual(submitted.request_id, submitted.correlation_id);
  assert.notEqual(submitted.request_id, next.request_id);
  assert.equal(submitted.expires_at, '2026-09-25T12:02:00.000Z');
  assert.equal(next.expires_at, '2026-09-25T12:02:30.000Z');
});

test('history is scoped and sanitized, status verifies request and effect linkage independently', () => {
  const effect = { event_id: 'event-1', event_type: 'transaction.started', observed_at: '2026-09-25T12:01:00Z' };
  const source = { request_id: 'r1', correlation_id: 'c1', resource: station.station, operation: 'start',
    lifecycle: { stage: 'protocol_response', accepted: true }, admitted_at: '2026-09-25T12:00:00Z', expires_at: '2026-09-25T12:02:00Z', observed_effects: [effect], secret: 'not for UI' };
  assert.deepEqual(parseHistory({ items: [source] }, station.station).items[0].observed_effects, [effect]);
  assert.equal('secret' in parseHistory({ items: [source] }, station.station).items[0], false);
  assert.equal(parseDetail({ ...source, return_route: { request_id: 'r1', origin: { principal_id: 'private' } } }, 'r1', station.station).lifecycle?.accepted, true);
  assert.throws(() => parseDetail({ ...source, return_route: { request_id: 'different' } }, 'r1', station.station), /request mismatch/);
  assert.throws(() => parseHistory({ items: [{ ...source, resource: { ...station.station, station_id: 'other' } }] }, station.station), /scope mismatch/);
  assert.deepEqual(parseSchemas({ items: [schema] }, station.station).items[0].fields, schema.fields);
  const protectedStart = parseSchemas({ items: [schema], start: { resource: station.station, authorization_reference: 'opaque' } }, station.station).start;
  assert.equal(protectedStart?.authorization_reference, 'opaque');
  assert.equal(protectedStart?.resource.station_id, station.station.station_id);
  assert.throws(() => parseSchemas({ items: [schema], start: { resource: { ...station.station, station_id: 'other' }, authorization_reference: 'opaque' } }, station.station), /Start scope mismatch/);
  assert.throws(() => parseSchemas({ items: [{ ...schema, resource: { ...station.station, station_id: 'other' } }] }, station.station), /scope mismatch/);
});

test('accepted request IDs longer than 4096 characters remain readable in history and detail', () => {
  const requestId = 'r'.repeat(5000);
  const row = { request_id: requestId, resource: station.station, observed_effects: [] };

  assert.equal(parseHistory({ items: [row] }, station.station).items[0].request_id, requestId);
  assert.equal(parseDetail({ ...row, return_route: { request_id: requestId } }, requestId, station.station).request_id, requestId);
  assert.throws(() => parseDetail({ ...row, return_route: { request_id: 'different' } }, requestId, station.station), /request mismatch/);
  assert.throws(() => parseHistory({ items: [{ ...row, resource: { ...station.station, station_id: 'other' } }] }, station.station), /scope mismatch/);
  assert.throws(() => parseHistory({ items: [{ ...row, request_id: `r\n${requestId}` }] }, station.station), /Invalid response/);
  assert.throws(() => parseHistory({ items: [{ ...row, request_id: '' }] }, station.station), /Invalid response/);
  assert.throws(() => parseHistory({ items: [{ ...row, request_id: 'r'.repeat(64 * 1024 + 1) }] }, station.station), /Invalid response/);
});

test('command history rejects invalid and oversized next cursors while retaining row scope', () => {
  const row = { request_id: 'r'.repeat(300), resource: station.station, observed_effects: [] };
  const page = { items: [row], next_cursor: 'uob:command:opaque-anchor' };
  assert.equal(parseHistory(page, station.station).items[0].request_id, row.request_id);
  assert.equal(parseHistory(page, station.station).next_cursor, page.next_cursor);
  assert.equal(parseHistory({ ...page, next_cursor: `uob:command:${'a'.repeat(8179)}` }, station.station).next_cursor?.length, 8191);
  assert.throws(() => parseHistory({ ...page, next_cursor: `uob:command:${'a'.repeat(8181)}` }, station.station), /Invalid response/);
  assert.throws(() => parseHistory({ ...page, next_cursor: `uob:command:${'é'.repeat(4090)}` }, station.station), /Invalid response/);
  assert.throws(() => parseHistory({ ...page, next_cursor: 'uob:command:' }, station.station), /Invalid response/);
  assert.throws(() => parseHistory({ ...page, next_cursor: 'uob:station:abc' }, station.station), /Invalid response/);
  assert.throws(() => parseHistory({ ...page, next_cursor: 'uob:command:bad\ncursor' }, station.station), /Invalid response/);
  assert.throws(() => parseHistory({ ...page, items: [{ ...row, resource: { ...station.station, station_id: 'other' } }] }, station.station), /scope mismatch/);
});
