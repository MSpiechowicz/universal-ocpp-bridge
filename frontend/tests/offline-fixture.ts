export const identity = { bridge_id: 'offline-bridge', runtime: { environment: 'staging', release_id: 'release-offline', release_digest: 'sha256:offline', process_instance_id: 'offline-process' } };
export const trace = (sequence: number, payload = '<img src=x onerror=alert(1)>') => ({
  schema_version: { major: 1, revision: 0 }, trace_id: `trace-${sequence}`, process_instance_id: 'offline-process',
  trace_sequence: sequence, stage: 'storage.commit', direction: 'internal', observed_at: '2026-09-12T12:00:00Z',
  correlation_id: 'offline-correlation', outcome: { status: 'succeeded' },
  redacted_details: { truncated: true, fields: { evidence: 'completed', station_id: 'offline-station', 'source.payload': payload } },
});
export function captureLines(records = [trace(2), trace(4)]) {
  const window = { first_sequence: records[0]?.trace_sequence ?? null, next_sequence: (records.at(-1)?.trace_sequence ?? -1) + 1,
    retained_records: records.length, retained_bytes: 1024, evicted_records: 2, dropped_records: 3, shed_records: 4 };
  const manifest = { type: 'manifest', schema_version: '1.0', trace_schema_version: { major: 1, revision: 0 },
    build_version: '0.1.0', identity, capture_id: 1, filters: { station_id: 'offline-station', target_id: null },
    configuration: { capture_level: 'redacted_payload', persistence: 'memory_only' }, window,
    limits: { bytes: 9 * 1024 * 1024, records: 2000, lifetime_ms: 30000, idle_ms: 5000 }, replay: 'best_effort', history_complete: false };
  const before = [manifest, ...records.map(record => ({ type: 'trace', record }))];
  const bytes = new TextEncoder().encode(before.map(line => JSON.stringify(line) + '\n').join('')).length;
  const summary = { type: 'summary', schema_version: '1.0', reason: 'window_end', history_complete: false,
    retained_window_complete: true, exported_records: records.length, bytes_before_summary: bytes,
    last_sequence: records.at(-1)?.trace_sequence ?? null, unexported_initial_records: 0,
    missing_sequences: records.length ? records.at(-1)!.trace_sequence - records[0].trace_sequence + 1 - records.length : 0,
    truncated_records: records.filter(record => record.redacted_details.truncated).length, last_observed_window: window };
  return { manifest, records, summary };
}
export function encodeCapture(fixture = captureLines()) {
  const prefix = [fixture.manifest, ...fixture.records.map(record => ({ type: 'trace', record }))].map(line => JSON.stringify(line) + '\n').join('');
  fixture.summary.bytes_before_summary = new TextEncoder().encode(prefix).length;
  return prefix + JSON.stringify(fixture.summary) + '\n';
}
