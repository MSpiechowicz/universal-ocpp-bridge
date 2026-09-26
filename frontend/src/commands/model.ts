import { boundedText, object } from '../identity';
import type { Capabilities, ResourceRef, StationSnapshot } from '../stations/schema';
import { parseResourceRef } from '../stations/schema';

export interface SchemaField { name: string; value_type: string; required: boolean; enum_values?: string[] }
export interface CommandSchema { resource: ResourceRef; protocol: string; action: string; payload_schema: string; fields: SchemaField[] }
export interface CommandOptions { items: CommandSchema[]; start?: { resource: ResourceRef; authorization_reference: string } }
export interface Effect { event_id: string; event_type: string; observed_at: string }
export interface Lifecycle { stage: string; accepted?: boolean; error?: { code: string; detail?: string }; detail?: string }
export interface CommandRow {
  request_id: string; correlation_id?: string; resource: ResourceRef; lifecycle?: Lifecycle;
  operation?: string; admitted_at?: string; expires_at?: string; recorded_at?: string; observed_effects: Effect[];
}
export interface CommandPage { items: CommandRow[]; next_cursor?: string }
export interface Draft { resource: ResourceRef; operation: { kind: string; parameters: unknown }; request_id: string; correlation_id: string; expires_at: string }

const list = (value: unknown, limit: number): unknown[] => {
  if (!Array.isArray(value) || value.length > limit) throw new Error('Invalid command response');
  return value;
};
const optional = (value: unknown): string | undefined => value == null ? undefined : boundedText(value, 512);
// Command history cursors are printable ASCII, so the server's 8192-byte limit is also a character limit.
export function commandHistoryCursor(value: unknown): string {
  const cursor = boundedText(value, 8192);
  if (!cursor.startsWith('uob:command:') || cursor === 'uob:command:' || /[^\x20-\x7e]/.test(cursor)) {
    throw new Error('Invalid response');
  }
  return cursor;
}
// The management command POST body is limited to 64 KiB of UTF-8 JSON, so an admitted ID cannot exceed this many characters.
export const commandRequestId = (value: unknown): string => boundedText(value, 64 * 1024);
const resourceKey = (resource: ResourceRef) => JSON.stringify([resource.bridge_id, resource.station_id, resource.resource]);
export const sameResource = (left: ResourceRef, right: ResourceRef) => resourceKey(left) === resourceKey(right);

function effects(value: unknown): Effect[] {
  return list(value ?? [], 100).map(entry => {
    const effect = object(entry);
    return { event_id: boundedText(effect.event_id), event_type: boundedText(effect.event_type), observed_at: boundedText(effect.observed_at) };
  });
}
function lifecycle(value: unknown): Lifecycle | undefined {
  if (value == null) return undefined;
  const state = object(value);
  return { stage: boundedText(state.stage, 64), accepted: typeof state.accepted === 'boolean' ? state.accepted : undefined,
    error: state.error == null ? undefined : { code: boundedText(object(state.error).code, 64), detail: optional(object(state.error).detail) },
    detail: optional(state.detail) };
}
function row(value: unknown, station: ResourceRef, requestId?: string): CommandRow {
  const item = object(value);
  const resource = parseResourceRef(item.resource);
  if (resource.bridge_id !== station.bridge_id || resource.station_id !== station.station_id) throw new Error('Command scope mismatch');
  return { request_id: commandRequestId(requestId ?? item.request_id), correlation_id: optional(item.correlation_id), resource,
    operation: optional(item.operation), lifecycle: lifecycle(item.lifecycle), admitted_at: optional(item.admitted_at),
    expires_at: optional(item.expires_at), recorded_at: optional(item.recorded_at), observed_effects: effects(item.observed_effects) };
}
export function parseHistory(value: unknown, station: ResourceRef): CommandPage {
  const page = object(value);
  return { items: list(page.items, 50).map(entry => row(entry, station)), next_cursor: page.next_cursor == null ? undefined : commandHistoryCursor(page.next_cursor) };
}
export function parseDetail(value: unknown, requestId: string, station: ResourceRef): CommandRow {
  const result = object(value), route = object(result.return_route);
  if (route.request_id !== requestId) throw new Error('Command request mismatch');
  return row(result, station, requestId);
}
export function parseSchemas(value: unknown, station: ResourceRef): CommandOptions {
  const response = object(value);
  const start = response.start == null ? undefined : object(response.start);
  const startResource = start ? parseResourceRef(start.resource) : undefined;
  if (startResource && (startResource.bridge_id !== station.bridge_id || startResource.station_id !== station.station_id)) throw new Error('Start scope mismatch');
  return { start: startResource ? { resource: startResource, authorization_reference: boundedText(start!.authorization_reference, 1024) } : undefined,
    items: list(response.items, 50).map(entry => {
      const item = object(entry), resource = parseResourceRef(item.resource);
      if (resource.bridge_id !== station.bridge_id || resource.station_id !== station.station_id) throw new Error('Schema scope mismatch');
      return { resource, protocol: boundedText(item.protocol, 32), action: boundedText(item.action, 64),
        payload_schema: boundedText(item.payload_schema, 256), fields: list(item.fields, 24).map(raw => {
          const field = object(raw);
          if (typeof field.required !== 'boolean') throw new Error('Invalid schema field');
          return { name: boundedText(field.name, 64), value_type: boundedText(field.value_type, 32), required: field.required,
            ...(field.enum_values == null ? {} : { enum_values: list(field.enum_values, 30).map(choice => boundedText(choice, 128)) }) };
        }) };
    }) };
}

function decimal(value: string): { coefficient: bigint; scale: number } {
  if (!/^[+-]?(?:0|[1-9]\d*)(?:\.\d+)?$/.test(value) || value.length > 80) throw new Error('Enter an exact decimal, without exponent or rounding.');
  const [whole, fraction = ''] = value.replace(/^\+/, '').split('.');
  const coefficient = BigInt(`${whole}${fraction}`);
  if (coefficient < -(1n << 127n) || coefficient > (1n << 127n) - 1n) throw new Error('Charging limit exceeds exact decimal range.');
  return { coefficient, scale: fraction.length };
}
export function compareDecimal(left: string, right: string): number {
  const a = decimal(left), b = decimal(right);
  const difference = a.coefficient * 10n ** BigInt(b.scale) - b.coefficient * 10n ** BigInt(a.scale);
  return difference < 0n ? -1 : difference > 0n ? 1 : 0;
}
function parameters(capabilities: Capabilities, kind: string) {
  const operation = capabilities.operations.find(item => item.operation.kind === kind);
  if (!operation) throw new Error('Operation is not advertised for this resource.');
  return operation.parameters;
}
export function composeOperation(snapshot: StationSnapshot, resource: ResourceRef, kind: string, values: Record<string, string>, schema?: CommandSchema) {
  const owner = sameResource(resource, snapshot.station) ? snapshot : snapshot.resources.find(item => sameResource(item.resource, resource));
  if (!owner) throw new Error('Resource is not in the selected station.');
  if (kind === 'ocpp') {
    if (!schema || !sameResource(schema.resource, resource) || !owner.capabilities.operations.some(item =>
      item.operation.kind === 'protocol_action' && item.operation.protocol === schema.protocol && item.operation.action === schema.action)) {
      throw new Error('Privileged action is not advertised with a supported schema.');
    }
    const payload: Record<string, string | number | boolean> = {};
    for (const field of schema.fields) {
      const input = values[field.name]?.trim() ?? '';
      if (!input) { if (field.required) throw new Error(`${field.name} is required.`); continue; }
      if (field.enum_values && !field.enum_values.includes(input)) throw new Error(`${field.name} is not allowed.`);
      if (field.value_type === 'unsigned_integer' || field.value_type === 'signed_integer') {
        if (!/^-?(?:0|[1-9]\d*)$/.test(input)) throw new Error(`${field.name} must be an integer.`);
        const number = BigInt(input);
        if (number < (field.value_type === 'unsigned_integer' ? 0n : -(1n << 53n) + 1n) || number > BigInt(Number.MAX_SAFE_INTEGER)) throw new Error(`${field.name} is out of range.`);
        payload[field.name] = Number(number);
      } else if (field.value_type === 'boolean') {
        if (input !== 'true' && input !== 'false') throw new Error(`${field.name} must be boolean.`);
        payload[field.name] = input === 'true';
      } else if (field.value_type === 'text' || field.value_type === 'named_enum') payload[field.name] = boundedText(input, 1024);
      else throw new Error(`${field.name} has an unsupported field type.`);
    }
    return { kind, parameters: { protocol: schema.protocol, action: schema.action, payload_schema: schema.payload_schema, payload } };
  }
  const declared = parameters(owner.capabilities, kind);
  if (kind === 'start') {
    const reference = boundedText(values.authorization_reference?.trim());
    const param = declared.find(item => item.name === 'authorization_reference');
    if (param?.constraints.enum_values.length && !param.constraints.enum_values.includes(reference)) throw new Error('Authorization reference is not allowed.');
    return { kind, parameters: { authorization_reference: reference } };
  }
  if (kind === 'stop') {
    const transaction_id = boundedText(values.transaction_id);
    if (!snapshot.transactions.some(tx => tx.transaction_id === transaction_id && tx.state !== 'ended' &&
      (sameResource(resource, snapshot.station) || sameResource(tx.resource, resource)))) throw new Error('Select an active transaction on this resource.');
    return { kind, parameters: { transaction_id } };
  }
  if (kind === 'set_charging_limit') {
    const value = values.value?.trim() ?? '';
    decimal(value);
    const unit = values.unit;
    if (!['ampere', 'milliampere', 'watt', 'kilowatt'].includes(unit)) throw new Error('Choose a supported charging limit unit.');
    const phases = values.phases?.trim();
    if (phases && !['1', '2', '3'].includes(phases)) throw new Error('Phases must be 1, 2 or 3.');
    for (const field of declared) {
      const input = values[field.name]?.trim();
      if (field.required && !input) throw new Error(`${field.name} is required.`);
      if (input && field.constraints.enum_values.length && !field.constraints.enum_values.includes(input)) throw new Error(`${field.name} is not allowed.`);
      if (input && (field.name === 'value' || field.name === 'phases')) {
        const { minimum, maximum } = field.constraints;
        if (minimum && compareDecimal(input, String(minimum.value)) < 0) throw new Error(`${field.name} is below the declared minimum.`);
        if (maximum && compareDecimal(input, String(maximum.value)) > 0) throw new Error(`${field.name} exceeds the declared maximum.`);
      }
    }
    return { kind, parameters: { value, unit, ...(phases ? { phases: Number(phases) } : {}) } };
  }
  throw new Error('Unsupported command operation.');
}
export function newDraft(resource: ResourceRef, operation: Draft['operation'], now = Date.now()): Draft {
  return { resource, operation, request_id: crypto.randomUUID(), correlation_id: crypto.randomUUID(), expires_at: new Date(now + 120000).toISOString() };
}
