import { useEffect, useRef, useState } from 'react';
import type { FormEvent } from 'react';
import { ApiClient, ApiError } from '../http';
import type { Identity } from '../identity';
import { parseReleaseEvents, parseReleaseStatus, releaseEventsPath, releaseStatusPath } from './release';
import type { ReleaseSnapshot } from './release';

import { Fields } from '../Fields';
import type { Field } from '../Fields';

export function Release({ identity, hidden }: { identity: Identity; hidden: boolean }) {
  const [open, setOpen] = useState(false);
  const [snapshot, setSnapshot] = useState<ReleaseSnapshot>();
  const [pending, setPending] = useState(false);
  const [failure, setFailure] = useState('');
  const [received, setReceived] = useState('');
  const credential = useRef<HTMLInputElement>(null);
  const active = useRef<ApiClient | undefined>(undefined);
  const generation = useRef(0);
  const inFlight = useRef(false);
  function clear() {
    generation.current++; inFlight.current = false; active.current?.close(); active.current = undefined;
    if (credential.current) credential.current.value = '';
    setSnapshot(undefined); setPending(false); setFailure(''); setReceived('');
  }
  useEffect(() => {
    clear();
    window.addEventListener('pagehide', clear);
    return () => { clear(); window.removeEventListener('pagehide', clear); };
  }, [identity]);

  async function inspect(event?: FormEvent) {
    event?.preventDefault();
    if (inFlight.current || hidden || identity.runtime.environment !== 'production') return;
    const operation = ++generation.current;
    inFlight.current = true; setPending(true); setFailure('');
    try {
      if (!active.current) {
        const token = credential.current?.value ?? '';
        if (credential.current) credential.current.value = '';
        active.current = new ApiClient(location.origin, identity, token);
      }
      const client = active.current;
      const status = parseReleaseStatus(await client.request(releaseStatusPath));
      await client.verifyIdentity();
      const events = parseReleaseEvents(await client.request(releaseEventsPath));
      await client.verifyIdentity();
      if (operation !== generation.current) return;
      setSnapshot({ ...status, ...events }); setReceived(new Date().toLocaleTimeString());
    } catch (error) {
      if (operation !== generation.current) return;
      if (error instanceof ApiError && (error.kind === 'identity' || error.status === 401 || error.status === 403)) {
        clear(); setFailure(error.message);
      } else {
        if (!snapshot) { active.current?.close(); active.current = undefined; }
        setFailure('Release evidence unavailable. Retained data is stale; use the independent CLI.');
      }
    } finally {
      if (operation === generation.current) { inFlight.current = false; setPending(false); }
    }
  }
  return <section className="operations release-panel" aria-label="Environments and releases">
    <button className="secondary" aria-expanded={open} onClick={() => setOpen(value => !value)}>{open ? 'Hide' : 'Show'} environments and releases</button>
    {open && <>
      <h3>Environments and releases</h3>
      <Fields rows={[
        ['Environment', identity.runtime.environment], ['Bridge', identity.bridge_id], ['Origin', location.origin],
        ['Runtime release', identity.runtime.release_id], ['Runtime release digest (reported)', identity.runtime.release_digest],
      ]}/>
      {identity.runtime.environment !== 'production' ? <p>Release read routes and production credentials are unavailable here.</p> : <>
        {!snapshot && <form onSubmit={inspect} autoComplete="off">
          <label htmlFor="release-credential">Release read credential</label>
          <input id="release-credential" ref={credential} type="password" autoComplete="off" maxLength={8000} required disabled={pending} spellCheck={false}/>
          <p className="field-note">Memory-only; cleared on disconnect or navigation.</p>
          <button disabled={pending || hidden}>Inspect release status</button>
        </form>}
        {(snapshot || pending) && <div className="debug-actions">
          {snapshot && <button className="secondary" disabled={pending || hidden} onClick={() => void inspect()}>Refresh release evidence</button>}
          <button className="secondary" onClick={clear}>Disconnect release</button>
        </div>}
        {failure && <p className="notice error" role="alert">{failure}</p>}
        <p className="notice" role="status">{failure || hidden ? 'Retained release evidence is stale.' : snapshot ? `Authorized release evidence received. ${received}` : pending ? 'Reading release evidence…' : 'Not connected.'}</p>
        {snapshot && <>
          {snapshot.code === 'recovery_required' && <p className="notice error" role="alert">Supervisor recovery required. Use the CLI.</p>}
          <p className="field-note">Manual snapshot. Only journal pointers show current activation; all other evidence is historical and candidate-bound.</p>
          <Evidence snapshot={snapshot}/>
        </>}
      </>}
      <section aria-label="Release CLI authorization"><h4>Headless recovery</h4>
        <p><code>uob release status</code> / <code>uob release events</code> work with the bridge stopped. Mutations use separate local CLI permissions: stage or activate. No browser controls.</p>
      </section>
    </>}
  </section>;
}

function Evidence({ snapshot: s }: { snapshot: ReleaseSnapshot }) {
  const a = s.activation, q = s.status.qualification, p = s.status.promotion;
  const h = s.status.probation, f = s.status.failures, r = s.status.rollback;
  const d = s.audit.reduce<ReleaseSnapshot['audit'][number]['decision']>((last, row) => row.decision?.kind === 'promote' ? row.decision : last, undefined);
  const sections: { title: string; note?: string; rows: Field[] }[] = [
    { title: 'Activation journal pointers', note: a ? undefined : 'Journal unavailable; no inferred pointers.', rows: [
      ['Production digest', a?.production?.digest], ['Production phase', a?.production?.phase],
      ['Previous good digest', a?.previousGood], ['Candidate digest', a?.candidate?.digest], ['Candidate phase', a?.candidate?.phase],
    ] },
    { title: 'Qualification evidence', note: q ? undefined : 'Current qualification unavailable.', rows: [
      ['Current candidate digest', q?.candidate], ['Trusted evidence digest', q?.evidence], ['Pi measurements digest', q?.piMeasurements],
    ] },
    { title: 'Promotion and drain evidence', rows: d?.kind !== 'promote' ? [] : [
      ['Candidate digest', d.candidate], ['Previous good digest', d.previousGood], ['Evidence digest', d.evidence],
      ['Configuration digest', d.configuration], ['Compatibility check', d.compatibility], ['Drain check', d.drain],
      ['Health decision', d.health], ['Outcome', d.outcome],
    ] },
    { title: 'Probation health evidence', rows: h ? [
      ['Observed candidate', h.observation.candidate], ['Observed configuration', h.observation.configuration],
      ['Observed Unix seconds', h.observation.at], ['Verified seconds', h.verified],
      ['Required seconds', h.requiredSeconds], ['Maximum gap seconds', h.maximumGapSeconds],
      ['Interrupted intervals', h.interruptions], ['Health profile digest', h.profile],
      ...Object.entries(h.observation.checks).map(([check, passed]): Field => [check, passed === undefined ? undefined : passed ? 'passed' : 'failed']),
    ] : [] },
    { title: 'Failure and rollback evidence', rows: [
      ['Retained failure decision', f?.decision], ['Last retained failure signal', f?.last?.signal],
      ['Rollback quarantined digest', r?.quarantined], ['Rollback previous good digest', r?.previousGood],
      ['Rollback step', r?.step], ['Rollback reason', r?.reason],
    ] },
    { title: 'Production resource isolation evidence', note: 'Live resource measurements unavailable. Metadata is historical.', rows: p ? [
      ['Candidate digest', p.candidate], ['Production inputs digest', p.inputs], ['Production configuration digest', p.configuration],
      ['Operational database device', p.device], ['Operational database inode', p.inode],
      ['Promotion step', p.step], ['Recovery attempted', p.recoveryAttempted ? 'yes' : 'no'],
    ] : [] },
  ];
  return <>
    {sections.map(section => <section key={section.title} aria-label={section.title}><h4>{section.title}</h4>
      {section.note && <p className="field-note">{section.note}</p>}
      {section.rows.length ? <Fields rows={section.rows}/> : <p>Unavailable.</p>}
    </section>)}
    <section aria-label="Retained release audit"><h4>Retained release audit</h4>
      <p>{s.audit.length}/64 records · cursor {s.latestSequence} · {s.truncated ? 'Earlier records were truncated.' : 'No truncation reported.'}</p>
      {s.audit.map(row => <details key={row.sequence}><summary>Audit {row.sequence}: {row.operation} · {row.result}</summary>
        <Fields rows={[
          ['Actor / UID', `${row.actor} / ${row.uid}`], ['Digest', row.digest], ['Evidence digest', row.evidence],
          ...Object.entries(row.decision ?? {}).filter(([, value]) => typeof value === 'string').map(([key, value]): Field => [key, value as string]),
        ]}/>
      </details>)}
    </section>
  </>;
}
