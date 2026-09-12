import { memo, useState } from 'react';
import { decisionExplanation, jsonTokens } from './inspection';
import type { Entry, Inspection } from './inspection';

export const InertJson = memo(function InertJson({ text }: { text: string }) {
  return <pre>{jsonTokens(text).map((token, index) => <span key={index} className={`json-${token.kind}`}>{token.text}</span>)}</pre>;
});
function Fields({ title, entries }: { title: string; entries: Entry[] }) {
  return <section aria-label={title}><h3>{title}</h3>{entries.length
    ? <InertJson text={JSON.stringify(Object.fromEntries(entries), null, 2)}/>
    : <p className="field-note">Unavailable in this trace.</p>}</section>;
}
export function Inspector({ inspection, raw }: { inspection?: Inspection; raw?: string }) {
  const [expanded, setExpanded] = useState(false);
  if (!inspection) return <p>{raw ?? 'This record was evicted or cleared. Its details are no longer retained.'}</p>;
  return <div className="inspector">
    <h2>Message and state inspector</h2>
    <p className="notice">Read-only, centrally redacted evidence. {inspection.truncated || inspection.omitted ? 'Truncated or omitted details: this is a partial view.' : 'No truncation reported.'}</p>
    <div className="representation-grid">
      <Fields title="Redacted source" entries={inspection.source}/>
      <Fields title="Canonical data" entries={inspection.canonical}/>
      <Fields title="Redacted target" entries={inspection.target}/>
    </div>
    <Fields title="Validation field paths and errors" entries={inspection.validation}/>
    <Fields title="Topic, API, node or register mappings" entries={inspection.mappings}/>
    <Fields title="Units and quality" entries={inspection.measurements}/>
    <Fields title="Opaque, unsupported or redacted fields" entries={inspection.unsupported}/>
    <Fields title="Original sizes (bytes) and omitted field counts" entries={inspection.sizes}/>
    <section aria-label="Changed fields"><h3>Changed fields</h3>
      <p className="field-note">At most 16 reported availability changes, in the producer’s resource order. No full snapshot is reconstructed.</p>
      {inspection.changes.length ? <table><thead><tr><th>Field path</th><th>Before</th><th>After</th></tr></thead>
        <tbody>{inspection.changes.map(change => <tr key={change.path}><td>{change.path}</td><td>{change.before}</td><td>{change.after}</td></tr>)}</tbody></table>
        : <p>No changed fields supplied; this does not prove unchanged state.</p>}
    </section>
    <section aria-label="Decision and trigger"><h3>Decision and trigger</h3>
      <dl><div><dt>Stage evidence</dt><dd>{inspection.evidence || 'Unavailable'}</dd></div>
        <div><dt>Safe reason</dt><dd>{inspection.reason || 'Unavailable'}</dd></div>
        <div><dt>Triggering parent trace</dt><dd>{inspection.trigger || 'Unavailable; no triggering message inferred'}</dd></div>
        <div><dt>Correlation</dt><dd>{inspection.correlation || 'Uncorrelated'}</dd></div></dl>
      <p>{decisionExplanation(inspection.evidence)}</p>
    </section>
    <details onToggle={event => setExpanded(event.currentTarget.open)}><summary>Redacted trace JSON</summary>
      {expanded && raw && <InertJson text={raw}/>}
    </details>
  </div>;
}
