import { increment } from './diagnostics/store';
import { ApiClient, ApiError, readJson } from './http';
import { boundedText, object } from './identity';
import { SseParser } from './sse';
import type { StationSnapshot } from './stations/schema';

export interface ConnectionState {
  status: 'connecting' | 'live' | 'stale' | 'stopped';
  message: string;
  attempts: number;
  received: number;
  gaps: number;
  lastActivity?: number;
  lastEvent?: string;
}

export function subscribe(
  api: ApiClient, station: string, publish: (state: ConnectionState) => void,
  changes?: { stale: () => void; live: () => void; event: (stationId: string) => void; recovered: (snapshot: StationSnapshot) => void },
): () => void {
  const lifetime = new AbortController();
  let current: AbortController | undefined;
  let cursor: string | undefined;
  let gap: unknown;
  let terminal: ApiError | undefined;
  let state: ConnectionState = { status: 'connecting', message: 'Opening authenticated event stream…', attempts: 0, received: 0, gaps: 0 };
  let identityPending = false;
  const update = setInterval(() => { if (!lifetime.signal.aborted) publish({ ...state }); }, 1000);
  const identity = setInterval(() => {
    if (identityPending || lifetime.signal.aborted) return;
    identityPending = true;
    void api.verifyIdentity(lifetime.signal).catch(error => {
      if (lifetime.signal.aborted) return;
      if (error instanceof ApiError && error.kind === 'identity') terminal = error;
      current?.abort();
    }).finally(() => { identityPending = false; });
  }, 15000);

  async function recover(value: unknown) {
    const resource = object(object(object(value).recovery).resource);
    if (resource.bridge_id !== api.identity.bridge_id) throw new ApiError(0, 'identity');
    const selected = boundedText(resource.station_id);
    if (station && selected !== station) throw new ApiError(0, 'identity');
    const snapshot = await api.station(selected);
    if (lifetime.signal.aborted) return;
    changes?.recovered(snapshot);
    cursor = undefined;
    state.gaps = increment(state.gaps);
  }

  async function run() {
    let failures = 0;
    while (!lifetime.signal.aborted) {
      current = new AbortController();
      const signal = AbortSignal.any([lifetime.signal, current.signal]);
      let watchdog: ReturnType<typeof setTimeout>;
      const activity = () => {
        clearTimeout(watchdog);
        watchdog = setTimeout(() => current?.abort(), 75000);
        state.lastActivity = Date.now();
      };
      let reader: ReadableStreamDefaultReader<Uint8Array> | undefined;
      const cancel = () => { void reader?.cancel().catch(() => {}); };
      signal.addEventListener('abort', cancel, { once: true });
      const began = Date.now();
      try {
        watchdog = setTimeout(() => current?.abort(), 10000);
        const response = await api.openEvents(station, cursor, signal);
        if (lifetime.signal.aborted) { await response.body?.cancel(); break; }
        if (response.status === 410) {
          changes?.stale();
          await recover(await readJson(response));
          throw new ApiError(410, 'gap');
        }
        if (!response.ok) { await response.body?.cancel(); throw new ApiError(response.status); }
        if (!response.headers.get('content-type')?.startsWith('text/event-stream') || !response.body) {
          await response.body?.cancel();
          throw new ApiError(0, 'format');
        }
        reader = response.body.getReader();
        activity();
        if (lifetime.signal.aborted) break;
        state.status = 'live';
        state.message = state.gaps ? 'Connected after a history gap. Earlier events may be missing.' : 'Authenticated event stream connected.';
        changes?.live();
        publish({ ...state });
        const parser = new SseParser(record => {
          if (lifetime.signal.aborted) return;
          if (record.event === 'gap') { gap = JSON.parse(record.data); throw new ApiError(410, 'gap'); }
          if (record.event === 'error') {
            const code = object(JSON.parse(record.data)).error;
            if (code === 'events.resource_unauthorized') throw new ApiError(403);
            throw new ApiError(0, 'stream');
          }
          if (record.event === 'durable') {
            const event = object(JSON.parse(record.data));
            const runtime = object(event.runtime);
            // Historical durable events may originate from an earlier process/release.
            if (object(event.resource).bridge_id !== api.identity.bridge_id || runtime.environment !== api.identity.runtime.environment) {
              throw new ApiError(0, 'identity');
            }
            const type = boundedText(event.event_type, 128);
            const resource = object(event.resource);
            const stationId = boundedText(resource.station_id);
            if (station && station !== stationId) throw new ApiError(0, 'identity');
            state.received = Math.min(Number.MAX_SAFE_INTEGER, state.received + 1);
            changes?.event(stationId);
            state.lastEvent = type;
          }
          if (record.id !== undefined) cursor = record.id || undefined;
        });
        while (true) {
          const part = await reader.read();
          if (part.done) break;
          activity();
          try { parser.push(part.value); }
          catch (error) { throw error instanceof ApiError ? error : new ApiError(0, 'format'); }
        }
        throw new ApiError(0, 'closed');
      } catch (error) {
        if (lifetime.signal.aborted) break;
        const failure = terminal ?? (error instanceof ApiError ? error : new ApiError(0));
        if (failure.kind === 'identity' || failure.status === 401 || failure.status === 403 || failure.kind === 'limit' || failure.kind === 'format') {
          changes?.stale();
          api.close();
          state = { ...state, status: 'stopped', message: failure.kind === 'limit' || failure.kind === 'format' ? 'Stream exceeded the browser safety limits. Reconnect explicitly.' : failure.message };
          publish({ ...state });
          break;
        }
        state.status = 'stale';
        changes?.stale();
        state.message = 'Connection interrupted. Displayed data is stale; reconnecting…';
        publish({ ...state });
        // Release the old socket before fetching recovery data or starting another subscription.
        current.abort();
        if (gap !== undefined) {
          try { await recover(gap); gap = undefined; }
          catch { api.close(); state.message = 'History gap detected. Snapshot recovery is unavailable; reconnect explicitly.'; state.status = 'stopped'; publish({ ...state }); break; }
        }
        if (Date.now() - began > 10000) failures = 0;
        failures++;
      } finally {
        signal.removeEventListener('abort', cancel);
        clearTimeout(watchdog!);
        current.abort();
        await reader?.cancel().catch(() => {});
        reader?.releaseLock();
      }
      state.attempts = increment(state.attempts);
      const delay = Math.min(30000, 1000 * 2 ** Math.min(failures - 1, 5));
      await pause(delay, lifetime.signal);
    }
    clearInterval(update);
    clearInterval(identity);
  }
  void run();
  return () => { lifetime.abort(); current?.abort(); clearInterval(update); clearInterval(identity); };
}

function pause(ms: number, signal: AbortSignal) {
  return new Promise<void>(resolve => {
    const done = () => { clearTimeout(timer); signal.removeEventListener('abort', done); resolve(); };
    const timer = setTimeout(done, ms);
    signal.addEventListener('abort', done, { once: true });
    if (signal.aborted) done();
  });
}
