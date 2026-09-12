// Optional scalar metadata in the centrally redacted trace contract. Never parse embedded
// strings as JSON/HTML or reconstruct payloads from neighboring, potentially unrelated rows.
export const FIELD_LIMIT = 128;
export const VALUE_LIMIT = 4096;
export type Entry = [string, string];
export interface Inspection {
  source: Entry[]; target: Entry[]; canonical: Entry[]; validation: Entry[];
  mappings: Entry[]; measurements: Entry[]; unsupported: Entry[]; sizes: Entry[];
  changes: { path: string; before: string; after: string }[];
  evidence: string; reason: string; trigger: string; correlation: string;
  truncated: boolean; omitted: boolean;
}
const map = (value: unknown): Record<string, unknown> => value !== null && typeof value === 'object' && !Array.isArray(value) ? value as Record<string, unknown> : {};
const text = (value: unknown) => typeof value === 'string' ? value : typeof value === 'number' || typeof value === 'boolean' ? String(value) : '';
const explanations: Record<string, string> = {
  rejected: 'This stage rejected the operation. See the reported reason; no physical effect is established.',
  duplicate: 'This stage identified a duplicate. This record alone does not establish replay or a new effect.',
  stale: 'This stage identified stale evidence. It does not establish a current state change.',
  reconciled: 'This stage reports reconciliation. Only the supplied changed fields are shown.',
  uncertain: 'The outcome remains uncertain; missing evidence does not establish success.',
};
export function inspectRecord(value: unknown): Inspection {
  const record = map(value), details = map(record.redacted_details), fields = map(details.fields);
  const result: Inspection = {
    source: [], target: [], canonical: [], validation: [], mappings: [], measurements: [], unsupported: [], sizes: [], changes: [],
    evidence: text(fields.evidence).slice(0, VALUE_LIMIT), reason: '',
    trigger: text(record.parent_trace_id).slice(0, VALUE_LIMIT), correlation: text(record.correlation_id).slice(0, VALUE_LIMIT),
    truncated: details.truncated === true, omitted: fields.state_details_omitted === 'true',
  };
  let count = 0;
  for (const [key, value] of Object.entries(fields)) {
    if (count++ >= FIELD_LIMIT) { result.omitted = true; break; }
    const scalar = text(value);
    const display = scalar.length > VALUE_LIMIT ? scalar.slice(0, VALUE_LIMIT) + '… [display truncated]' : scalar;
    if (scalar.length > VALUE_LIMIT || key.length > VALUE_LIMIT) result.omitted = true;
    const entry: Entry = [key.slice(0, VALUE_LIMIT), display || '[unsupported non-scalar or empty field]'];
    const change = /^resources\.\d+\.availability$/.test(key) && /^(\w+) -> (\w+)$/.exec(scalar);
    if (change && result.changes.length < 16) result.changes.push({ path: key, before: change[1], after: change[2] });
    else if (/^resources\.\d+\.availability$/.test(key)) { result.unsupported.push(entry); result.omitted = true; }
    else if (key === 'decision.reason' || key === 'reason_code') result.reason = display;
    else if (/^(unit|quality)$|\.(unit|quality)$/.test(key)) result.measurements.push(entry);
    else if (key.startsWith('source.')) result.source.push(entry);
    else if (key.startsWith('target.')) result.target.push(entry);
    else if (key.startsWith('canonical.') || ['station_id', 'evse_id', 'connector_id', 'transaction_id'].includes(key)) result.canonical.push(entry);
    else if (key.startsWith('validation.')) result.validation.push(entry);
    else if (key.startsWith('mapping.')) result.mappings.push(entry);
    else if (key.endsWith('.original_size') || key === 'payload_bytes' || key === 'details.omitted_fields') result.sizes.push(entry);
    else if (key.startsWith('redacted.') || key.startsWith('opaque.') || key.startsWith('unsupported.') || key === 'vendor_payload') result.unsupported.push(entry);
    else if (!['evidence', 'state_details_omitted', 'protocol', 'action', 'source_time', 'correlation', 'correlation_id', 'endpoint_label', 'severity'].includes(key) && !key.startsWith('audit.')) result.unsupported.push(entry);
  }
  for (const key of ['protocol', 'action', 'source_time']) if (fields[key] !== undefined) result.source.push([key, text(fields[key]).slice(0, VALUE_LIMIT)]);
  for (const [key, value] of Object.entries(map(record.target)).slice(0, 8)) result.target.push([key.slice(0, VALUE_LIMIT), text(value).slice(0, VALUE_LIMIT)]);
  if (fields.endpoint_label !== undefined) result.target.push(['endpoint_label', text(fields.endpoint_label).slice(0, VALUE_LIMIT)]);
  return result;
}
export function decisionExplanation(evidence: string): string {
  return Object.hasOwn(explanations, evidence) ? explanations[evidence] : 'Only the reported stage evidence is available; no further decision or physical effect is inferred.';
}

export interface JsonToken { text: string; kind: string }
// Bound syntax node count independently of bytes. The remaining text stays visible and inert.
export function jsonTokens(text: string): JsonToken[] {
  const tokens: JsonToken[] = [];
  const expression = /"(?:[^"\\]|\\.)*"\s*:|"(?:[^"\\]|\\.)*"|\b(?:true|false|null)\b|-?\d+(?:\.\d+)?(?:[eE][+-]?\d+)?/g;
  let start = 0;
  for (const match of text.matchAll(expression)) {
    if (tokens.length >= 1022) break;
    if (match.index > start) tokens.push({ text: text.slice(start, match.index), kind: 'plain' });
    const word = match[0];
    tokens.push({ text: word, kind: word.endsWith(':') ? 'key' : word.startsWith('"') ? 'string' : 'literal' });
    start = match.index + word.length;
  }
  if (start < text.length) tokens.push({ text: text.slice(start), kind: 'plain' });
  return tokens;
}

// Bound formatting depth/work before pretty printing unknown top-level extensions.
export function canFormat(value: unknown, depth = 0, budget = { nodes: 4096 }): boolean {
  if (--budget.nodes < 0 || depth > 32) return false;
  if (value === null || typeof value !== 'object') return true;
  return Object.values(value).every(child => canFormat(child, depth + 1, budget));
}
