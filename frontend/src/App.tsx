import { DiagnosticsPanel } from './diagnostics/Panel';
import { diagnostics } from './diagnostics/store';
import { useEffect, useRef, useState } from 'react';
import type { FormEvent } from 'react';
import { ApiClient, ApiError } from './http';
import { Debug } from './debug/Debug';
import type { Identity } from './identity';
import { subscribe } from './events';
import type { ConnectionState } from './events';

export function App() {
  const [identity, setIdentity] = useState<Identity>();
  const [failure, setFailure] = useState('');
  const [client, setClient] = useState<ApiClient>();
  const [pending, setPending] = useState(false);
  const [station, setStation] = useState('');
  const [page, setPage] = useState<{ count: number; more: boolean }>();
  const [connection, setConnection] = useState<ConnectionState>();
  const [hidden, setHidden] = useState(document.hidden);
  const credential = useRef<HTMLInputElement>(null);
  const active = useRef<ApiClient | undefined>(undefined);
  const operation = useRef(0);
  useEffect(() => { diagnostics.commit('app'); });

  useEffect(() => {
    const controller = new AbortController();
    void ApiClient.identify(location.origin, controller.signal).then(setIdentity).catch(() => {
      if (!controller.signal.aborted) setFailure('Cannot verify service identity. Check the management connection and reload.');
    });
    const visibility = () => setHidden(document.hidden);
    const leave = () => { operation.current++; active.current?.close(); };
    document.addEventListener('visibilitychange', visibility);
    const restore = (event: PageTransitionEvent) => { if (event.persisted) location.reload(); };
    window.addEventListener('pagehide', leave);
    window.addEventListener('pageshow', restore);
    return () => { controller.abort(); leave(); document.removeEventListener('visibilitychange', visibility); window.removeEventListener('pagehide', leave); window.removeEventListener('pageshow', restore); };
  }, []);

  useEffect(() => {
    if (!client) return;
    const stop = subscribe(client, station, setConnection);
    return () => { stop(); client.close(); };
  }, [client, station]);

  async function connect(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    if (!identity || pending || client) return;
    const token = credential.current?.value ?? '';
    if (credential.current) credential.current.value = '';
    setFailure(''); setPending(true);
    const generation = ++operation.current;
    let next: ApiClient | undefined;
    try {
      next = new ApiClient(location.origin, identity, token);
      active.current = next;
      const inventory = await next.stations();
      if (generation !== operation.current) { next.close(); return; }
      setPage(inventory); setClient(next);
    } catch (error) {
      next?.close();
      if (generation === operation.current) setFailure(error instanceof ApiError ? error.message : 'Connection failed. No credentials were saved.');
    } finally { if (generation === operation.current) setPending(false); }
  }

  function disconnect() {
    diagnostics.clear();
    operation.current++; active.current?.close(); active.current = undefined;
    setClient(undefined); setConnection(undefined); setPage(undefined); setPending(false);
    setFailure('');
    // A new connection requires a fresh service identity and an explicitly entered credential.
    setIdentity(undefined);
    void ApiClient.identify(location.origin).then(setIdentity).catch(() => setFailure('Cannot verify service identity. Reload to retry.'));
  }

  const state = hidden && client ? 'stale' : connection?.status ?? (pending ? 'connecting' : 'disconnected');
  return <div className="console">
    <header className="topbar">
      <a href="/" className="brand"><span className="brand-mark" aria-hidden="true">U</span><span>Universal OCPP Bridge<small>Management console</small></span></a>
      <span className={`environment ${identity?.runtime.environment ?? ''}`}>{identity?.runtime.environment ?? 'Identity unverified'}</span>
    </header>
    <div className="layout">
      <aside aria-label="Console navigation">
        <p className="section-label">Workspace</p>
        <a className="nav-active" href="#connection" aria-current="page">Connection</a>
        <a href="#debug" className="debug-nav">Debug timeline</a>
        <a href="/?offline=1">Offline capture inspector</a>
        <div className="sidebar-note">Local management<br/><span>HTTP / JSON + SSE</span></div>
      </aside>
      <main id="connection">
        <div className="page-heading"><div><p className="section-label">Bridge console</p><h1>Service connection</h1></div><span className={`status ${state}`} role="status">{state}</span></div>
        <p className="intro">Verify the destination, then connect with a scoped management credential.</p>
        <section className="identity-panel" aria-labelledby="identity-heading">
          <h2 id="identity-heading">Destination</h2>
          <dl>
            <div><dt>Bridge</dt><dd>{identity?.bridge_id ?? 'Verifying…'}</dd></div>
            <div><dt>Target</dt><dd>{identity?.selected_target_id ?? 'none selected'}</dd></div>
            <div><dt>Release</dt><dd>{identity?.runtime.release_id ?? '—'}</dd></div>
            <div><dt>Origin</dt><dd>{location.origin}</dd></div>
            <div><dt>Process</dt><dd>{identity?.runtime.process_instance_id ?? '—'}</dd></div>
          </dl>
        </section>
        <section className="connection-panel" aria-labelledby="access-heading">
          <div><p className="section-label">Access</p><h2 id="access-heading">Connect to this bridge</h2><p>Credentials stay in this tab’s memory. Reloading or disconnecting clears the connection.</p></div>
          <form onSubmit={connect} autoComplete="off">
            <label htmlFor="credential">Management read credential</label>
            <input ref={credential} id="credential" name="management-credential" type="password" autoComplete="off" maxLength={8000} required disabled={!!client || pending || !identity} spellCheck={false}/>
            <label htmlFor="station">Station scope <span>(optional)</span></label>
            <input id="station" value={station} onChange={event => setStation(event.target.value)} maxLength={256} disabled={!!client || pending} placeholder="Use the credential’s default station" autoComplete="off"/>
            <p className="field-note">Permissions are checked by the service. This screen opens no diagnostic capture.</p>
            {client || pending ? <button type="button" className="secondary" onClick={disconnect}>Disconnect and clear credential</button> : <button disabled={!identity} type="submit">Connect to {identity?.runtime.environment ?? 'bridge'}</button>}
          </form>
        </section>
        {failure && <p className="notice error" role="alert">{failure}</p>}
        {client && <section className="stream-panel" aria-labelledby="stream-heading">
          <h2 id="stream-heading">Event connection</h2>
          <p className="notice" role="status">{hidden ? 'Tab is hidden. The display may be stale; server processing continues.' : connection?.message ?? 'Opening authenticated event stream…'}</p>
          <dl>
            <div><dt>Inventory query</dt><dd>{page?.count ?? 0} visible in first page{page?.more ? ' · more available' : ''}</dd></div>
            <div><dt>Events received</dt><dd>{connection?.received ?? 0}</dd></div>
            <div><dt>Reconnect attempts</dt><dd>{connection?.attempts ?? 0}</dd></div>
            <div><dt>History gaps</dt><dd>{connection?.gaps ?? 0}</dd></div>
            <div><dt>Latest event type</dt><dd>{connection?.lastEvent ?? 'No event received'}</dd></div>
            <div><dt>Last stream activity</dt><dd>{connection?.lastActivity ? new Date(connection.lastActivity).toLocaleTimeString() : 'Waiting'}</dd></div>
          </dl>
          <p className="field-note">Stream activity confirms connectivity. It does not confirm a charger action or refresh the inventory query.</p>
        </section>}
        <DiagnosticsPanel connection={connection} hidden={hidden}/>
        {identity && <Debug key={JSON.stringify(identity)} identity={identity} hidden={hidden}/>}
        <footer>Independent management interface <span>Bound to the destination shown above</span></footer>
      </main>
    </div>
  </div>;
}
