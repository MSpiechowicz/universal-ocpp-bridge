import { commandEvidence, commandTrace, stages, COMMAND_LINK_LIMIT } from './command';
import type { Row } from './buffer';

export function CommandTrace({ rows, selected, ceiling, select }: {
  rows: readonly Row[]; selected: number; ceiling: number; select: (sequence: number) => void;
}) {
  const trace = commandTrace(rows, selected, ceiling);
  if (!trace) return null;
  const { anchor, evidence, related, omitted } = trace;
  const links = (items: Row[]) => items.slice(-COMMAND_LINK_LIMIT).map(row => <li key={row.sequence}>
    <button className="secondary" onClick={() => select(row.sequence)}>Inspect trace {row.sequence}</button>
    <span>{row.stage} · {commandEvidence(row)}</span>
    <span>Safe reason: {row.command.reason || 'Unavailable'} · target: {row.index.target || 'Unavailable'}</span>
    {row.command.event && <span>Observed event: {row.command.event}</span>}
    <span>Server: {row.time} · device: {row.deviceTime || 'Unavailable'}</span>
    <span>Local span elapsed: {row.command.duration ? `${row.command.duration} µs` : 'Unavailable'}</span>
  </li>);
  return <section className="command-trace inspector" aria-label="Command evidence">
    <h2>Command evidence</h2>
    <p className="notice">Partial retained evidence, not a complete command history. Gaps, eviction and capture scope can hide stages. Missing evidence does not establish success or failure.</p>
    <dl><div><dt>Request identity</dt><dd>{anchor.command.request || 'Unavailable; only the selected row establishes command evidence'}</dd></div>
      <div><dt>Correlation</dt><dd>{anchor.command.correlation || 'Unavailable; no neighboring events linked'}</dd></div>
      <div><dt>Originating target / client</dt><dd>{evidence.find(row => row.command.origin)?.command.origin || 'Unavailable in retained evidence'}</dd></div>
      <div><dt>Queue wait</dt><dd>Unavailable as an independent measurement. Local span elapsed includes other work; durations from different spans are not subtracted.</dd></div></dl>
    <table><thead><tr><th>Evidence stage</th><th>Retained observation</th></tr></thead><tbody>{stages.map(([stage, label]) => {
      const records = evidence.filter(row => row.stage === stage);
      return <tr key={stage}><th scope="row">{label}</th><td>{records.length
        ? records.slice(-4).map(row => <p key={row.sequence}>#{row.sequence}: {commandEvidence(row)}{row.command.reason && ` · ${row.command.reason}`}</p>)
        : 'Not evidenced in the retained request traces'}{records.length > 4 && <p>{records.length - 4} earlier observations omitted from this summary.</p>}</td></tr>;
    })}</tbody></table>
    {evidence.some(row => row.command.partial) && <p className="notice">Some command details were truncated or shed.</p>}
    <h3>Request traces and observed event references</h3>
    <ol className="command-links">{links(evidence)}</ol>
    {evidence.length > COMMAND_LINK_LIMIT && <p>{evidence.length - COMMAND_LINK_LIMIT} earlier request links omitted.</p>}
    <h3>Related correlation traces and target reports</h3>
    <p className="field-note">Exact correlation, process and station matches only. These links may include other requests; they do not establish this request’s outcomes or its originating target’s consumption.</p>
    {related.length ? <ol className="command-links">{links(related)}</ol> : <p>No additional correlated traces retained.</p>}
    {omitted > 0 && <p>{omitted} additional correlation links omitted.</p>}
  </section>;
}
