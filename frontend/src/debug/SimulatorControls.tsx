import { useEffect, useRef, useState } from 'react';
import type { FormEvent } from 'react';
import type { Identity } from '../identity';
import { availableControls, controlSeed, SimulatorController } from './simulator-control';
import type { ControlRun, Intervention, RunEntry, Scenario } from './simulator-control';

export function SimulatorControls({ identity, hidden, origin }: { identity: Identity; hidden: boolean; origin: string }) {
  const [client, setClient] = useState<SimulatorController>();
  const [catalog, setCatalog] = useState<Scenario[]>([]);
  const [runs, setRuns] = useState<RunEntry[]>([]);
  const [scenarioId, setScenarioId] = useState('');
  const [stationId, setStationId] = useState('');
  const [seed, setSeed] = useState('');
  const [runId, setRunId] = useState('');
  const [run, setRun] = useState<ControlRun>();
  const [stale, setStale] = useState(false);
  const [stepId, setStepId] = useState('');
  const [kind, setKind] = useState('');
  const [delay, setDelay] = useState(150);
  const [pending, setPending] = useState(false);
  const [message, setMessage] = useState('');
  const [error, setError] = useState('');
  const token = useRef<HTMLInputElement>(null);
  const active = useRef<SimulatorController | undefined>(undefined);
  const generation = useRef(0);
  const selected = catalog.find(item => item.id === scenarioId);
  const steps = run?.steps.filter(step => step.station === stationId) ?? [];
  const step = steps.find(item => item.id === stepId);
  const controls = run?.status === 'running' && !stale && step ? availableControls(step) : [];
  let seedValid = true;
  if (seed) {
    try { controlSeed(seed); } catch { seedValid = false; }
  }

  function clear() {
    generation.current++;
    active.current?.close(); active.current = undefined;
    if (token.current) token.current.value = '';
    setClient(undefined); setCatalog([]); setRuns([]); setRun(undefined); setRunId(''); setStepId('');
    setStale(false); setPending(false); setError(''); setMessage('');
  }
  useEffect(() => {
    const leave = () => { generation.current++; active.current?.close(); active.current = undefined; if (token.current) token.current.value = ''; };
    window.addEventListener('pagehide', leave);
    return () => { leave(); window.removeEventListener('pagehide', leave); };
  }, []);
  useEffect(() => { clear(); }, [identity, origin]);

  async function connect(event: FormEvent) {
    event.preventDefault();
    if (client || pending || hidden) return;
    const credential = token.current?.value ?? '';
    if (token.current) token.current.value = '';
    const operation = ++generation.current;
    setPending(true); setError('');
    let next: SimulatorController | undefined;
    try {
      next = new SimulatorController(origin, credential, identity);
      active.current = next;
      const scenarios = await next.catalog(location.origin);
      const existing = await next.list(location.origin);
      if (operation !== generation.current) { next.close(); return; }
      setCatalog(scenarios); setRuns(existing); setClient(next);
      setScenarioId(scenarios[0]?.id ?? ''); setStationId(scenarios[0]?.stations[0] ?? '');
      setMessage('Catalog and retained run list received. No run has been started by this connection.');
    } catch {
      next?.close();
      if (operation === generation.current) setError('Simulator control connection unavailable. Check the separate control credential, configured console origin, environment and bridge identity.');
    } finally { if (operation === generation.current) setPending(false); }
  }

  async function operation(action: 'list' | 'start' | 'status' | 'stop' | 'remove' | 'intervene') {
    if (!client || pending || hidden) return;
    const sequence = ++generation.current;
    setPending(true); setError(''); setMessage('');
    setStale(true);
    try {
      if (action === 'list') {
        setRuns(await client.list(location.origin));
        setStale(!!run);
        setMessage('Retained run list refreshed. Select a run to inspect its current server state.');
      } else if (action === 'start' && selected) {
        const id = await client.start(location.origin, selected, seed ? controlSeed(seed) : undefined);
        if (sequence !== generation.current) return;
        setRunId(id); setRun(undefined); setStepId(''); setStale(false);
        setRuns(previous => [...previous.filter(item => item.id !== id), { id, scenario: selected.id, seed: seed || selected.seed, terminal: false }]);
        setMessage(`Server acknowledged run ${id}. Refresh its status for observed progress.`);
      } else if (action === 'status' && runId) {
        const current = await client.status(location.origin, runId);
        if (sequence !== generation.current) return;
        setRun(current); setStale(false);
        if (catalog.some(item => item.id === current.scenario)) setScenarioId(current.scenario);
        setStationId(previous => current.steps.some(item => item.station === previous) ? previous : current.steps[0]?.station ?? '');
        setStepId(previous => current.steps.some(item => item.id === previous) ? previous : current.steps[0]?.id ?? '');
        setMessage(`Server snapshot: ${current.status}. This is not continuous observation.`);
      } else if (action === 'stop' && runId && run && !stale && (run.status === 'running' || run.status === 'stopping')) {
        await client.stop(location.origin, runId);
        setMessage('Server acknowledged stop request; refresh status for the actual terminal result.');
      } else if (action === 'remove' && runId && run && !stale && (run.status === 'passed' || run.status === 'failed')) {
        await client.remove(location.origin, runId);
        setRunId(''); setRun(undefined); setStepId(''); setStale(false);
        setMessage('Server acknowledged terminal run removal. Refresh the list to confirm remaining runs.');
      } else if (action === 'intervene' && runId && run && !stale && step && controls.includes(kind)) {
        const intervention: Intervention = step.action === 'wait' && (kind === 'disconnect' || kind === 'reconnect')
          ? { kind } : { kind: 'fault', fault: kind, delay_ms: kind === 'response_delay' || kind === 'out_of_order_response' ? delay : 0 };
        await client.intervene(location.origin, runId, step, intervention);
        setMessage(`Server acknowledged scheduling for step ${step.id}; refresh status for execution and assertion evidence.`);
      } else throw new Error('Selection unavailable');
    } catch {
      if (sequence === generation.current) {
        setError(action === 'start' || action === 'stop' || action === 'intervene' || action === 'remove'
          ? 'Control outcome unconfirmed. A request may have reached the server. Last confirmed snapshot is stale. Refresh the run list/status before any deliberate new action; no automatic retry occurred.'
          : 'Simulator refresh unavailable or invalid. Last confirmed snapshot is retained but stale; refresh explicitly before another control.');
        if (client.closed) { clear(); setError('Bridge identity changed or session ended. Control credential cleared; reconnect explicitly.'); }
      }
    } finally { if (sequence === generation.current) setPending(false); }
  }

  return <section className="operations-panel" aria-label="Simulator scenario controls">
    <h3>Simulator scenario controls</h3>
    <p>Separate control credential, explicitly entered here. The Debug evidence credential and bridge credentials cannot start or change runs. No broker or EMS control is available.</p>
    {!client && <form autoComplete="off" onSubmit={connect}>
      <label>Simulator control credential<input ref={token} type="password" autoComplete="off" maxLength={64} required disabled={pending}/></label>
      <p>Target: {identity.runtime.environment.toUpperCase()} · {origin}. Control is allowed only when this simulator explicitly permits this console origin.</p>
      <button disabled={pending || hidden}>Enter simulator controls</button>
    </form>}
    {client && <>
      <p className="notice">Verified target: {identity.runtime.environment.toUpperCase()} · {client.origin}. Actions below affect the isolated simulator, not the normal bridge command API.</p>
      <label>Authored scenario<select value={scenarioId} disabled={pending} onChange={event => { const next = catalog.find(item => item.id === event.target.value); setScenarioId(event.target.value); setStationId(next?.stations[0] ?? ''); }}>
        {catalog.map(item => <option key={item.id} value={item.id}>{item.id}</option>)}
      </select></label>
      {selected && <>
        <p>Full station set affected when starting: {selected.stations.join(', ') || 'No stations'} · authored seed {selected.seed}. Station selection below filters display only; it does not narrow a run.</p>
        <label>Station context<select value={stationId} disabled={pending} onChange={event => { setStationId(event.target.value); setStepId(''); }}>
          {[...new Set([...selected.stations, ...(run?.steps.map(step => step.station) ?? [])])].map(id => <option key={id} value={id}>{id}</option>)}
        </select></label>
        <p>Authored steps: {selected.steps.filter(step => step.station === stationId).map(step => `${step.id} (${step.action})`).join(', ') || 'None for this station'}.</p>
        <label>Exact seed override (optional, unsigned 64-bit decimal)<input value={seed} onChange={event => setSeed(event.target.value)} maxLength={20} inputMode="numeric" autoComplete="off" disabled={pending}/></label>
        <button disabled={pending || hidden || stale || !selected.stations.length || !seedValid} onClick={() => void operation('start')}>Start scenario on {identity.runtime.environment}</button>
      </>}
      <div className="debug-actions"><button className="secondary" disabled={pending || hidden} onClick={() => void operation('list')}>Recover / refresh run list</button>
        <button className="secondary" onClick={clear}>Leave controls and clear credential</button></div>
      <label>Retained simulator run<select value={runId} disabled={pending} onChange={event => { setRunId(event.target.value); setRun(undefined); setStepId(''); setStale(false); }}>
        <option value="">Choose a run</option>{runs.map(item => <option key={item.id} value={item.id}>{item.id} · {item.scenario} · {item.terminal ? 'terminal' : 'active'}</option>)}
        {runId && !runs.some(item => item.id === runId) && <option value={runId}>{runId} · awaiting list refresh</option>}
      </select></label>
      {runId && <div className="debug-actions"><button className="secondary" disabled={pending || hidden} onClick={() => void operation('status')}>Refresh selected run status</button>
        <button className="secondary" disabled={pending || hidden || stale || !run || !['running', 'stopping'].includes(run.status)} onClick={() => void operation('stop')}>Request stop</button>
        <button className="secondary" disabled={pending || hidden || stale || !run || !['passed', 'failed'].includes(run.status)} onClick={() => void operation('remove')}>Remove terminal run</button></div>}
      {run && <section aria-label="Simulator control result">
        {stale && <p className="notice error" role="status">Last confirmed snapshot is stale or a control outcome is unconfirmed. These are historical values, not live status; refresh before another control.</p>}
        <p>Run {run.id} · {run.scenario} · exact seed {run.seed} · {stale ? 'last confirmed, stale server status' : 'observed server status'} {run.status}.</p>
        {run.failure && <p className="notice error">{stale ? 'Last confirmed terminal failure' : 'Terminal failure'}: {run.failure.category} · {run.failure.code}</p>}
        <label>Pending station-owned step / result<select value={stepId} disabled={pending} onChange={event => { setStepId(event.target.value); setKind(''); }}>
          <option value="">Choose a step</option>{steps.map(item => <option key={item.id} value={item.id}>{item.id} · {item.status}</option>)}
        </select></label>
        {step && <><p>Station {step.station} · action {step.action} · {step.status} · expected {step.expected ?? 'not declared'} · observed {step.actual ?? 'not observed'}.</p>
          <p>Scheduled intervention: {step.intervention ? `${step.intervention.kind}${step.intervention.kind === 'fault' ? ` ${step.intervention.fault} · ${step.intervention.delay_ms} ms` : ''}` : 'none'} · effect {step.effectStatus ?? 'not scheduled'} · response delay scope {step.responseDelayScope ?? 'not applicable'} · selected fault: {step.selected === undefined ? 'not evaluated' : step.selected ? 'yes' : 'no'} · assertion {step.passed === undefined ? 'not completed' : step.passed ? 'passed' : 'failed'}.</p>
          <p>Failure: {step.category ?? 'none recorded'} · {step.failure ?? 'none recorded'}.</p>
          {step.responseDelayScope === 'step_completion' && <p className="field-note">Heartbeat response_delay and missing_response affect simulator step completion after the Heartbeat exchange; they do not delay or suppress the peer socket reply. Use a remote-command step with peer_reply scope to exercise wire response timing.</p>}
          {controls.length > 0 && <><label>Eligible intervention<select value={kind} disabled={pending} onChange={event => setKind(event.target.value)}>
            <option value="">Choose a server-eligible control</option>{controls.map(name => <option key={name} value={name}>{name}</option>)}
          </select></label>
            {(kind === 'response_delay' || kind === 'out_of_order_response') && <label>{step.responseDelayScope === 'step_completion' ? 'Step completion delay (ms)' : 'Peer response delay (ms)'}<input type="number" min={1} max={30000} value={delay} onChange={event => setDelay(Number(event.target.value))} disabled={pending}/></label>}
            <button disabled={pending || hidden || !controls.includes(kind) || (kind === 'response_delay' || kind === 'out_of_order_response') && (!Number.isInteger(delay) || delay < 1 || delay > 30000)} onClick={() => void operation('intervene')}>Schedule intervention for {step.station}</button>
          </>}
          <p>Context only, not per-step correlation: <a href="#stations" onClick={() => window.dispatchEvent(new CustomEvent('uob-simulator-station', { detail: step.station }))}>Select {step.station} in normal station inventory</a> · <a href="#debug" onClick={() => window.dispatchEvent(new CustomEvent('uob-debug-filter', { detail: { station: step.station } }))}>Filter retained Debug traces by {step.station}</a>. Neither view guarantees an observation of this simulator step.</p>
          {step.correlation && <p>Server correlation <code>{step.correlation}</code> · <button className="secondary" onClick={() => {
            window.dispatchEvent(new CustomEvent('uob-correlation', { detail: step.correlation }));
            document.querySelector('[aria-label="Trace timeline"]')?.scrollIntoView();
          }}>Find exact correlation in retained traces</button></p>}
        </>}
      </section>}
    </>}
    {pending && <p role="status">Waiting for simulator response…</p>}
    {message && <p className="notice" role="status">{message}</p>}
    {error && <p className="notice error" role="alert">{error}</p>}
    {hidden && <p className="notice">Hidden tab: controls are disabled; refresh explicitly after returning.</p>}
  </section>;
}
