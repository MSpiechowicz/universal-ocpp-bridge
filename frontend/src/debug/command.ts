import type { Row } from './buffer';

const object = (value: unknown): Record<string, unknown> => value !== null && typeof value === 'object' && !Array.isArray(value) ? value as Record<string, unknown> : {};
const text = (value: unknown, limit = 512) => typeof value === 'string' && value.length <= limit ? value : '';
export interface CommandMetadata {
  process: string; correlation: string; request: string; origin: string; reason: string;
  event: string; duration: string; partial: boolean;
}
export function commandMetadata(record: Record<string, unknown>): CommandMetadata {
  const details = object(record.redacted_details), fields = object(details.fields);
  return {
    process: text(record.process_instance_id), correlation: text(record.correlation_id, 1024),
    request: text(fields['command.request_id'], 256), origin: text(fields['command.origin'], 1024),
    reason: text(fields.reason_code) || text(fields['decision.reason']) || text(object(record.outcome).reason),
    event: text(fields['command.event_id'], 256),
    duration: Number.isSafeInteger(record.duration_micros) && (record.duration_micros as number) >= 0 ? String(record.duration_micros) : '',
    partial: details.truncated === true || fields.state_details_omitted === 'true',
  };
}
export const stages = [
  ['command.authorization', 'Authorization'], ['command.ingress', 'Command admission'],
  ['validation', 'Command validation'], ['application', 'Pre-dispatch decision'],
  ['command.dispatch', 'Dispatch'], ['command.protocol_response', 'Charger response'],
  ['command.observed_effect', 'Observed charging effect'], ['management.delivery', 'API exposure'],
  ['target.report', 'Target delivery report'], ['command.deduplication', 'Retry / duplicate decision'],
] as const;

// A trace outcome is successful only at its named stage. Never advance a progress ladder.
export function commandEvidence(row: Row): string {
  const key = `${row.stage}:${row.evidence}`;
  const known: Record<string, string> = {
    'command.authorization:completed': 'Authorized by the shared access policy',
    'command.authorization:rejected': 'Authorization rejected',
    'command.ingress:completed': 'Coordinator received the request; durable admission is not established here',
    'command.dispatch:completed': 'Station dispatch invoked; transmission and acceptance are not established here',
    'command.protocol_response:charger_accepted': 'Charger accepted the protocol operation; physical effect remains separate',
    'command.protocol_response:rejected': 'Charger rejected the protocol operation',
    'command.protocol_response:not_transmitted': 'Proven not transmitted; command rejected',
    'application:not_transmitted': 'Command not transmitted; see safe reason',
    'command.protocol_response:uncertain': 'Transmission uncertain; no authoritative response. Automatic replay is unsafe',
    'command.observed_effect:observed_effect': 'Later observed effect explicitly linked and persisted',
    'management.delivery:locally_exposed': 'Result exposed through the API; HTTP status and client consumption are not established here',
    'target.report:locally_exposed': 'Locally exposed to the target; downstream consumption is not established',
    'target.report:peer_acknowledged': 'Peer acknowledged delivery; charger acceptance and physical effect remain separate',
    'target.report:uncertain': 'Target delivery uncertain',
    'target.report:failed': 'Target delivery failed',
    'command.deduplication:duplicate': 'Duplicate request detected; this trace does not prove another dispatch',
  };
  return Object.hasOwn(known, key) &&
    (row.outcome === 'succeeded' || ['failed', 'uncertain'].includes(row.outcome) &&
      ['rejected', 'not_transmitted', 'uncertain', 'failed'].includes(row.evidence))
    ? known[key] : `Unclassified stage evidence (${row.evidence || 'unavailable'}; ${row.outcome}); no further outcome inferred`;
}
export const COMMAND_LINK_LIMIT = 64;
export function commandTrace(rows: readonly Row[], selected: number, ceiling = Infinity) {
  const anchor = rows.find(row => row.sequence === selected && row.sequence <= ceiling);
  if (!anchor) return undefined;
  const sameContext = (row: Row) => row.sequence <= ceiling && row.command.process === anchor.command.process && row.index.station === anchor.index.station;
  // Explicit request IDs join command evidence. Correlation-only links never prove another
  // request's command outcome, and missing/truncated identities never join unrelated rows.
  const evidence = rows.filter(row => sameContext(row) && (row.sequence === selected ||
    !!anchor.command.request && row.command.request === anchor.command.request));
  const evidenceSequences = new Set(evidence.map(row => row.sequence));
  const related = rows.filter(row => sameContext(row) && !!anchor.command.correlation &&
    row.command.correlation === anchor.command.correlation && !evidenceSequences.has(row.sequence));
  return { anchor, evidence, related: related.slice(0, COMMAND_LINK_LIMIT), omitted: Math.max(0, related.length - COMMAND_LINK_LIMIT) };
}
