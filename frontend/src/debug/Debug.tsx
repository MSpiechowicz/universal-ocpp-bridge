import { diagnostics } from '../diagnostics/store';
import { useEffect, useRef, useState } from 'react';
import type { FormEvent } from 'react';
import { ApiClient, ApiError } from '../http';
import type { Identity } from '../identity';
import { TraceBuffer } from './buffer';
import { capturePath, parseCapture, sessionPath, traceStream } from './capture';
import type { Capture, TraceState } from './capture';
import { Timeline } from './Timeline';

export function Debug({ identity, hidden }: { identity: Identity; hidden: boolean }) {
  const [buffer] = useState(() => new TraceBuffer());
  const [state] = useState<TraceState>(() => ({ message: 'Capture has not been requested. Authenticate to inspect status or explicitly start capture.', gaps: 0, evicted: 0, dropped: 0, shed: 0, terminal: false }));
  const [tick, setTick] = useState(0);
  const [api, setApi] = useState<ApiClient>();
  const [capture, setCapture] = useState<Capture>();
  const [pending, setPending] = useState(false);
  const [failure, setFailure] = useState('');
  const [confirmed, setConfirmed] = useState(false);
  const [paused, setPaused] = useState(false);
  const [ceiling, setCeiling] = useState(-1);
  const [station, setStation] = useState('');
  const [target, setTarget] = useState('');
  const [level, setLevel] = useState('metadata');
  const [duration, setDuration] = useState(600);
  const credential = useRef<HTMLInputElement>(null);
  const active = useRef<ApiClient | undefined>(undefined);
  const generation = useRef(0);
  useEffect(() => { diagnostics.commit('debug'); });
  useEffect(() => {
    const leave = () => { generation.current++; active.current?.close(); };
    window.addEventListener('pagehide', leave);
    return () => { leave(); buffer.clear(); window.removeEventListener('pagehide', leave); };
  }, [buffer]);
  useEffect(() => {
    if (!capture) return;
    // Five stream-driven renders per second maximum; no idle/hidden render queue.
    let previous = '';
    const timer = setInterval(() => {
      if (document.hidden) return;
      const current = JSON.stringify([buffer.version, state, Math.max(0, Math.ceil((capture.deadline - Date.now()) / 1000))]);
      if (current !== previous) { previous = current; setTick(value => value + 1); }
    }, 200);
    return () => clearInterval(timer);
  }, [capture, buffer, state]);
  useEffect(() => {
    if (!api || !capture) return;
    state.terminal = false;
    return traceStream(api, capture, buffer, state);
  }, [api, capture, buffer, state]);

  async function status(client: ApiClient): Promise<Capture | undefined> {
    try { return parseCapture(await client.request(capturePath), client); }
    catch (error) { if (error instanceof ApiError && error.status === 410) return undefined; throw error; }
  }
  async function connect(event: FormEvent) {
    event.preventDefault();
    if (pending || api) return;
    const token = credential.current?.value ?? '';
    if (credential.current) credential.current.value = '';
    const operation = ++generation.current;
    setPending(true); setFailure('');
    let client: ApiClient | undefined;
    try {
      client = new ApiClient(location.origin, identity, token); active.current = client;
      const current = await status(client);
      if (operation !== generation.current) { client.close(); return; }
      setApi(client); setCapture(current);
      state.message = current ? 'Reading the authorized active capture.' : 'No active capture. Collection starts only when explicitly requested.';
    } catch (error) { client?.close(); if (operation === generation.current) setFailure(safeFailure(error)); }
    finally { if (operation === generation.current) setPending(false); }
  }
  async function control(action: 'start' | 'stop' | 'status') {
    if (!api || pending || (action !== 'status' && !confirmed)) return;
    const destination = action === 'status' ? undefined : api.destinationKey;
    setConfirmed(false);
    const operation = generation.current;
    setPending(true); setFailure('');
    try {
      let current: Capture | undefined;
      if (action === 'start') {
        current = parseCapture(await api.request(capturePath, { method: 'POST', body: JSON.stringify({ station_id: station || null, target_id: target || null, level, duration_seconds: duration }) }, undefined, destination), api);
      } else if (action === 'stop' && capture) {
        await api.request(`${sessionPath(capture)}/stop`, { method: 'POST' }, undefined, destination);
      } else current = await status(api);
      if (operation !== generation.current) return;
      if (current && current.id !== capture?.id) buffer.clear();
      setCapture(current); setPaused(false);
      state.message = current ? 'Reading the authorized active capture.' : action === 'stop' ? 'Capture stopped. Displayed traces are a finite, incomplete history.' : 'No active capture.';
    } catch (error) { if (operation === generation.current) setFailure(safeFailure(error)); }
    finally { if (operation === generation.current) setPending(false); }
  }
  function disconnect() {
    generation.current++; active.current?.close(); active.current = undefined;
    setConfirmed(false); setApi(undefined); setCapture(undefined); setPending(false); setFailure(''); buffer.clear();
    state.message = 'Disconnected. Credentials and display buffer cleared. A server capture keeps its own deadline.';
  }
  return <section id="debug" className="debug-panel" aria-labelledby="debug-heading">
    <h2 id="debug-heading">Debug timeline</h2>
    <p className="debug-destination">{identity.runtime.environment.toUpperCase()} · {identity.bridge_id} · {identity.runtime.release_id} · target {identity.selected_target_id ?? 'none selected'} · station {capture?.station ?? (station || 'all authorized')}</p>
    <p>Read-only troubleshooting. Capture requires separate diagnostic permissions and an explicit start. No charging commands or replay controls are provided here.</p>
    {!api && <form onSubmit={connect} autoComplete="off">
      <label htmlFor="debug-credential">Diagnostic credential</label>
      <input id="debug-credential" ref={credential} type="password" autoComplete="off" maxLength={8000} required disabled={pending}/>
      <button disabled={pending}>Inspect capture status</button>
    </form>}
    {api && <>
      <div className="filter-grid">
        <label>Capture station<input value={station} onChange={event => setStation(event.target.value)} maxLength={256} disabled={!!capture || pending}/></label>
        <label>Capture target<input value={target} onChange={event => setTarget(event.target.value)} maxLength={256} disabled={!!capture || pending}/></label>
        <label>Capture level<select value={level} onChange={event => setLevel(event.target.value)} disabled={!!capture || pending}><option value="metadata">Metadata</option><option value="redacted_payload">Redacted payload</option></select></label>
        <label>Capture seconds<input type="number" value={duration} min={1} max={1800} onChange={event => setDuration(Number(event.target.value))} disabled={!!capture || pending}/></label>
      </div>
      <label className="destination-confirmation"><input type="checkbox" checked={confirmed} disabled={pending} onChange={event => setConfirmed(event.target.checked)}/>
        Confirm next control destination: {identity.runtime.environment.toUpperCase()} · {identity.bridge_id} · release {identity.runtime.release_id} · target {identity.selected_target_id ?? 'none selected'} · {location.origin}
      </label>
      <div className="debug-actions">
        <button disabled={!confirmed || pending || !!capture || !Number.isInteger(duration) || duration < 1 || duration > 1800 || (level === 'redacted_payload' && !station)} onClick={() => void control('start')}>Start capture on {identity.runtime.environment}</button>
        <button className="secondary" disabled={!confirmed || pending || !capture} onClick={() => void control('stop')}>Stop capture on {identity.runtime.environment}</button>
        <button className="secondary" disabled={pending} onClick={() => void control('status')}>Refresh capture status</button>
      </div>
    </>}
    {(api || pending) && <button className="secondary" onClick={disconnect}>Disconnect diagnostics</button>}
    {failure && <p className="notice error" role="alert">{failure}</p>}
    <p className="notice" role="status">{state.message} {hidden ? 'Hidden tab: display is stale.' : ''}</p>
    {capture && <p>Capture #{capture.id} · {capture.level} · station {capture.station ?? 'all authorized'} · target {capture.target ?? 'all authorized'} · {Math.max(0, Math.ceil((capture.deadline - Date.now()) / 1000))} seconds remaining (estimate; server enforces expiry).</p>}
    <p>Trace reconnect attempts: {state.attempts ?? 0} · last activity: {state.lastActivity ? new Date(state.lastActivity).toLocaleTimeString() : 'Unavailable'}. {paused ? 'Display paused; capture continues.' : hidden ? 'Hidden display; observations may be stale.' : 'Best-effort stream; reconnect or restart can lose history.'}</p>
    <p>Stream gaps: {state.gaps} · server evictions: {state.evicted} · dropped: {state.dropped} · details shed: {state.shed}</p>
    <button className="secondary" onClick={() => { setCeiling(buffer.rows.at(-1)?.sequence ?? -1); setPaused(value => !value); }}>{paused ? 'Resume display' : 'Pause display'}</button>
    {paused && <p className="notice">Display paused at trace {ceiling}. Capture and expiry continue. Evicted rows disappear; resume shows the current retained window.</p>}
    <Timeline buffer={buffer} tick={tick} paused={paused} ceiling={ceiling}/>
  </section>;
}
function safeFailure(error: unknown) {
  if (error instanceof ApiError && error.status === 409) return 'Another capture is active. Refresh status; its scope was not changed.';
  return error instanceof ApiError ? error.message : 'Diagnostic operation failed. No raw server error is displayed.';
}
