import { correlationId } from '../diagnostics/store';
import { useEffect, useRef, useState } from 'react';
import { Inspector } from './Inspector';
import { CommandTrace } from './CommandTrace';
import { filterNames, matches, TraceBuffer } from './buffer';
import type { Filters } from './buffer';

const rowHeight = 72;
const viewportHeight = 432;
export function Timeline({ buffer, tick, paused, ceiling, offline = false }: { offline?: boolean; buffer: TraceBuffer; tick: number; paused: boolean; ceiling: number }) {
  const [filters, setFilters] = useState<Filters>({});
  const [scroll, setScroll] = useState(0);
  const [auto, setAuto] = useState(true);
  const [selected, setSelected] = useState<number>();
  const [, redraw] = useState(0);
  useEffect(() => {
    if (offline) return;
    const selectCorrelation = (event: Event) => {
      const correlation = correlationId((event as CustomEvent<unknown>).detail);
      if (correlation) { setFilters({ correlation }); setScroll(0); setSelected(undefined); }
    };
    window.addEventListener('uob-correlation', selectCorrelation);
    return () => window.removeEventListener('uob-correlation', selectCorrelation);
  }, [offline]);
  const viewport = useRef<HTMLDivElement>(null);
  // Index matching does not parse or sort payloads; only the viewport is mounted in React.
  const rows = buffer.rows.filter(row => (!paused || row.sequence <= ceiling) && matches(row, filters));
  const first = Math.min(Math.max(0, Math.floor(scroll / rowHeight) - 2), Math.max(0, rows.length - 1));
  const visible = rows.slice(first, first + Math.ceil(viewportHeight / rowHeight) + 4);
  useEffect(() => {
    if (auto && !paused && viewport.current) viewport.current.scrollTop = viewport.current.scrollHeight;
  }, [tick, auto, paused]);
  const detail = selected === undefined ? undefined : buffer.detail(selected);
  function filter(key: keyof Filters, value: string) {
    setFilters(previous => ({ ...previous, [key]: value }));
    setScroll(0); if (viewport.current) viewport.current.scrollTop = 0;
  }
  return <>
    <details className="debug-filters" open={!!filters.correlation}><summary>Filter retained traces</summary>
      <div className="filter-grid">
        {filterNames.map(name => <label key={name}>{name}<input aria-label={`Filter ${name}`} value={filters[name] ?? ''} onChange={event => filter(name, event.target.value)} maxLength={128}/></label>)}
        {(['from', 'until'] as const).map(name => <label key={name}>{name} (local time)<input aria-label={`Filter ${name}`} type="datetime-local" onChange={event => filter(name, event.target.value)}/></label>)}
      </div>
      <p className="field-note">Missing metadata is unavailable, not inferred. Search “unavailable” in a field to select missing values. Severity is separate from outcome.</p>
    </details>
    <label>Search retained payload text<input aria-label="Search retained payload text" value={filters.search ?? ''} onChange={event => filter('search', event.target.value)} maxLength={128}/></label>
    <div className="debug-actions"><label><input type="checkbox" checked={auto} onChange={event => setAuto(event.target.checked)}/> Auto-scroll</label>
      <button className="secondary" onClick={() => { buffer.clear(); setSelected(undefined); redraw(value => value + 1); }}>Clear display buffer</button>
    </div>
    <p className="field-note">{rows.length} matching rows · {buffer.rows.length} retained · {buffer.bytes} encoded bytes · {buffer.evicted} browser evictions · {buffer.expiredBookmarks} expired bookmarks. Bookmarks: {buffer.bookmarks.size}/64.</p>
    <div className="bookmarks" aria-label="Retained bookmarks">{[...buffer.bookmarks].map(sequence => <button className="secondary" key={sequence} onClick={() => setSelected(sequence)}>Trace {sequence}</button>)}</div>
    <div ref={viewport} className="trace-viewport" role="region" aria-label="Trace timeline" tabIndex={0} onScroll={event => setScroll(event.currentTarget.scrollTop)}>
      <div style={{ height: rows.length * rowHeight, position: 'relative' }}>
        {visible.map((row, index) => <div className="trace-row" key={row.sequence} style={{ top: (first + index) * rowHeight, height: rowHeight }}>
          <button className="trace-open" onClick={() => setSelected(row.sequence)}>
            <strong>#{row.sequence} · {row.stage} · {row.index.direction} · {row.outcome}</strong>
            <span>{row.time} · {row.index.station || 'station unavailable'} · {row.index.correlation || 'uncorrelated'}</span>
            <span>{row.evidence || 'assurance unavailable'} · device: {row.deviceTime || 'unavailable'}{row.truncated ? ' · truncated' : ''}</span>
          </button>
          <button className="secondary" aria-label={`Bookmark trace ${row.sequence}`} aria-pressed={buffer.bookmarks.has(row.sequence)} onClick={() => { buffer.bookmark(row.sequence); redraw(value => value + 1); }}>{buffer.bookmarks.has(row.sequence) ? '★' : '☆'}</button>
        </div>)}
      </div>
    </div>
    {!rows.length && <p>No matching retained traces. {offline ? 'Offline inspection only.' : 'Opening this view does not enable capture.'}</p>}
    {selected !== undefined && <section className="trace-detail" aria-label="Trace detail">
      <button className="secondary" onClick={() => setSelected(undefined)}>Close detail</button>
      <CommandTrace rows={buffer.rows} selected={selected} ceiling={paused ? ceiling : Infinity} select={setSelected}/>
      <Inspector key={selected} inspection={buffer.inspection(selected)} raw={detail}/>
    </section>}
  </>;
}
