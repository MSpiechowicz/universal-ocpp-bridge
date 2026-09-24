import { boundedText, object } from '../identity';

export interface ResourceRef {
  bridge_id: string; station_id: string;
  resource?: { kind: 'connector'; connector_id: string } | { kind: 'evse'; evse_id: string; connector_id?: string };
  native_protocol_reference?: { protocol: 'ocpp16'; connector_id: number } | { protocol: 'ocpp201'; evse_id: number; connector_id?: number };
}
export interface TypedValue { type: string; value: string | boolean }
export interface Constraints { minimum?: TypedValue; maximum?: TypedValue; enum_values: string[] }
export interface Capabilities {
  operations: { operation: { kind: string; protocol?: string; action?: string }; parameters: { name: string; value_type: string; required: boolean; constraints: Constraints }[] }[];
  optional: { name: string; value?: TypedValue }[];
  protocol_details: { protocol: string; name: string; value: TypedValue }[];
}
export interface PointValue {
  point_id: string; value?: TypedValue; source_time?: string; observed_at: string;
  quality: { level: string; reason?: string }; freshness: { status: string; valid_until?: string };
  measurement?: { original_value: string; original_unit?: string; measurand?: string; phase?: string; context?: string; location?: string };
}
export interface StationSnapshot {
  schema_version: { major: number; revision: number }; station: ResourceRef; observed_at: string;
  connectivity: { status: string; protocol?: string; connected_at?: string; last_message_at?: string };
  capabilities: Capabilities;
  resources: { resource: ResourceRef; availability: string; capabilities: Capabilities;
    data_points: { point_id: string; semantic_name: string; value_type: string; unit?: string; access: string; constraints: Constraints }[];
    current_values: PointValue[] }[];
  transactions: { transaction_id: string; resource: ResourceRef; state: string; started_at: string; ended_at?: string }[];
  current_values: PointValue[];
}
export interface StationPage { items: StationSnapshot[]; next_cursor?: string }
const MAX_ROWS = 200;
function text(value: unknown, max = 1024): string { return boundedText(value, max); }
function optional(value: unknown): string | undefined { return value == null ? undefined : text(value); }
function list<T>(value: unknown, parse: (entry: unknown) => T, max = MAX_ROWS): T[] {
  if (value === undefined) return [];
  if (!Array.isArray(value) || value.length > max) throw new Error('Invalid station response');
  return value.map(parse);
}
function unsigned(value: unknown): number {
  if (typeof value !== 'number' || !Number.isSafeInteger(value) || value < 0 || value > 4294967295) throw new Error('Unsafe native number');
  return value;
}
function ref(value: unknown): ResourceRef {
  const item = object(value);
  const resource = item.resource == null ? undefined : object(item.resource);
  let canonical: ResourceRef['resource'];
  if (resource) {
    if (resource.kind === 'connector') canonical = { kind: 'connector', connector_id: text(resource.connector_id) };
    else if (resource.kind === 'evse') canonical = { kind: 'evse', evse_id: text(resource.evse_id), connector_id: optional(resource.connector_id) };
    else throw new Error('Invalid resource kind');
  }
  const native = item.native_protocol_reference == null ? undefined : object(item.native_protocol_reference);
  let nativeRef: ResourceRef['native_protocol_reference'];
  if (native) {
    if (native.protocol === 'ocpp16') nativeRef = { protocol: 'ocpp16', connector_id: unsigned(native.connector_id) };
    else if (native.protocol === 'ocpp201') nativeRef = { protocol: 'ocpp201', evse_id: unsigned(native.evse_id), connector_id: native.connector_id == null ? undefined : unsigned(native.connector_id) };
    else throw new Error('Invalid native protocol');
  }
  return { bridge_id: text(item.bridge_id), station_id: text(item.station_id), resource: canonical, native_protocol_reference: nativeRef };
}
function typed(value: unknown): TypedValue {
  const item = object(value); const type = text(item.type, 64);
  if (type === 'boolean') { if (typeof item.value !== 'boolean') throw new Error('Invalid boolean'); return { type, value: item.value }; }
  if (type === 'signed_integer' || type === 'unsigned_integer') {
    // HTTP parsing quotes unsafe integer lexemes before JSON.parse can round them.
    if (typeof item.value !== 'string' && (typeof item.value !== 'number' || !Number.isSafeInteger(item.value))) throw new Error('Unsafe integer');
    const exact = String(item.value);
    if (!/^-?(?:0|[1-9]\d*)$/.test(exact) || exact.length > 21) throw new Error('Invalid integer');
    const integer = BigInt(exact);
    if (type === 'signed_integer' ? integer < -(1n << 63n) || integer > (1n << 63n) - 1n : integer < 0n || integer > (1n << 64n) - 1n) throw new Error('Out of range integer');
    return { type, value: exact };
  }
  if (type === 'decimal') {
    const decimal = text(item.value);
    if (!/^[+-]?(?:\d+)(?:\.\d+)?$/.test(decimal)) throw new Error('Invalid decimal');
    return { type, value: decimal };
  }
  if (type === 'text' || type === 'named_enum') return { type, value: text(item.value, 4096) };
  throw new Error('Invalid value type');
}
function constraints(value: unknown): Constraints {
  const item = value === undefined ? {} : object(value);
  return { minimum: item.minimum == null ? undefined : typed(item.minimum), maximum: item.maximum == null ? undefined : typed(item.maximum), enum_values: list(item.enum_values, entry => text(entry), 100) };
}
function capabilities(value: unknown): Capabilities {
  const item = object(value);
  return {
    operations: list(item.operations, entry => {
      const row = object(entry), operation = object(row.operation);
      const kind = text(operation.kind, 64);
      return { operation: { kind, protocol: optional(operation.protocol), action: optional(operation.action) },
        parameters: list(row.parameters, param => { const field = object(param);
          if (typeof field.required !== 'boolean') throw new Error('Invalid parameter');
          return { name: text(field.name), value_type: text(field.value_type, 64), required: field.required, constraints: constraints(field.constraints) }; }, 50) };
    }, 100),
    optional: list(item.optional, entry => { const row = object(entry); return { name: text(row.name), value: row.value == null ? undefined : typed(row.value) }; }),
    protocol_details: list(item.protocol_details, entry => { const row = object(entry); return { protocol: text(row.protocol, 64), name: text(row.name), value: typed(row.value) }; }),
  };
}
function points(value: unknown): PointValue[] {
  return list(value, entry => {
    const row = object(entry), quality = object(row.quality), freshness = object(row.freshness);
    const measurement = row.measurement == null ? undefined : object(row.measurement);
    return { point_id: text(row.point_id), value: row.value == null ? undefined : typed(row.value),
      source_time: optional(row.source_time), observed_at: text(row.observed_at),
      quality: { level: text(quality.level, 64), reason: optional(quality.reason) },
      freshness: { status: text(freshness.status, 64), valid_until: optional(freshness.valid_until) },
      measurement: measurement ? { original_value: text(measurement.original_value, 4096), original_unit: optional(measurement.original_unit),
        measurand: optional(measurement.measurand), phase: optional(measurement.phase), context: optional(measurement.context), location: optional(measurement.location) } : undefined };
  });
}
export function parseStation(value: unknown, bridge: string): StationSnapshot {
  const row = object(value), station = ref(row.station), connectivity = object(row.connectivity);
  if (station.bridge_id !== bridge || station.resource) throw new Error('Station identity mismatch');
  const version = object(row.schema_version);
  if (typeof version.major !== 'number' || !Number.isSafeInteger(version.major) || version.major !== 1 ||
      typeof version.revision !== 'number' || !Number.isSafeInteger(version.revision) || version.revision < 0 || version.revision > 65535) throw new Error('Unsupported schema version');
  return { schema_version: { major: version.major, revision: version.revision }, station, observed_at: text(row.observed_at),
    connectivity: { status: text(connectivity.status, 64), protocol: optional(connectivity.protocol), connected_at: optional(connectivity.connected_at), last_message_at: optional(connectivity.last_message_at) },
    capabilities: capabilities(row.capabilities),
    resources: list(row.resources, entry => { const resource = object(entry), address = ref(resource.resource);
      if (address.bridge_id !== bridge || address.station_id !== station.station_id || !address.resource) throw new Error('Resource identity mismatch');
      return { resource: address, availability: text(resource.availability, 64), capabilities: capabilities(resource.capabilities),
        data_points: list(resource.data_points, descriptor => { const point = object(descriptor), owner = ref(point.resource);
          if (owner.bridge_id !== bridge || owner.station_id !== station.station_id) throw new Error('Point identity mismatch');
          return { point_id: text(point.point_id), semantic_name: text(point.semantic_name), value_type: text(point.value_type, 64),
            unit: optional(point.unit), access: text(point.access, 64), constraints: constraints(point.constraints) }; }), current_values: points(resource.current_values) };
    }),
    transactions: list(row.transactions, entry => { const tx = object(entry), address = ref(tx.resource);
      if (address.bridge_id !== bridge || address.station_id !== station.station_id) throw new Error('Transaction identity mismatch');
      return { transaction_id: text(tx.transaction_id), resource: address, state: text(tx.state, 64), started_at: text(tx.started_at), ended_at: optional(tx.ended_at) }; }),
    current_values: points(row.current_values) };
}
export function parsePage(value: unknown, bridge: string): StationPage {
  const row = object(value);
  if (!Array.isArray(row.items)) throw new Error('Invalid station page');
  return { items: list(row.items, entry => parseStation(entry, bridge), 10), next_cursor: optional(row.next_cursor) };
}
