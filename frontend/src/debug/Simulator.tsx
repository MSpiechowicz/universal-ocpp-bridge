import { useEffect, useRef, useState } from 'react';
import type { FormEvent } from 'react';
import type { Identity } from '../identity';
import { SimulatorControls } from './SimulatorControls';
import { SimulatorReader } from './simulator';
import type { SimulatorEvidence } from './simulator';

export function Simulator({ identity, hidden }: { identity: Identity; hidden: boolean }) {
  const [origin, setOrigin] = useState('http://127.0.0.1:9001');
  if (identity.runtime.environment === 'production') return null;
  return <><Evidence identity={identity} hidden={hidden} origin={origin} setOrigin={setOrigin}/>
    <SimulatorControls identity={identity} hidden={hidden} origin={origin}/></>;
}
function Evidence({ identity, hidden, origin, setOrigin }: { identity: Identity; hidden: boolean; origin: string; setOrigin: (value: string) => void }) {
  const [run, setRun] = useState('1');
  const [reader, setReader] = useState<SimulatorReader>();
  const [snapshot, setSnapshot] = useState<{ data: SimulatorEvidence; at: number }>();
  const [selected, setSelected] = useState(0);
  const [pending, setPending] = useState(false);
  const [failure, setFailure] = useState(false);
  const token = useRef<HTMLInputElement>(null);
  const active = useRef<SimulatorReader | undefined>(undefined);
  const generation = useRef(0);
  function clear() {
    generation.current++; active.current?.close(); active.current = undefined;
    if (token.current) token.current.value = '';
    setReader(undefined); setSnapshot(undefined); setPending(false); setFailure(false);
  }
  useEffect(() => {
    const leave = () => { generation.current++; active.current?.close(); active.current = undefined; if (token.current) token.current.value = ''; };
    window.addEventListener('pagehide', leave);
    return () => { leave(); window.removeEventListener('pagehide', leave); };
  }, []);
  useEffect(() => { clear(); }, [identity, origin]);
  async function read(client: SimulatorReader) {
    const operation = ++generation.current;
    setPending(true); setFailure(false); setSnapshot(undefined);
    try {
      const data = await client.read(location.origin);
      if (generation.current === operation) { setSnapshot({ data, at: Date.now() }); setSelected(0); }
    } catch {
      if (generation.current === operation) { client.close(); setReader(undefined); setFailure(true); }
    } finally { if (generation.current === operation) setPending(false); }
  }
  function connect(event: FormEvent) {
    event.preventDefault();
    if (pending || reader || hidden) return;
    const credential = token.current?.value ?? '';
    if (token.current) token.current.value = '';
    try {
      const next = new SimulatorReader(origin, run, credential, identity);
      active.current = next; setReader(next); void read(next);
    } catch { setSnapshot(undefined); setFailure(true); }
  }
  const data = snapshot?.data;
  const step = data?.steps[selected];
  return <section className="operations-panel" aria-label="Simulator scenario evidence">
    <h3>Simulator scenario evidence</h3>
    <p>Inspect a run from an explicitly enabled, isolated simulator. Use its separate Debug read credential. Charging commands stay in the ordinary bridge API.</p>
    {!reader && <form onSubmit={connect} autoComplete="off">
      <label>Simulator origin<input value={origin} onChange={e => setOrigin(e.target.value)} maxLength={128} required disabled={pending}/></label>
      <label>Simulator run ID<input value={run} onChange={e => setRun(e.target.value)} maxLength={20} required disabled={pending}/></label>
      <label>Simulator Debug read credential<input ref={token} type="password" autoComplete="off" maxLength={64} required disabled={pending}/></label>
      <p>Destination: {identity.runtime.environment.toUpperCase()} · {origin} · run {run}. This credential is sent only to that simulator.</p>
      <button disabled={pending || hidden}>Read simulator evidence</button>
    </form>}
    {reader && <button className="secondary" disabled={pending || hidden} onClick={() => void read(reader)}>Refresh simulator evidence</button>}
    {(reader || pending || snapshot) && <button className="secondary" onClick={clear}>Clear simulator credential and evidence</button>}
    {failure && <p className="notice error" role="alert">Simulator evidence unavailable. Check run, environment, separate read credential and allowed console origin. Previous evidence was cleared.</p>}
    {hidden && <p className="notice">Hidden tab: evidence is stale. Refresh after returning.</p>}
    {data && <>
      <p className="notice">{data.environment.toUpperCase()} · {reader?.origin} · scenario {data.scenario} · run {data.run} · seed {data.seed} · {data.status}</p>
      <p>Finite snapshot received {new Date(snapshot.at).toLocaleTimeString()}. Refresh explicitly for new progress. Pending steps may never have run.</p>
      {data.failure && <p className="notice error">Terminal failure: {data.failure.category} · {data.failure.code}. A setup failure may leave every step pending.</p>}
      {data.events.filter(event => event.event === 'run_failed').map((event, index) => <p className="notice error" key={index}>Run failure: {event.category ?? 'Unavailable'} · {event.failure ?? 'Unavailable'} <Correlation value={event.correlation}/></p>)}
      <label>Inspect scenario step<select value={selected} onChange={e => setSelected(Number(e.target.value))}>
        {data.steps.map((step, index) => <option key={step.id} value={index}>{step.id} · {step.status}</option>)}
      </select></label>
      {step && <section aria-label="Simulator step detail">
        <h4>{step.id} · {step.station} · {step.action}</h4>
        <dl>
          <dt>Expected event</dt><dd>{step.expected ?? 'No declared event expectation'}</dd>
          <dt>Actual event</dt><dd>{step.actual ?? 'Unavailable — no completed action event recorded'}</dd>
          <dt>Assertion outcome</dt><dd>{step.passed === undefined ? 'Not completed' : step.passed ? 'Passed' : 'Failed'} · {step.status}</dd>
          <dt>Detail assertion</dt><dd>{step.detailAssertion ? 'Declared; wire values omitted' : 'None declared'}</dd>
          <dt>Failure</dt><dd>{step.category ?? 'None recorded'} · {step.failure ?? 'None recorded'}</dd>
          <dt>Configured fault / selected</dt><dd>{step.fault ?? 'None'} · {step.selected === undefined ? 'Not evaluated' : step.selected ? 'Selected' : 'Not selected'}</dd>
          <dt>Server-eligible controls</dt><dd>{step.eligible.join(', ') || 'None'}</dd>
          <dt>Scheduled intervention</dt><dd>{step.intervention ?? 'None'} {step.interventionFault} {step.delay !== undefined && `· ${step.delay} ms`}</dd>
        </dl>
        <Correlation value={step.correlation}/>
        <h4>Recorded logical events</h4>
        {data.events.filter(event => event.step === step.id).map((event, index) => <p key={index}>{event.id} · {event.event} · {event.status} <Correlation value={event.correlation}/></p>)}
      </section>}
      <p className="field-note">Correlation links filter existing authorized traces; missing or evicted traces stay missing. Simulator event IDs are not server correlation IDs. Inspection cannot replay events, inject faults or start charging.</p>
    </>}
  </section>;
}
function Correlation({ value }: { value?: string }) {
  return value ? <><code>{value}</code> <button className="secondary" onClick={() => {
    window.dispatchEvent(new CustomEvent('uob-correlation', { detail: value }));
    document.querySelector('[aria-label="Trace timeline"]')?.scrollIntoView();
  }}>Find simulator correlation in retained traces</button></> : <span>Server correlation unavailable.</span>;
}
