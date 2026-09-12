import { commandMetadata } from './command';
import type { CommandMetadata } from './command';
import { canFormat, inspectRecord } from './inspection';
import type { Inspection } from './inspection';
import { ApiError } from '../http';
import { boundedText, object } from '../identity';

export const ROW_LIMIT = 2000;
export const BYTE_LIMIT = 4 * 1024 * 1024;
export const DETAIL_LIMIT = 256 * 1024;
export const filterNames = ['target', 'kind', 'station', 'protocol', 'evse', 'connector', 'transaction', 'action', 'direction', 'severity', 'correlation'] as const;
export type FilterName = typeof filterNames[number];
export type Filters = Partial<Record<FilterName | 'search' | 'from' | 'until', string>>;
export interface Row {
  command: CommandMetadata;
  sequence: number; raw: string; bytes: number; stage: string; time: string; deviceTime: string;
  evidence: string; outcome: string; truncated: boolean; index: Record<FilterName, string>;
}
const encoder = new TextEncoder();
const scalar = (value: unknown) => value === undefined || value === null ? '' : boundedText(value, 512);

export function parseRow(raw: string, process: string): Row {
  const bytes = encoder.encode(raw).length;
  if (bytes > 64 * 1024) throw new ApiError(0, 'limit');
  const record = object(JSON.parse(raw));
  if (object(record.schema_version).major !== 1 || record.process_instance_id !== process) throw new ApiError(0, 'identity');
  const sequence = record.trace_sequence;
  if (!Number.isSafeInteger(sequence) || (sequence as number) < 0) throw new ApiError(0, 'format');
  const details = record.redacted_details == null ? {} : object(record.redacted_details);
  const fields = details.fields == null ? {} : object(details.fields);
  const target = record.target == null ? {} : object(record.target);
  const outcome = scalar(object(record.outcome).status);
  const direction = scalar(record.direction);
  if (!['inbound', 'outbound', 'internal'].includes(direction) || !['succeeded', 'failed', 'uncertain', 'dropped'].includes(outcome)) throw new ApiError(0, 'format');
  const time = boundedText(record.observed_at);
  if (!Number.isFinite(Date.parse(time))) throw new ApiError(0, 'format');
  return {
    command: commandMetadata(record),
    sequence: sequence as number, raw, bytes, stage: boundedText(record.stage), time,
    deviceTime: scalar(fields.source_time), evidence: scalar(fields.evidence), outcome, truncated: details.truncated === true,
    index: {
      target: scalar(target.instance_id), kind: scalar(target.kind), station: scalar(fields.station_id),
      protocol: scalar(fields.protocol), evse: scalar(fields.evse_id), connector: scalar(fields.connector_id),
      transaction: scalar(fields.transaction_id), action: scalar(fields.action), direction,
      severity: scalar(fields.severity), correlation: scalar(record.correlation_id),
    },
  };
}

export function matches(row: Row, filters: Filters): boolean {
  for (const field of filterNames) {
    const value = filters[field];
    if (value && !(row.index[field] || 'unavailable').toLowerCase().includes(value.toLowerCase())) return false;
  }
  if (filters.from && row.time && Date.parse(row.time) < Date.parse(filters.from)) return false;
  if (filters.until && row.time && Date.parse(row.time) > Date.parse(filters.until)) return false;
  return !filters.search || row.raw.toLowerCase().includes(filters.search.toLowerCase());
}

// One retained buffer, including while paused/hidden. React only receives viewport rows.
export class TraceBuffer {
  readonly rows: Row[] = [];
  readonly bookmarks = new Set<number>();
  private readonly details = new Map<number, { text: string; inspection?: Inspection; bytes: number }>();
  bytes = 0;
  detailBytes = 0;
  evicted = 0;
  expiredBookmarks = 0;
  version = 0;
  private lastSequence = -1;

  append(raw: string, process: string, expectedSequence?: number): number {
    const row = parseRow(raw, process);
    if (expectedSequence !== undefined && row.sequence !== expectedSequence) throw new ApiError(0, 'format');
    if (row.sequence <= this.lastSequence) return row.sequence;
    this.lastSequence = row.sequence;
    while (this.rows.length >= ROW_LIMIT || this.bytes + row.bytes > BYTE_LIMIT) this.evict();
    this.rows.push(row); this.bytes += row.bytes; this.version++;
    return row.sequence;
  }

  private evict() {
    const row = this.rows.shift();
    if (!row) return;
    this.bytes -= row.bytes; this.evicted++;
    this.removeDetail(row.sequence);
    if (this.bookmarks.delete(row.sequence)) this.expiredBookmarks++;
  }

  private removeDetail(sequence: number) {
    const cached = this.details.get(sequence);
    if (cached) { this.detailBytes -= cached.bytes; this.details.delete(sequence); }
  }

  inspection(sequence: number): Inspection | undefined {
    this.detail(sequence);
    return this.details.get(sequence)?.inspection;
  }

  detail(sequence: number): string | undefined {
    const cached = this.details.get(sequence);
    if (cached) return cached.text;
    const row = this.rows.find(row => row.sequence === sequence);
    if (!row) return undefined;
    const record: unknown = JSON.parse(row.raw);
    let text: string;
    let inspection: Inspection | undefined;
    try {
      if (!canFormat(record)) throw new Error('detail depth/work limit');
      text = JSON.stringify(record, null, 2);
      inspection = inspectRecord(record);
    } catch {
      // Hostile nesting may exceed the engine's formatting stack. Cache a bounded failure.
      text = 'Detail exceeds safe formatting depth.';
    }
    let bytes = encoder.encode(text).length + encoder.encode(JSON.stringify(inspection) ?? '').length;
    if (bytes > DETAIL_LIMIT) {
      text = 'Detail exceeds display limit.'; inspection = undefined;
      bytes = encoder.encode(text).length;
    }
    while (this.details.size >= 4 || this.detailBytes + bytes > DETAIL_LIMIT) this.removeDetail(this.details.keys().next().value!);
    this.details.set(sequence, { text, inspection, bytes }); this.detailBytes += bytes;
    return text;
  }

  bookmark(sequence: number) {
    if (this.bookmarks.has(sequence)) this.bookmarks.delete(sequence);
    else if (this.bookmarks.size < 64 && this.rows.some(row => row.sequence === sequence)) this.bookmarks.add(sequence);
    this.version++;
  }

  clear() {
    this.rows.length = 0; this.bytes = 0; this.details.clear(); this.detailBytes = 0;
    this.bookmarks.clear(); this.version++;
    // Keep the sequence watermark: clearing must not replay the same retained records.
  }
}
