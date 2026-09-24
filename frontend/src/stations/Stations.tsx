import type { InventoryState, StationStore } from './store';
import type { Capabilities, Constraints, PointValue, ResourceRef, StationSnapshot, TypedValue } from './schema';

// Station IDs are indexed in retained traces; point, resource, and transaction
// identifiers in snapshots cannot be used to filter those traces.
function validFilter(value: string, maximum: number): boolean {
  if (!value || value.length > maximum) return false;
  for (let index = 0; index < value.length; index++) {
    const code = value.charCodeAt(index);
    if (code < 32 || code === 127) return false;
  }
  return true;
}

function DebugLink({ owner }: { owner: ResourceRef }) {
  const station = owner.station_id;
  if (!validFilter(station, 256)) return null;
  return <a href="#debug" onClick={() => window.dispatchEvent(new CustomEvent('uob-debug-filter', { detail: { station } }))}>
    Search retained Debug traces for station {station} (station-scoped only)
  </a>;
}

function address(ref: ResourceRef): string {
  const canonical = ref.resource;
  if (!canonical) return `Station ${ref.station_id}`;
  if (canonical.kind === 'connector') return `Connector ${canonical.connector_id}`;
  return `EVSE ${canonical.evse_id}${canonical.connector_id === undefined ? '' : ` / connector ${canonical.connector_id}`}`;
}
function native(ref: ResourceRef): string {
  const origin = ref.native_protocol_reference;
  if (!origin) return 'Native address unavailable';
  if (origin.protocol === 'ocpp16') return `OCPP 1.6 connector ${origin.connector_id}`;
  return `OCPP 2.0.1 EVSE ${origin.evse_id}${origin.connector_id === undefined ? '' : ` / connector ${origin.connector_id}`}`;
}
function value(item?: TypedValue): string {
  if (!item) return 'Unavailable';
  if (item.type === 'boolean') return item.value ? 'True' : 'False';
  return String(item.value);
}
function limits(item: Constraints): string {
  const parts = [item.minimum && `minimum ${value(item.minimum)}`, item.maximum && `maximum ${value(item.maximum)}`,
    item.enum_values.length && `allowed: ${item.enum_values.join(', ')}`].filter(Boolean);
  return parts.length ? parts.join(' · ') : 'No declared limits';
}
function CapabilitiesView({ capabilities }: { capabilities: Capabilities }) {
  return <div className="station-capabilities"><h4>Explicit capabilities</h4>
    {capabilities.operations.length ? <ul>{capabilities.operations.map((item, index) => <li key={index}>
      <strong>{item.operation.kind === 'protocol_action' ? `${item.operation.protocol} / ${item.operation.action}` : item.operation.kind}</strong>
      {item.parameters.length ? <ul>{item.parameters.map(param => <li key={param.name}>{param.name} ({param.value_type}{param.required ? ', required' : ', optional'}) · {limits(param.constraints)}</li>)}</ul> : ' · no declared parameters'}
    </li>)}</ul> : <p>No operations advertised; controls are not inferred.</p>}
    {!!capabilities.optional.length && <p>Optional: {capabilities.optional.map(item => `${item.name}${item.value ? `: ${value(item.value)}` : ' (present)'}`).join(' · ')}</p>}
    {!!capabilities.protocol_details.length && <p>Protocol facts: {capabilities.protocol_details.map(item => `${item.protocol} ${item.name}: ${value(item.value)}`).join(' · ')}</p>}
  </div>;
}
function PointList({ points, owner, descriptors = [] }: { points: PointValue[]; owner: ResourceRef; descriptors?: StationSnapshot['resources'][number]['data_points'] }) {
  const byId = new Map(descriptors.map(item => [item.point_id, item]));
  return <><h4>Observed points</h4>{points.length ? <ul className="point-list">{points.map(point => {
    const descriptor = byId.get(point.point_id);
    return <li key={point.point_id}><strong>{descriptor?.semantic_name ?? point.point_id}</strong> <code>{point.point_id}</code>
      <p className="point-reading">{value(point.value)}{descriptor?.unit && point.value ? ` ${descriptor.unit}` : ''} <span>({point.value?.type ?? 'missing'})</span></p>
      <p>Quality: {point.quality.level}{point.quality.reason ? ` · ${point.quality.reason}` : ''} · Freshness: {point.freshness.status}{point.freshness.valid_until ? ` until ${point.freshness.valid_until}` : ''}</p>
      <p>Source time: {point.source_time ?? 'unavailable'} · Observed: <time dateTime={point.observed_at}>{point.observed_at}</time></p>
      {descriptor && <p>{descriptor.access} · {descriptor.value_type} · {limits(descriptor.constraints)}</p>}
      {point.measurement && <p>Original: {point.measurement.original_value}{point.measurement.original_unit ? ` ${point.measurement.original_unit}` : ''}{point.measurement.phase ? ` · phase ${point.measurement.phase}` : ''}{point.measurement.context ? ` · ${point.measurement.context}` : ''}</p>}
      <p><DebugLink owner={owner}/></p>
    </li>;
  })}</ul> : <p>No point observations available.</p>}</>;
}
function Detail({ station }: { station: StationSnapshot }) {
  return <article className="station-detail" aria-label={`Station ${station.station.station_id} detail`}>
    <h3>{address(station.station)}</h3>
    <p>Canonical: <code>{station.station.bridge_id} / {station.station.station_id}</code> · {native(station.station)}</p>
    <p>Connection: {station.connectivity.status}{station.connectivity.protocol ? ` · ${station.connectivity.protocol}` : ''} · Snapshot observed <time dateTime={station.observed_at}>{station.observed_at}</time></p>
    {station.connectivity.connected_at && <p>Connected: {station.connectivity.connected_at} · Last message: {station.connectivity.last_message_at ?? 'unknown'}</p>}
    <CapabilitiesView capabilities={station.capabilities}/>
    <h4>Transactions</h4>
    {station.transactions.length ? <ul className="transaction-list">{station.transactions.map(tx => <li key={tx.transaction_id}>
      <strong>{tx.state}</strong> · <code>{tx.transaction_id}</code> · {address(tx.resource)} · {native(tx.resource)}<br/>
      Started {tx.started_at}{tx.ended_at ? ` · Ended ${tx.ended_at}` : ''}
      {' · '}<DebugLink owner={tx.resource}/>
    </li>)}</ul> : <p>No current or recently ended transactions recorded.</p>}
    <PointList points={station.current_values} owner={station.station}/>
    <h4>Charging resources ({station.resources.length})</h4>
    {station.resources.length ? station.resources.map((resource, index) => {
      const observed = new Set(resource.current_values.map(point => point.point_id));
      const missing = resource.data_points.filter(point => !observed.has(point.point_id));
      return <section key={`${address(resource.resource)}-${index}`} className="resource-card">
        <h5>{address(resource.resource)} · {resource.availability}</h5>
        <p>Canonical: <code>{resource.resource.bridge_id} / {resource.resource.station_id} / {address(resource.resource)}</code> · {native(resource.resource)}</p>
        <CapabilitiesView capabilities={resource.capabilities}/>
        <PointList points={resource.current_values} owner={resource.resource} descriptors={resource.data_points}/>
        {!!missing.length && <p>Described, not observed: {missing.map(point => point.semantic_name).join(', ')}</p>}
      </section>;
    }) : <p>No connector or EVSE topology observed.</p>}
  </article>;
}
export function Stations({ state, store, scope, hidden = false }: { state: InventoryState; store: StationStore; scope: string; hidden?: boolean }) {
  const items = state.page?.items ?? [];
  return <section className="stations-panel" id="stations" aria-labelledby="stations-heading">
    <div className="stations-heading"><div><p className="section-label">Queried observations</p><h2 id="stations-heading">Stations</h2></div>
      <button type="button" className="secondary" onClick={() => store.refresh()}>Refresh snapshot</button></div>
    <p className="field-note">Read-only canonical snapshots as of each query. The event stream observes only the selected station; other inventory rows are not continuously live. Operations shown below are advertised capabilities, not command controls.</p>
    {scope && <p className="field-note">Event scope: {scope}. Other stations cannot be selected in this connection; reconnect with a different scope to inspect them.</p>}
    {(state.stale || hidden) && <p className="notice error" role="status">Snapshot stale or refreshing. Do not treat displayed observations as live.</p>}
    {state.error && <p className="notice error" role="alert">{state.error}</p>}
    <div className="station-columns"><nav aria-label="Station inventory"><h3>Inventory ({items.length} shown)</h3>
      {items.length ? <ul className="station-list">{items.map(item => {
        const id = item.station.station_id;
        return <li key={id}><button type="button" className="station-select" aria-current={state.selected === id ? 'true' : undefined}
          disabled={!!scope && scope !== id} onClick={() => store.select(id)}>{id}</button>
          <span>{item.connectivity.status} · {item.resources.length} resources · {item.transactions.length} transactions</span></li>;
      })}</ul> : <p>{state.loading ? 'Loading inventory…' : 'No stations in this page.'}</p>}
      {state.page?.next_cursor && <button type="button" className="secondary" disabled={state.moreLoading} onClick={() => store.more()}>{state.moreLoading ? 'Loading…' : 'Load next page'}</button>}
    </nav><div className="station-detail-container">{state.detail ? <>
      <DebugLink owner={state.detail.station}/>
      <Detail station={state.detail}/>
      <p className="field-note">Debug links show station-scoped, already retained traces, not transaction, resource, or point matches. Opening Debug does not start capture; no matching trace is guaranteed.</p>
    </> : <p>{state.loading ? 'Loading station detail…' : 'Select a station to inspect its state.'}</p>}</div></div>
  </section>;
}
