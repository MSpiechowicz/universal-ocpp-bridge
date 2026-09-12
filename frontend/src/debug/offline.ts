import { boundedText, object, parseIdentity } from '../identity';
import type { Identity } from '../identity';
import { TraceBuffer, ROW_LIMIT, parseRow } from './buffer';
import { validateCaptureLine } from './schema';

export const FILE_LIMIT = 9 * 1024 * 1024;
export const LINE_LIMIT = 64 * 1024 + 32;
const METADATA_LIMIT = 16 * 1024;
export interface OfflineCapture {
  identity: Identity; build: string; captureId: number; station: string; target: string;
  level: string; window: Record<string, unknown>; summary: Record<string, unknown>;
  buffer: TraceBuffer; bytes: number; records: number;
}
const count = (value: unknown): number => {
  if (!Number.isSafeInteger(value) || (value as number) < 0) throw new Error('Invalid capture counter.');
  return value as number;
};
function windowValue(value: unknown) {
  const window = object(value);
  for (const [key, number] of Object.entries(window)) if (key !== 'first_sequence' || number !== null) count(number);
  if (count(window.retained_records) > ROW_LIMIT || count(window.retained_bytes) > 8 * 1024 * 1024) throw new Error('Capture window exceeds limits.');
  if ((window.first_sequence === null) !== (window.retained_records === 0)
    || (window.first_sequence !== null && count(window.first_sequence) >= count(window.next_sequence))) throw new Error('Invalid capture window.');
  return window;
}

// Reject excessive nesting before JSON.parse, independently of the encoded byte ceiling.
function parse(text: string): Record<string, unknown> {
  let depth = 0, quoted = false, escaped = false;
  for (const char of text) {
    if (quoted) {
      if (escaped) escaped = false;
      else if (char === '\\') escaped = true;
      else if (char === '"') quoted = false;
    } else if (char === '"') quoted = true;
    else if (char === '{' || char === '[') { if (++depth > 32) throw new Error('Capture nesting exceeds limits.'); }
    else if (char === '}' || char === ']') depth--;
  }
  const value = object(JSON.parse(text));
  validateCaptureLine(value);
  return value;
}

// Only Blob reads: no fetch, credential, service identity lookup, storage, or replay port.
// One <=32 KiB slice and one <=64 KiB line are held outside the shared 4 MiB display ring.
export async function importCapture(file: Blob, signal?: AbortSignal): Promise<OfflineCapture> {
  if (!Number.isSafeInteger(file.size) || file.size < 1 || file.size > FILE_LIMIT) throw new Error('Capture file exceeds 9 MiB or is empty.');
  const buffer = new TraceBuffer();
  const line = new Uint8Array(LINE_LIMIT);
  const decoder = new TextDecoder('utf-8', { fatal: true });
  let length = 0, consumed = 0, records = 0, truncated = 0, gaps = 0;
  let last: number | null = null;
  let capture: OfflineCapture | undefined;
  let limits: Record<string, unknown> = {};
  let finished = false;
  try {
    for (let offset = 0; offset < file.size; offset += 32 * 1024) {
      signal?.throwIfAborted();
      const chunk = new Uint8Array(await file.slice(offset, offset + 32 * 1024).arrayBuffer());
      signal?.throwIfAborted();
      for (const byte of chunk) {
        consumed++;
        if (byte !== 10) {
          if (length >= LINE_LIMIT) throw new Error('Capture line exceeds limits.');
          line[length++] = byte;
          continue;
        }
        if (finished || length === 0) throw new Error('Unexpected capture line.');
        const lineBytes = length + 1;
        const value = parse(decoder.decode(line.subarray(0, length)));
        length = 0;
        if (value.type !== 'trace' && lineBytes > METADATA_LIMIT) throw new Error('Capture metadata exceeds limits.');
        if (!capture) {
          if (value.type !== 'manifest') throw new Error('Capture manifest is missing.');
          limits = object(value.limits);
          if (file.size > count(limits.bytes)) throw new Error('Capture exceeds its declared byte limit.');
          capture = {
            identity: parseIdentity(value.identity), build: boundedText(value.build_version), captureId: count(value.capture_id),
            station: object(value.filters).station_id == null ? 'all authorized' : boundedText(object(value.filters).station_id),
            target: object(value.filters).target_id == null ? 'all authorized' : boundedText(object(value.filters).target_id),
            level: boundedText(object(value.configuration).capture_level), window: windowValue(value.window),
            summary: {}, buffer, bytes: file.size, records: 0,
          };
        } else if (value.type === 'trace') {
          if (++records > ROW_LIMIT || records > count(limits.records) || records > count(capture.window.retained_records)) throw new Error('Capture record count exceeds limits.');
          const record = object(value.record);
          const version = object(record.schema_version);
          if (version.major !== 1 || version.revision !== 0) throw new Error('Unsupported trace schema version.');
          const raw = JSON.stringify(record);
          const row = parseRow(raw, capture.identity.runtime.process_instance_id);
          if (row.sequence < count(capture.window.first_sequence) || row.sequence >= count(capture.window.next_sequence)
            || (last !== null && row.sequence <= last)) throw new Error('Invalid capture sequence or process identity.');
          gaps += row.sequence - (last === null ? count(capture.window.first_sequence) : last + 1);
          last = row.sequence;
          if (row.truncated) truncated++;
          buffer.append(raw, capture.identity.runtime.process_instance_id);
        } else if (value.type === 'summary') {
          if (count(value.exported_records) !== records || value.last_sequence !== last
            || count(value.bytes_before_summary) !== consumed - lineBytes
            || count(value.truncated_records) !== truncated || count(value.missing_sequences) !== gaps
            || count(value.unexported_initial_records) !== count(capture.window.retained_records) - records
            || value.retained_window_complete !== (value.reason === 'window_end' && records === capture.window.retained_records)) throw new Error('Capture summary does not match its records.');
          windowValue(value.last_observed_window);
          capture.summary = value; capture.records = records; finished = true;
        } else throw new Error('Unexpected capture manifest.');
      }
    }
    if (length || !capture || !finished) throw new Error('Capture download is incomplete; manifest, summary, and final newline are required.');
    return capture;
  } catch (error) { buffer.clear(); throw error; }
}
