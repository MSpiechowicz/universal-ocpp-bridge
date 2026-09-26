import { useEffect, useRef, useState } from 'react';
import { ApiClient, ApiError } from '../http';
import type { ResourceRef, StationSnapshot } from '../stations/schema';
import { composeOperation, newDraft, sameResource } from './model';
import type { CommandOptions, CommandPage, CommandRow, Draft } from './model';

const failure = (error: unknown): string => error instanceof ApiError ? error.message : error instanceof Error &&
  ['Operation is not advertised for this resource.', 'Resource is not in the selected station.', 'Privileged action is not advertised with a supported schema.'].includes(error.message)
  ? error.message : error instanceof Error && /^(Enter |Charging limit |Choose |Phases |Select |Unsupported |Request expired|[\w]+ (is |must |exceeds |has ))/.test(error.message)
    ? error.message : 'Command data unavailable or invalid. No result is inferred.';
const label = (resource: ResourceRef): string => resource.resource?.kind === 'connector' ? `Connector ${resource.resource.connector_id}`
  : resource.resource?.kind === 'evse' ? `EVSE ${resource.resource.evse_id}${resource.resource.connector_id ? ` / ${resource.resource.connector_id}` : ''}` : `Station ${resource.station_id}`;
function Evidence({ item }: { item: CommandRow }) {
  const lifecycle = item.lifecycle;
  let stage = 'Unknown; check status';
  if (lifecycle?.stage === 'admitted') stage = 'Admitted; dispatch not established';
  if (lifecycle?.stage === 'dispatched') stage = 'Dispatched; response pending';
  if (lifecycle?.stage === 'protocol_response') {
    stage = lifecycle.accepted ? 'Charger accepted protocol request; physical effect not established' : 'Charger rejected protocol request';
  }
  if (lifecycle?.stage === 'transmission_uncertain') stage = 'Transmission uncertain; no automatic replay';
  if (lifecycle?.stage === 'rejected') stage = 'Rejected';
  const admission = item.admitted_at ?? (lifecycle?.stage === 'rejected' ? 'Rejected' : lifecycle ? 'Confirmed by lifecycle, timestamp unavailable' : 'Not established');
  return <div className="command-evidence">
    <p><strong>{item.operation ?? 'Command'} · {label(item.resource)}</strong> · Request <code>{item.request_id}</code></p>
    <p>Admission: {admission} · Deadline: {item.expires_at ?? 'not available'}</p>
    <p>Dispatch / protocol response: {stage}{lifecycle?.error ? ` · ${lifecycle.error.code}${lifecycle.error.detail ? `: ${lifecycle.error.detail}` : ''}` : ''}
      {lifecycle?.detail ? ` · ${lifecycle.detail}` : ''} · Latest recorded: {item.recorded_at ?? 'not available'}</p>
    <p>Observed physical effect: {item.observed_effects.length ? item.observed_effects.map(effect =>
      <span key={effect.event_id}>{effect.event_type} · event <code>{effect.event_id}</code> · {effect.observed_at}; </span>) : 'None linked. Protocol acceptance is not charging success.'}</p>
    {item.correlation_id && <a href="#debug" onClick={() => window.dispatchEvent(new CustomEvent('uob-correlation', { detail: item.correlation_id }))}>Search retained diagnostics by correlation {item.correlation_id}</a>}
    <p className="field-note">Durable event IDs identify observed effects. The station event stream and retained Debug traces are separate, possibly incomplete views.</p>
  </div>;
}

export function Commands({ client, snapshot, hidden }: { client: ApiClient; snapshot: StationSnapshot; hidden: boolean }) {
  const station = snapshot.station;
  const [options, setOptions] = useState<CommandOptions>();
  const [history, setHistory] = useState<CommandPage>();
  const [detail, setDetail] = useState<CommandRow>();
  const [resourceIndex, setResourceIndex] = useState(0);
  const [choice, setChoice] = useState('');
  const [values, setValues] = useState<Record<string, string>>({});
  const [confirmed, setConfirmed] = useState(false);
  const [draft, setDraft] = useState<Draft>();
  const [busy, setBusy] = useState(false);
  const [notice, setNotice] = useState('');
  const [loading, setLoading] = useState(true);
  const credential = useRef<HTMLInputElement>(null);
  const generation = useRef(0);
  const sending = useRef(false);
  const [identityLost, setIdentityLost] = useState(false);
  const refs = [station, ...snapshot.resources.map(item => item.resource)];
  const resource = refs[resourceIndex] ?? station;
  const capabilities = resourceIndex === 0 ? snapshot.capabilities : snapshot.resources[resourceIndex - 1]?.capabilities;
  const operations = capabilities?.operations.filter(item => ['start', 'stop', 'set_charging_limit'].includes(item.operation.kind) &&
    (item.operation.kind !== 'start' || (options?.start && sameResource(options.start.resource, resource)))) ?? [];
  const offered = (options?.items ?? []).filter(schema => sameResource(schema.resource, resource) && capabilities?.operations.some(item =>
    item.operation.kind === 'protocol_action' && item.operation.protocol === schema.protocol && item.operation.action === schema.action));
  const selectedSchema = offered.find(schema => `ocpp:${schema.protocol}:${schema.action}` === choice);
  const selectedOperation = operations.find(item => item.operation.kind === choice);
  const supported = !!selectedOperation || !!selectedSchema;
  const stopped = hidden || busy || loading || identityLost;
  function reportError(error: unknown) {
    if (error instanceof ApiError && error.kind === 'identity') setIdentityLost(true);
    setNotice(failure(error));
  }

  function changeValue(name: string, value: string) {
    setValues(current => ({ ...current, [name]: value }));
    setConfirmed(false);
  }

  useEffect(() => {
    let live = true;
    const leave = () => {
      generation.current++;
      if (credential.current) credential.current.value = '';
      setOptions(undefined); setDraft(undefined); setConfirmed(false);
    };
    window.addEventListener('pagehide', leave);
    setLoading(true);
    void client.commandHistory(station).then(rows => {
      if (live) setHistory(rows);
    }).catch(error => {
      if (live) reportError(error);
    })
      .finally(() => { if (live) setLoading(false); });
    return () => { live = false; window.removeEventListener('pagehide', leave); generation.current++; if (credential.current) credential.current.value = ''; };
  }, [client, station.bridge_id, station.station_id]);

  async function loadOptions() {
    if (stopped || !credential.current?.value) return;
    const sequence = ++generation.current;
    setBusy(true); setConfirmed(false); setOptions(undefined); setChoice('');
    try {
      const available = await client.commandSchemas(station, credential.current.value);
      if (sequence === generation.current) { setOptions(available); setNotice('Protected control options loaded for this credential and station.'); }
    } catch (error) {
      if (sequence === generation.current) {
        if (credential.current) credential.current.value = '';
        reportError(error);
      }
    } finally { if (sequence === generation.current) setBusy(false); }
  }
  useEffect(() => {
    if (!hidden) return;
    generation.current++;
    setConfirmed(false); setOptions(undefined); setChoice(''); setBusy(false);
    if (credential.current) credential.current.value = '';
  }, [hidden]);

  function field(name: string, placeholder = '', type = 'text') {
    return <label key={name}>{name.replaceAll('_', ' ')}<input type={type} value={values[name] ?? ''} placeholder={placeholder}
      onChange={event => changeValue(name, event.target.value)} disabled={stopped || !!draft} maxLength={1024} autoComplete="off"/></label>;
  }
  async function refresh(id?: string) {
    if (busy) return;
    const sequence = ++generation.current;
    setBusy(true); setNotice('');
    try {
      const result = id ? await client.commandStatus(id, station) : await client.commandHistory(station);
      if (sequence !== generation.current) return;
      if (id) setDetail(result as CommandRow);
      else setHistory(result as CommandPage);
    } catch (error) {
      if (sequence === generation.current) reportError(error);
    } finally { if (sequence === generation.current) setBusy(false); }
  }
  async function submit(event: { preventDefault(): void }, retry = false) {
    event.preventDefault();
    if (sending.current || stopped || !confirmed || (!retry && (!supported || draft))) return;
    const control = credential.current?.value ?? '';
    if (credential.current) credential.current.value = '';
    setConfirmed(false);
    if (!control || control.length > 8000) { setNotice('Enter a separate control credential for this submission.'); return; }
    let request: Draft;
    try {
      if (retry) {
        if (!draft || Date.parse(draft.expires_at) <= Date.now()) throw new Error('Request expired; create a new command instead.');
        request = draft;
      } else {
        const parameters = choice === 'start' ? { ...values, authorization_reference: options?.start?.authorization_reference ?? '' } : values;
        const operation = composeOperation(snapshot, resource, selectedSchema ? 'ocpp' : choice, parameters, selectedSchema);
        request = newDraft(resource, operation);
      }
    } catch (error) { setNotice(failure(error)); return; }
    sending.current = true;
    const sequence = ++generation.current;
    setDraft(request); setOptions(undefined); setBusy(true); setNotice('Submitting once; an unknown response is not permission to create a new request.');
    try {
      await client.submitCommand(request, control, client.destinationKey);
      if (sequence !== generation.current) return;
      setNotice('Submission returned. Read status to distinguish admission, protocol response and observed effects.');
    } catch (error) {
      if (sequence !== generation.current) return;
      if (error instanceof ApiError && error.kind === 'identity') setIdentityLost(true);
      setNotice(error instanceof ApiError && error.kind.startsWith('command.') ? `${failure(error)} Check this request ID for its durable status before another action.` :
        `${failure(error)} Submission outcome may be unknown. Check this request ID before intentional retry.`);
    } finally { sending.current = false; if (sequence === generation.current) setBusy(false); }
  }
  function reset() {
    generation.current++;
    setDraft(undefined); setDetail(undefined); setOptions(undefined); setValues({}); setChoice('');
    setNotice('New request will receive a new identity; inspect earlier request status first. Reload protected options before another start or privileged action.');
  }
  return <section className="commands-panel" id="commands" aria-labelledby="commands-heading">
    <h2 id="commands-heading">Authorized commands · {station.station_id}</h2>
    <p className="field-note">Only explicit per-resource capabilities and server-supported privileged schemas appear. Read, control and privileged credentials have separate grants: control authorizes standard actions, privileged authorizes OCPP actions. A schema does not imply that this credential may submit it. Admission and charger response do not prove a charging effect.</p>
    {notice && <p className="notice" role="status">{notice}</p>}
    {hidden && <p className="notice error">Tab hidden: refresh station observations before preparing another action.</p>}
    <form onSubmit={event => void submit(event)} autoComplete="off">
      <label>Target resource<select value={resourceIndex} disabled={stopped || !!draft} onChange={event => { setResourceIndex(Number(event.target.value)); setChoice(''); setValues({}); setConfirmed(false); }}>
        {refs.map((ref, index) => <option key={index} value={index}>{label(ref)}</option>)}</select></label>
      <label>Advertised operation<select value={choice} disabled={stopped || !!draft} onChange={event => { setChoice(event.target.value); setValues({}); setConfirmed(false); }}>
        <option value="">Choose an operation</option>
        {operations.map(item => <option key={item.operation.kind} value={item.operation.kind}>{item.operation.kind.replaceAll('_', ' ')}</option>)}
        {offered.map(schema => <option key={`${schema.protocol}:${schema.action}`} value={`ocpp:${schema.protocol}:${schema.action}`}>Privileged {schema.protocol} / {schema.action}</option>)}
      </select></label>
      {!loading && !operations.length && !offered.length && <p>No supported operation is available for this resource and entered grant. Start needs a provisioned control authorization reference; privileged actions need a pinned server schema.</p>}
      {choice === 'start' && supported && <p className="field-note">Using a protected, resource-scoped authorization reference; its value is not displayed.</p>}
      {choice === 'stop' && supported && <label>Open transaction<select value={values.transaction_id ?? ''} disabled={stopped || !!draft} onChange={event => { setValues({ transaction_id: event.target.value }); setConfirmed(false); }}>
        <option value="">Choose an open transaction</option>
        {snapshot.transactions.filter(tx => tx.state !== 'ended' && (resourceIndex === 0 || sameResource(tx.resource, resource))).map(tx =>
          <option key={tx.transaction_id} value={tx.transaction_id}>{tx.transaction_id}</option>)}
      </select></label>}
      {choice === 'set_charging_limit' && supported && <>
        {field('value', 'Exact decimal, no exponent')}
        <label>Engineering unit<select value={values.unit ?? ''} disabled={stopped || !!draft} onChange={event => changeValue('unit', event.target.value)}>
          <option value="">Select unit</option>{['ampere', 'milliampere', 'watt', 'kilowatt'].map(unit => <option key={unit}>{unit}</option>)}
        </select></label>{field('phases', 'Optional: 1, 2 or 3')}
        {selectedOperation?.parameters.map(param => <p key={param.name} className="field-note">{param.name}: {param.required ? 'required' : 'optional'}
          {param.constraints.minimum && ` · min ${param.constraints.minimum.value}`}{param.constraints.maximum && ` · max ${param.constraints.maximum.value}`}
          {!!param.constraints.enum_values.length && ` · allowed ${param.constraints.enum_values.join(', ')}`}</p>)}
      </>}
      {selectedSchema && <><p className="field-note">Pinned schema: <code>{selectedSchema.payload_schema}</code></p>
        {selectedSchema.fields.map(item => item.enum_values?.length ? <label key={item.name}>{item.name}<select value={values[item.name] ?? ''} disabled={stopped || !!draft}
          onChange={event => changeValue(item.name, event.target.value)}>
          <option value="">Choose {item.name}</option>{item.enum_values.map(value => <option key={value}>{value}</option>)}</select></label>
          : field(item.name, item.value_type))}</>}
      <label>Independent control or privileged credential<input ref={credential} type="password" autoComplete="off" maxLength={8000} required disabled={stopped}
        onChange={() => { setOptions(undefined); setChoice(''); setConfirmed(false); }}/></label>
      <button type="button" className="secondary" disabled={stopped || !!draft} onClick={() => void loadOptions()}>Load protected control options</button>
      <label className="destination-confirmation"><input type="checkbox" checked={confirmed} disabled={stopped || (!supported && !draft)} onChange={event => setConfirmed(event.target.checked)}/>
        Confirm this submission only: {client.identity.runtime.environment.toUpperCase()} · bridge {client.identity.bridge_id} · release {client.identity.runtime.release_id} · target {client.identity.selected_target_id ?? 'none'} · station {station.station_id} · {client.origin}
      </label>
      {draft ? <><p>Immutable request <code>{draft.request_id}</code> · correlation <code>{draft.correlation_id}</code> · expires {draft.expires_at}</p>
        <div className="command-actions"><button type="button" className="secondary" disabled={busy} onClick={() => void refresh(draft.request_id)}>Check request status</button>
          <button type="button" disabled={stopped || !confirmed || Date.parse(draft.expires_at) <= Date.now()} onClick={event => void submit(event, true)}>Intentionally retry exact request</button>
          <button type="button" className="secondary" disabled={busy} onClick={reset}>Prepare new request</button></div></>
        : <button type="submit" disabled={stopped || !supported || !confirmed}>Submit once</button>}
    </form>
    {detail && <section aria-label="Command detail"><h3>Server-backed request detail</h3><Evidence item={detail}/></section>}
    <div className="command-actions"><h3>Server-backed sanitized history</h3><button type="button" className="secondary" disabled={busy} onClick={() => void refresh()}>Refresh history</button></div>
    {history ? <><ul className="command-history">{history.items.map(item => <li key={item.request_id}><Evidence item={item}/>
      <button type="button" className="secondary" disabled={busy} onClick={() => void refresh(item.request_id)}>Inspect detail</button></li>)}</ul>
      {!history.items.length && <p>No commands in this authorized station page.</p>}
      {history.next_cursor && <button type="button" className="secondary" disabled={busy} onClick={() => {
        const sequence = ++generation.current; setBusy(true);
        void client.commandHistory(station, history.next_cursor).then(next => {
          if (sequence === generation.current) setHistory({ items: [...history.items, ...next.items], next_cursor: next.next_cursor });
        }).catch(error => { if (sequence === generation.current) setNotice(failure(error)); })
          .finally(() => { if (sequence === generation.current) setBusy(false); });
      }}>Load next history page</button>}</> : <p>History unavailable or loading; status remains queryable by request ID after submission.</p>}
  </section>;
}
