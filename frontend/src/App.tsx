import { DiagnosticsPanel } from './diagnostics/Panel';
import { diagnostics } from './diagnostics/store';
import { useEffect, useRef, useState } from 'react';
import type { FormEvent } from 'react';
import { ApiClient, ApiError } from './http';
import { Debug } from './debug/Debug';
import type { Identity } from './identity';
import { subscribe } from './events';
import type { ConnectionState } from './events';
import { Fields } from './Fields';
import { Stations } from './stations/Stations';
import { Commands } from './commands/Commands';
import { StationStore } from './stations/store';
import type { InventoryState } from './stations/store';

export function App() {
  const [identity, setIdentity] = useState<Identity>();
  const [failure, setFailure] = useState('');
  const [client, setClient] = useState<ApiClient>();
  const [pending, setPending] = useState(false);
  const [station, setStation] = useState('');
  const [inventory, setInventory] = useState<InventoryState>();
  const [store, setStore] = useState<StationStore>();
  const [connection, setConnection] = useState<ConnectionState>();
  const [hidden, setHidden] = useState(document.hidden);
  const [stationContext, setStationContext] = useState('');
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
    if (!client || !store) return;
    return () => { store.close(); client.close(); };
  }, [client, store]);

  useEffect(() => {
    if (!client || !store) return;
    // A selection starts a new single-station subscription with its own cursor.
    // The previous subscription is stopped by effect cleanup without closing the read credential.
    store.stream(false);
    setConnection(undefined);
    const selected = store.state.selected;
    if (!selected) return;
    let disposed = false;
    const unsubscribe = subscribe(client, selected, value => { if (!disposed) setConnection(value); }, {
      stale: () => store.stream(false),
      live: () => store.stream(true),
      event: () => store.refresh(),
      recovered: snapshot => store.recovered(snapshot),
    });
    return () => { disposed = true; unsubscribe(); };
  }, [client, store, inventory?.selected]);
  useEffect(() => {
    const selectContext = (event: Event) => {
      const id: unknown = (event as CustomEvent<unknown>).detail;
      if (typeof id !== 'string' || !id || id.length > 64) return;
      for (let index = 0; index < id.length; index++) {
        const code = id.charCodeAt(index);
        if (code <= 31 || code === 127) return;
      }
      if (!store || !inventory?.page?.items.some(item => item.station.station_id === id) || station && station !== id) {
        setStationContext(`Station ${id} is not available in this connection's authorized, loaded inventory. Connect or load the relevant inventory page; the simulator step is not correlated to bridge state.`);
        return;
      }
      store.select(id);
      setStationContext(`Station ${id} selected in normal inventory for context only. Its observations do not prove this simulator step.`);
    };
    window.addEventListener('uob-simulator-station', selectContext);
    return () => window.removeEventListener('uob-simulator-station', selectContext);
  }, [store, inventory?.page, station]);

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
      const page = await next.stations();
      if (generation !== operation.current) { next.close(); return; }
      const controller = new StationStore(next, setInventory);
      controller.initialize(page);
      if (station || page.items[0]) controller.select(station || page.items[0].station.station_id);
      setStore(controller); setClient(next);
    } catch (error) {
      next?.close();
      if (generation === operation.current) setFailure(error instanceof ApiError ? error.message : 'Connection failed. No credentials were saved.');
    } finally { if (generation === operation.current) setPending(false); }
  }

  function disconnect() {
    diagnostics.clear();
    operation.current++; store?.close(); active.current?.close(); active.current = undefined;
    setClient(undefined); setStore(undefined); setConnection(undefined); setInventory(undefined); setPending(false);
    setFailure('');
    // A new connection requires a fresh service identity and an explicitly entered credential.
    setIdentity(undefined);
    void ApiClient.identify(location.origin).then(setIdentity).catch(() => setFailure('Cannot verify service identity. Reload to retry.'));
  }

  const state = !client ? (pending ? 'connecting' : 'disconnected') : hidden ? 'stale' : connection?.status ?? 'connecting';
  return <div className="console">
    <header className="topbar">
      <a href="/" className="brand"><span className="brand-mark" aria-hidden="true">U</span><span>Universal OCPP Bridge<small>Management console</small></span></a>
      <span className={`environment ${identity?.runtime.environment ?? ''}`}>{identity?.runtime.environment ?? 'Identity unverified'}</span>
    </header>
    <div className="layout">
      <aside aria-label="Console navigation">
        <p className="section-label">Workspace</p>
        <a className="nav-active" href="#connection" aria-current="page">Connection</a>
        <a href="#stations" className="debug-nav">Stations</a>
        <a href="#commands" className="debug-nav">Commands</a>
        <a href="#debug" className="debug-nav">Debug timeline</a>
        <a href="/?offline=1">Offline capture inspector</a>
        <div className="sidebar-note">Local management<br/><span>HTTP / JSON + SSE</span></div>
      </aside>
      <main id="connection">
        <div className="page-heading"><div><p className="section-label">Bridge console</p><h1>Service connection</h1></div><span className={`status ${state}`} role="status">{state}</span></div>
        <p className="intro">Verify the destination, then connect with a scoped management credential.</p>
        <section className="identity-panel" aria-labelledby="identity-heading">
          <h2 id="identity-heading">Destination</h2>
          <Fields rows={[
            ['Bridge', identity?.bridge_id ?? 'Verifying…'], ['Target', identity?.selected_target_id ?? 'none selected'],
            ['Release', identity?.runtime.release_id ?? '—'], ['Origin', location.origin],
            ['Process', identity?.runtime.process_instance_id ?? '—'],
          ]}/>
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
            <div><dt>Inventory query</dt><dd>{inventory?.page?.items.length ?? 0} visible{inventory?.page?.next_cursor ? ' · more available' : ''}</dd></div>
            <div><dt>Events received</dt><dd>{connection?.received ?? 0}</dd></div>
            <div><dt>Reconnect attempts</dt><dd>{connection?.attempts ?? 0}</dd></div>
            <div><dt>History gaps</dt><dd>{connection?.gaps ?? 0}</dd></div>
            <div><dt>Latest event type</dt><dd>{connection?.lastEvent ?? 'No event received'}</dd></div>
            <div><dt>Last stream activity</dt><dd>{connection?.lastActivity ? new Date(connection.lastActivity).toLocaleTimeString() : 'Waiting'}</dd></div>
          </dl>
          <p className="field-note">Stream activity confirms connectivity, not a charger action. Station observations refresh separately.</p>
        </section>}
        {stationContext && <p className="notice" role="status">{stationContext}</p>}
        {client && store && inventory && <Stations state={inventory} store={store} scope={station} hidden={hidden}/>}
        {client && inventory?.detail && <Commands key={`${client.destinationKey}:${inventory.detail.station.station_id}`} client={client} snapshot={inventory.detail} hidden={hidden || !!inventory.stale}/>}
        <DiagnosticsPanel connection={connection} hidden={hidden}/>
        {identity && <Debug key={JSON.stringify(identity)} identity={identity} hidden={hidden}/>}
        <footer>Independent management interface <span>Bound to the destination shown above</span></footer>
      </main>
    </div>
  </div>;
}
