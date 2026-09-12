// Fixed-size, tab-memory observations. Never accept exception objects, URLs, bodies or tokens.
export type RequestArea = 'identity' | 'inventory' | 'station' | 'command' | 'events' | 'capture' | 'management';
export type ExceptionKind = 'render' | 'recoverable' | 'uncaught' | 'rejection';
export const increment = (value: number) => Math.min(Number.MAX_SAFE_INTEGER, value + 1);
export function correlationId(value: unknown): string | undefined {
  // Only a canonical UUID is accepted as server correlation context. Arbitrary header text
  // could contain a reflected credential; absence/unsupported formats remain unavailable.
  return typeof value === 'string' && /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i.test(value) ? value : undefined;
}
export function requestArea(path: string): RequestArea {
  const route = path.split('?')[0];
  if (route === '/api/v1/identity') return 'identity';
  if (route === '/api/v1/stations') return 'inventory';
  if (route.startsWith('/api/v1/stations/')) return 'station';
  if (route.startsWith('/api/v1/commands')) return 'command';
  if (route.startsWith('/api/v1/events')) return 'events';
  if (route.startsWith('/api/v1/diagnostics/capture')) return 'capture';
  return 'management';
}
export class ClientDiagnostics {
  requests = 0;
  failures = 0;
  exceptions = 0;
  appCommits = 0;
  debugCommits = 0;
  lastFailure?: { area: RequestArea; status: number; correlation?: string; at: number };
  lastException?: { kind: ExceptionKind; at: number };
  request() { this.requests = increment(this.requests); }
  fail(area: RequestArea, status: number, correlation?: string) {
    this.failures = increment(this.failures);
    this.lastFailure = { area, status: Number.isInteger(status) && status >= 100 && status <= 599 ? status : 0,
      correlation: correlationId(correlation), at: Date.now() };
  }
  exception(kind: ExceptionKind) {
    this.exceptions = increment(this.exceptions);
    this.lastException = { kind, at: Date.now() };
  }
  commit(area: 'app' | 'debug') {
    if (area === 'app') this.appCommits = increment(this.appCommits);
    else this.debugCommits = increment(this.debugCommits);
  }
  clear() {
    this.requests = this.failures = this.exceptions = this.appCommits = this.debugCommits = 0;
    this.lastFailure = this.lastException = undefined;
  }
}
export const diagnostics = new ClientDiagnostics();
