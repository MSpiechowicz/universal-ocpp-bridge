import { object } from '../identity';
import type { Row } from './buffer';

export type Metric = number | undefined;
export interface Component {
  state: string; reconnects: Metric; backlog: Metric; inFlight: Metric; connections: Metric;
}
export interface Operations {
  targetKind: string; targetCapabilities: string;
  exportFields: [string, string][];
  readiness: string; storage: string; admission: string;
  queues: { name: string; used: Metric; capacity: Metric }[];
  resources: [string, Metric][];
  target: Component; exporter: Component; broker: Component; externalClient: Component;
}
const map = (value: unknown): Record<string, unknown> => value && typeof value === 'object' && !Array.isArray(value) ? value as Record<string, unknown> : {};
const label = (value: unknown) => typeof value === 'string' && /^[a-zA-Z0-9_.-]{1,128}$/.test(value) ? value : 'Unavailable';
export const metric = (value: unknown): Metric => typeof value === 'number' && Number.isSafeInteger(value) && value >= 0 ? value : undefined;
const state = (value: unknown, allowed: string[]) => typeof value === 'string' && allowed.includes(value) ? value : 'Unavailable';
const queueNames = ['charger_requests', 'database_work', 'subscribers', 'pending_requests', 'multipart_assemblies', 'target_ingress', 'target_egress', 'target_retries', 'critical_reports', 'diagnostics', 'exporter_batches', 'capture_records'];
function component(value: unknown): Component {
  const item = map(value);
  return {
    state: state(item.state, ['disabled', 'ready', 'degraded', 'reconnecting', 'stopped']),
    reconnects: metric(item.reconnects), backlog: metric(item.backlog_items),
    inFlight: metric(item.in_flight_items), connections: metric(item.active_connections),
  };
}
// Project only fixed enums and nonnegative exact counters. Never display arbitrary health
// reasons, extension strings, connection URLs, configuration blobs, or credentials.
export function parseOperations(value: unknown): Operations {
  const data = object(value);
  if (!['ready', 'not_ready'].includes(String(data.readiness))) throw new Error('Invalid health snapshot');
  const runtime = map(data.runtime), limits = map(data.runtime_limits);
  const queues = map(runtime.queues), capacities = map(limits.queues);
  const storage = map(data.storage_retention), latency = map(data.storage_latency), process = map(data.daemon_process);
  const components = map(data.components), exporter = map(data.export_observation);
  const target = map(data.target_configuration);
  const classes = ['measurement', 'transaction_lifecycle', 'resource_status_change', 'point_change', 'command_result'];
  return {
    targetKind: label(target.kind),
    targetCapabilities: Array.isArray(target.capabilities) && target.capabilities.length <= 32 ? target.capabilities.map(label).join(', ') || 'None declared' : 'Unavailable',
    exportFields: [
      ['Provider', label(exporter.provider)], ['Destination / revision label', label(exporter.destination_revision)],
      ['Selected record classes', Array.isArray(exporter.record_classes) && exporter.record_classes.length <= 5 && exporter.record_classes.every(value => classes.includes(value)) ? exporter.record_classes.join(', ') || 'None selected' : 'Unavailable'],
      ['Observation age (ms at snapshot)', displayMetric(metric(exporter.age_ms)) + ((metric(exporter.age_ms) ?? 0) > 30000 ? ' · stale' : '')],
      ...([
        ['Local export enqueue', 'enqueued_records'], ['Remote committed records', 'remote_committed_records'],
        ['Successful batches', 'successful_batches'], ['Failed batches', 'failed_batches'], ['Retries', 'retries'],
        ['Export lag (ms)', 'lag_milliseconds'], ['Duplicates handled', 'duplicates'],
        ['Quarantined records', 'quarantined_records'], ['Export data gaps', 'gap_count'], ['Dropped export records', 'dropped_records'],
      ] as [string, string][]).map(([name, key]): [string, string] => [name, displayMetric(metric(exporter[key]))]),
    ],
    readiness: state(data.readiness, ['ready', 'not_ready']),
    storage: state(data.storage, ['starting', 'safe', 'capacity_protected', 'failed', 'maintenance']),
    admission: data.accepts_new_sessions === true ? 'Allowed' : data.accepts_new_sessions === false ? 'Refused' : 'Unavailable',
    queues: [
      { name: 'Connected stations', used: metric(runtime.connected_stations), capacity: metric(limits.maximum_connected_stations) },
      { name: 'Queued payload bytes', used: metric(runtime.queued_payload_bytes), capacity: metric(limits.aggregate_queued_payload_bytes) },
      { name: 'Trace ring bytes', used: metric(runtime.trace_ring_bytes), capacity: metric(limits.trace_ring_bytes) },
      { name: 'Operational storage bytes (logical)', used: metric(storage.used_bytes), capacity: metric(storage.budget_bytes) },
      ...queueNames.map(name => ({ name: name.replaceAll('_', ' '), used: metric(queues[name]), capacity: metric(capacities[name]) })),
    ],
    resources: [
      ['Protected storage reserve bytes', metric(storage.active_session_reserve_bytes)],
      ['Required delivery backlog', metric(storage.retained_required_deliveries)],
      ['Retained critical events', metric(storage.retained_critical_events)],
      ['Dropped telemetry', metric(data.dropped_telemetry)],
      ['Dropped diagnostics', metric(runtime.dropped_diagnostics)],
      ['Storage telemetry shed', metric(storage.dropped_best_effort_telemetry)],
      ['Storage deliveries shed', metric(storage.dropped_best_effort_deliveries)],
      ['Uncertain commands', metric(data.uncertain_commands)],
      ['Database operation samples', metric(latency.samples)],
      ['Database latency p95 upper bound (ms)', metric(latency.samples) ? metric(latency.p95_upper_bound_ms) : undefined],
      ['Database latency maximum (ms)', metric(latency.samples) ? metric(latency.maximum_ms) : undefined],
      ['Database physical file size (bytes)', undefined],
      ['Daemon RSS (bytes)', metric(process.rss_bytes)],
      ['Daemon cumulative CPU time (ms)', metric(process.cpu_time_milliseconds)],
    ],
    target: component(components.target), exporter: component(components.external_exporter),
    broker: component(components.external_broker), externalClient: component(components.external_client),
  };
}
export const displayMetric = (value: Metric) => value === undefined ? 'Unavailable' : String(value);
export function latestCommit(rows: readonly Row[], ceiling: number): Row | undefined {
  for (let index = rows.length - 1; index >= 0; index--) {
    const row = rows[index];
    if (row.sequence <= ceiling && row.stage === 'storage.commit') return row;
  }
}

export function stationObservations(rows: readonly Row[], ceiling: number) {
  const stations = new Map<string, { station: string; protocol: string; heartbeat: string }>();
  for (let index = rows.length - 1; index >= 0; index--) {
    const row = rows[index], station = row.index.station;
    if (row.sequence > ceiling || !station || !row.stage.startsWith('ocpp.')) continue;
    if (!stations.has(station) && stations.size < 10) stations.set(station, { station, protocol: '', heartbeat: '' });
    const entry = stations.get(station);
    if (!entry) continue;
    entry.protocol ||= row.index.protocol;
    if (row.stage === 'ocpp.receive' && row.index.action === 'Heartbeat') entry.heartbeat ||= row.time;
  }
  return [...stations.values()];
}
