import type { ReactNode } from 'react';

export type Field = [string, ReactNode];

/** Shared bounded evidence layout; values are inert React children, never HTML. */
export function Fields({ rows }: { rows: Field[] }) {
  return <dl>{rows.map(([label, value]) => <div key={label}><dt>{label}</dt><dd>{value ?? 'Unavailable'}</dd></div>)}</dl>;
}
