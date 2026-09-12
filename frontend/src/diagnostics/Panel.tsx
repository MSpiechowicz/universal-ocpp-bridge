import { useEffect, useState } from 'react';
import { diagnostics } from './store';
import type { ConnectionState } from '../events';

export function DiagnosticsPanel({ connection, hidden }: { connection?: ConnectionState; hidden: boolean }) {
  const [, refresh] = useState(0);
  useEffect(() => {
    const timer = setInterval(() => { if (!document.hidden) refresh(value => (value + 1) % 1000000); }, 1000);
    return () => clearInterval(timer);
  }, []);
  const failure = diagnostics.lastFailure;
  const exception = diagnostics.lastException;
  return <section className="debug-panel" aria-labelledby="client-diagnostics-heading">
    <h2 id="client-diagnostics-heading">Browser and API diagnostics</h2>
    <p className="notice">{hidden ? 'Hidden tab: display may be stale.' : connection ? `Event stream: ${connection.status}.` : 'No management event subscription.'} Inventory is a point-in-time query; stream activity does not refresh it.</p>
    <dl>
      <div><dt>API requests / failures</dt><dd>{diagnostics.requests} / {diagnostics.failures}</dd></div>
      <div><dt>Sanitized frontend exceptions</dt><dd>{diagnostics.exceptions}{exception ? ` · ${exception.kind} · ${new Date(exception.at).toLocaleTimeString()}` : ' · none observed'}</dd></div>
      <div><dt>App / Debug committed updates</dt><dd>{diagnostics.appCommits} / {diagnostics.debugCommits}</dd></div>
      <div><dt>Event reconnect attempts / history gaps</dt><dd>{connection?.attempts ?? 0} / {connection?.gaps ?? 0}</dd></div>
      <div><dt>Last event stream activity</dt><dd>{connection?.lastActivity ? new Date(connection.lastActivity).toLocaleTimeString() : 'Unavailable'}</dd></div>
      <div><dt>Latest API failure</dt><dd>{failure ? `${failure.area} · ${failure.status || 'network or validation'} · ${new Date(failure.at).toLocaleTimeString()}` : 'None observed'}</dd></div>
    </dl>
    {failure && <p>Server correlation: {failure.correlation ? <><code>{failure.correlation}</code> <button className="secondary" onClick={() => {
      window.dispatchEvent(new CustomEvent('uob-correlation', { detail: failure.correlation }));
      document.getElementById('debug')?.scrollIntoView();
    }}>Find in retained traces</button></> : 'Unavailable (missing or unsupported identifier)'}</p>}
    <p className="field-note">Only the latest failure and exception category are retained, with bounded counters. No error messages, stacks, request URLs, response bodies or credentials are recorded. Correlation lookup filters existing traces and does not start capture.</p>
  </section>;
}
