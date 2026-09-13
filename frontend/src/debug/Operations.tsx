import { useEffect, useState } from 'react';
import { ApiClient, ApiError } from '../http';
import type { Identity } from '../identity';
import { correlationId } from '../diagnostics/store';
import type { TraceBuffer } from './buffer';
import { displayMetric, latestCommit, parseOperations, stationObservations } from './operations';
import type { Component, Operations as Snapshot } from './operations';

export function Operations({ identity, hidden, buffer, ceiling }: {
  identity: Identity; hidden: boolean; buffer: TraceBuffer; ceiling: number;
}) {
  const [open, setOpen] = useState(false);
  const [snapshot, setSnapshot] = useState<{ data: Snapshot; received: number }>();
  const [failure, setFailure] = useState(false);
  const [refresh, setRefresh] = useState(0);
  useEffect(() => {
    if (!open || hidden) return;
    const controller = new AbortController();
    let timer: ReturnType<typeof setTimeout>;
    async function read() {
      try {
        const data = parseOperations(await ApiClient.healthSnapshot(location.origin, identity, controller.signal));
        if (!controller.signal.aborted) { setSnapshot({ data, received: Date.now() }); setFailure(false); }
      } catch (error) {
        if (!controller.signal.aborted) setFailure(true);
        if (error instanceof ApiError && error.kind === 'identity') { setSnapshot(undefined); controller.abort(); }
      } finally {
        // One outstanding read, with a delay after completion; no catch-up work.
        if (!controller.signal.aborted) timer = setTimeout(() => void read(), 10000);
      }
    }
    void read();
    return () => { controller.abort(); clearTimeout(timer); };
  }, [identity, open, hidden, refresh]);
  // Authentication changes remount this panel in Debug, clearing retained observations.
  const data = snapshot?.data;
  const commit = latestCommit(buffer.rows, ceiling);
  const stations = stationObservations(buffer.rows, ceiling);
  const correlation = correlationId(commit?.index.correlation);
  return <section className="operations" aria-label="Connections, resources and export">
    <button className="secondary" aria-expanded={open} onClick={() => setOpen(value => !value)}>{open ? 'Hide' : 'Show'} connections, resources and export</button>
    {open && <>
      <h3>Connections and resources</h3>
      <p>Passive health snapshot; no capture or exporter work.</p>
      <button className="secondary" onClick={() => setRefresh(value => value + 1)} disabled={hidden}>Refresh operational snapshot</button>
      <p role="status">{failure ? 'Refresh failed; retained snapshot is stale.' : hidden ? 'Hidden tab: polling paused; retained snapshot is stale.' : snapshot ? 'Last health snapshot received' : 'Snapshot unavailable.'}
        {snapshot && ` · ${new Date(snapshot.received).toLocaleTimeString()}`}</p>
      <p className="field-note">Receipt time is not metric observation time. Ages are unavailable unless reported. Missing is not zero.</p>
      <dl><dt>Selected target identity</dt><dd>{identity.selected_target_id ?? 'None selected'}</dd>
        <dt>Target kind / declared capabilities</dt><dd>{data?.targetKind ?? 'Unavailable'} · {data?.targetCapabilities ?? 'Unavailable'}</dd>

      </dl>
      <h4>Captured OCPP connections (up to 10)</h4>
      <p>Historical authorized capture evidence. Reconnect counts and in-flight calls per station are unavailable.</p>
      {stations.length ? <dl>{stations.map(row => <div key={row.station}><dt>{row.station}</dt><dd>{row.protocol || 'Protocol unavailable'} · last captured heartbeat: {row.heartbeat || 'Unavailable'}</dd></div>)}</dl> : <p>No retained OCPP connection evidence.</p>}
      <div className="operations-grid">
        <ComponentView label="Selected target health" value={data?.target}/>
        <ComponentView label="Broker health" value={data?.broker}/>
        <ComponentView label="External client health" value={data?.externalClient}/>
      </div>
      <h3>Local persistence</h3>
      <dl><dt>Core readiness</dt><dd>{data?.readiness ?? 'Unavailable'}</dd>
        <dt>Local storage safety</dt><dd>{data?.storage ?? 'Unavailable'}</dd>
        <dt>New session admission</dt><dd>{data?.admission ?? 'Unavailable'}</dd>
        <dt>Latest retained local commit evidence</dt><dd>{commit ? `${commit.outcome} · ${commit.time} · trace ${commit.sequence}` : 'Unavailable — no storage.commit trace in the retained capture window'}
          {correlation && <button className="secondary" onClick={() => window.dispatchEvent(new CustomEvent('uob-correlation', { detail: correlation }))}>Open local commit correlation</button>}
        </dd>
      </dl>
      <p>Local commits survive export outages. Capture may be incomplete.</p>
      <h3>Resource use and capacity</h3>
      {data ? <><table><thead><tr><th scope="col">Resource / queue</th><th scope="col">Used</th><th scope="col">Capacity</th></tr></thead><tbody>
        {data.queues.map(row => <tr key={row.name}><th scope="row">{row.name}</th><td>{displayMetric(row.used)}</td><td>{displayMetric(row.capacity)}</td></tr>)}
      </tbody></table><dl className="operations-metrics">{data.resources.map(([name, value]) => <div key={name}><dt>{name}</dt><dd>{displayMetric(value)}</dd></div>)}</dl></> : <p>Metrics unavailable until a snapshot is read.</p>}
      <section aria-label="External database export"><h3>External database export</h3>
        <ComponentView label="Exporter connectivity" value={data?.exporter}/>
        {data?.exporter.state === 'disabled' && <p>Exporter disabled. Reading this snapshot creates no provider connection, batch, or retry work.</p>}
        {['degraded', 'reconnecting', 'stopped'].includes(data?.exporter.state ?? '') && <p className="notice error">Remote export is degraded. Local persistence is shown independently above.</p>}
        <dl>{data?.exportFields.map(([label, value]) => <div key={label}><dt>{label}</dt><dd>{value}</dd></div>)}</dl>
        <p>Connectivity is not commit evidence; gaps may be unknown.</p>
      </section>
    </>}
  </section>;
}
function ComponentView({ label, value }: { label: string; value?: Component }) {
  return <section aria-label={label}><h4>{label}</h4><dl>
    <dt>State</dt><dd>{value?.state ?? 'Unavailable'}</dd>
    <dt>Reconnects</dt><dd>{displayMetric(value?.reconnects)}</dd>
    <dt>Backlog items</dt><dd>{displayMetric(value?.backlog)}</dd>
    <dt>In-flight items</dt><dd>{displayMetric(value?.inFlight)}</dd>
    <dt>Active connections</dt><dd>{displayMetric(value?.connections)}</dd>
  </dl></section>;
}
