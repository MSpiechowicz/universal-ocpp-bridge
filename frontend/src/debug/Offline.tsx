import { useEffect, useRef, useState } from 'react';
import { importCapture } from './offline';
import type { OfflineCapture } from './offline';
import { Timeline } from './Timeline';

export function Offline() {
  const [capture, setCapture] = useState<OfflineCapture>();
  const [pending, setPending] = useState(false);
  const [failure, setFailure] = useState('');
  const [revision, setRevision] = useState(0);
  const active = useRef<AbortController | undefined>(undefined);
  const retained = useRef<OfflineCapture | undefined>(undefined);
  useEffect(() => {
    const clear = () => { active.current?.abort(); retained.current?.buffer.clear(); };
    window.addEventListener('pagehide', clear);
    return () => { clear(); window.removeEventListener('pagehide', clear); };
  }, []);
  function clear() {
    active.current?.abort(); active.current = undefined;
    retained.current?.buffer.clear(); retained.current = undefined;
    setCapture(undefined); setPending(false); setFailure(''); setRevision(value => value + 1);
  }
  async function open(file: File) {
    clear();
    const controller = new AbortController(); active.current = controller; setPending(true);
    try {
      const result = await importCapture(file, controller.signal);
      if (controller.signal.aborted) { result.buffer.clear(); return; }
      retained.current = result; setCapture(result);
    } catch {
      // Never echo parser exceptions, filenames, or file content into errors or telemetry.
      if (!controller.signal.aborted) setFailure('Capture rejected: malformed, unsupported, incomplete, inconsistent, or over its byte, record, or detail limits.');
    } finally { if (!controller.signal.aborted) setPending(false); }
  }
  return <div className="console"><header className="topbar">
    <span className="brand">Universal OCPP Bridge · Offline inspector</span><span className="environment">OFFLINE FILE</span>
  </header><main className="offline-panel">
    <h1>Inspect an exported capture</h1>
    <p>No live connection. Imported identities are unverified file provenance. Commands, capture controls, upload, and replay are unavailable.</p>
    <p>Open a version 1.0 JSONL export, up to 9 MiB and 2,000 records. Display retains at most 4 MiB; any browser evictions are shown. Files stay in tab memory.</p>
    <label htmlFor="capture-file">Capture file</label>
    <input id="capture-file" type="file" accept=".jsonl,.ndjson,application/x-ndjson" onChange={event => {
      const file = event.currentTarget.files?.[0]; event.currentTarget.value = ''; if (file) void open(file);
    }}/>
    <button className="secondary" onClick={clear}>Clear offline capture{pending ? ' / cancel import' : ''}</button>
    {pending && <p role="status">Validating capture locally…</p>}
    {failure && <p className="notice error" role="alert">{failure}</p>}
    {capture && <section aria-label="Offline capture">
      <h2>File provenance · {capture.identity.runtime.environment.toUpperCase()} · OFFLINE</h2>
      <dl>
        <div><dt>Bridge / process</dt><dd>{capture.identity.bridge_id} / {capture.identity.runtime.process_instance_id}</dd></div>
        <div><dt>Release / digest</dt><dd>{capture.identity.runtime.release_id} / {capture.identity.runtime.release_digest}</dd></div>
        <div><dt>Build / capture</dt><dd>{capture.build} / {capture.captureId}</dd></div>
        <div><dt>Station / target filter</dt><dd>{capture.station} / {capture.target}</dd></div>
        <div><dt>Selected target</dt><dd>{capture.identity.selected_target_id ?? 'none selected'}</dd></div>
        <div><dt>Capture level</dt><dd>{capture.level} · memory only</dd></div>
        <div><dt>File size / records</dt><dd>{capture.bytes} bytes / {capture.records}</dd></div>
      </dl>
      <p className="notice">History is incomplete. {capture.summary.retained_window_complete ? 'Initial retained window exported completely.' : 'Initial retained window is incomplete.'} Termination: {String(capture.summary.reason)}. No live freshness is implied.</p>
      <p>Initial sequences: {String(capture.window.first_sequence ?? 'empty')} to {String(capture.window.next_sequence)} (exclusive).
        {' '}Prior evictions: {String(capture.window.evicted_records)} · dropped: {String(capture.window.dropped_records)} · details shed: {String(capture.window.shed_records)}.</p>
      <p>Missing sequences: {String(capture.summary.missing_sequences)} · unexported initial records: {String(capture.summary.unexported_initial_records)} · truncated records: {String(capture.summary.truncated_records)}.</p>
      <details><summary>Last observed export window</summary><pre>{JSON.stringify(capture.summary.last_observed_window, null, 2)}</pre></details>
      <Timeline key={revision} buffer={capture.buffer} tick={revision} paused={false} ceiling={Infinity} offline/>
    </section>}
    <p><a href="/">Leave offline inspector and connect to a service</a></p>
  </main></div>;
}
