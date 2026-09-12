import { increment } from '../diagnostics/store';
import { ApiClient, ApiError } from '../http';
import { identityKey, object, parseIdentity } from '../identity';
import { SseParser } from '../sse';
import { TraceBuffer } from './buffer';

export const capturePath = '/api/v1/diagnostics/capture';
export interface Capture {
  id: number; process: string; station: string | null; target: string | null;
  level: string; deadline: number;
}
export function parseCapture(value: unknown, api: ApiClient): Capture {
  const data = object(value);
  if (identityKey(parseIdentity(data.identity)) !== identityKey(api.identity)) { api.close(); throw new ApiError(0, 'identity'); }
  if (!Number.isSafeInteger(data.id) || Number(data.id) < 1 || typeof data.remaining_seconds !== 'number' || data.remaining_seconds < 0 || data.remaining_seconds > 1800) throw new ApiError(0, 'format');
  if (data.level !== 'metadata' && data.level !== 'redacted_payload') throw new ApiError(0, 'format');
  for (const key of ['station_id', 'target_id']) if (data[key] !== null && (typeof data[key] !== 'string' || String(data[key]).length > 256)) throw new ApiError(0, 'format');
  return { id: Number(data.id), process: api.identity.runtime.process_instance_id,
    station: data.station_id as string | null, target: data.target_id as string | null,
    level: data.level, deadline: Date.now() + data.remaining_seconds * 1000 };
}
export const sessionPath = (capture: Capture) => `${capturePath}/${encodeURIComponent(capture.process)}/${capture.id}`;
export interface TraceState { attempts?: number; lastActivity?: number; message: string; gaps: number; evicted: number; dropped: number; shed: number; terminal: boolean }
const count = (value: unknown) => {
  if (!Number.isSafeInteger(value) || Number(value) < 0) throw new ApiError(0, 'format');
  return Number(value);
};

// No per-event React updates and no second pending queue. Only buffer + scalar evidence mutate.
export function traceStream(api: ApiClient, capture: Capture, buffer: TraceBuffer, state: TraceState): () => void {
  const lifetime = new AbortController();
  let current: AbortController | undefined;
  let cursor: string | undefined;
  let identityPending = false;
  const identity = setInterval(() => {
    if (identityPending || lifetime.signal.aborted) return;
    identityPending = true;
    void api.verifyIdentity(lifetime.signal).catch(() => {
      state.message = 'Identity verification failed. Reconnect explicitly.'; state.terminal = true;
      lifetime.abort(); current?.abort();
    }).finally(() => { identityPending = false; });
  }, 15000);
  const expiry = setTimeout(() => {
    state.message = 'Capture deadline reached. Check status explicitly; retained history is incomplete.';
    state.terminal = true; lifetime.abort(); current?.abort();
  }, Math.max(0, capture.deadline - Date.now()));
  const window = (value: unknown) => {
    const data = object(value);
    state.evicted = count(data.evicted_records); state.dropped = count(data.dropped_records); state.shed = count(data.shed_records);
  };
  async function run() {
    let failures = 0;
    try {
      while (!lifetime.signal.aborted) {
        if (failures) state.attempts = increment(state.attempts ?? 0);
        current = new AbortController();
        const signal = AbortSignal.any([lifetime.signal, current.signal]);
        let reader: ReadableStreamDefaultReader<Uint8Array> | undefined;
        let watchdog = setTimeout(() => current?.abort(), 10000);
        const cancel = () => { void reader?.cancel().catch(() => {}); };
        signal.addEventListener('abort', cancel, { once: true });
        try {
          const response = await api.openStream(`${sessionPath(capture)}/traces`, cursor, signal);
          if (!response.ok) { await response.body?.cancel(); throw new ApiError(response.status); }
          if (!response.headers.get('content-type')?.startsWith('text/event-stream') || !response.body) { await response.body?.cancel(); throw new ApiError(0, 'format'); }
          reader = response.body.getReader();
          state.message = 'Live best-effort trace stream. Missing history cannot be recovered.';
          const parser = new SseParser(record => {
            if (record.event === 'trace') {
              const id = JSON.parse(record.id ?? 'null');
              if (!Array.isArray(id) || id.length !== 3 || id[0] !== capture.process || id[1] !== capture.id || !Number.isSafeInteger(id[2]) || id[2] < 0) throw new ApiError(0, 'format');
              buffer.append(record.data, capture.process, id[2]);
              cursor = record.id;
            } else if (record.event === 'trace_window') {
              const data = object(JSON.parse(record.data));
              if (data.process_instance_id !== capture.process || data.capture_id !== capture.id || data.replay !== 'best_effort') throw new ApiError(0, 'identity');
              window(data.window);
            } else if (record.event === 'trace_gap') {
              const data = object(JSON.parse(record.data));
              if (!['overflow', 'dropped', 'expiry', 'slow_reader'].includes(String(data.reason))) throw new ApiError(0, 'format');
              state.gaps = increment(state.gaps); state.message = `Trace gap: ${String(data.reason)}. History is incomplete.`;
              if (data.window) window(data.window);
              if (data.reason === 'expiry') throw new ApiError(410);
              if (data.reason === 'slow_reader') throw new ApiError(0, 'gap');
            } else throw new ApiError(0, 'format');
          });
          while (!signal.aborted) {
            clearTimeout(watchdog); watchdog = setTimeout(() => current?.abort(), 45000);
            const part = await reader.read();
            if (part.done) break;
            state.lastActivity = Date.now();
            parser.push(part.value);
          }
          throw new ApiError(0);
        } catch (error) {
          if (lifetime.signal.aborted) break;
          const fatal = !(error instanceof ApiError) || error.kind === 'identity' || error.kind === 'format' || error.kind === 'limit' || [400, 401, 403, 410].includes(error.status);
          if (fatal) {
            state.message = error instanceof ApiError && error.status === 410 ? 'Capture stopped, expired or process restarted. Retained history is incomplete.' : 'Trace access or validation failed. Reconnect explicitly.';
            state.terminal = true; break;
          }
          state.gaps = increment(state.gaps); state.message = 'Trace connection interrupted. Reconnecting with a best-effort cursor; data may be missing.';
          failures++;
        } finally {
          clearTimeout(watchdog); current.abort(); signal.removeEventListener('abort', cancel);
          await reader?.cancel().catch(() => {}); reader?.releaseLock();
        }
        await new Promise<void>(resolve => {
          const done = () => { clearTimeout(timer); lifetime.signal.removeEventListener('abort', done); resolve(); };
          const timer = setTimeout(done, Math.min(30000, 1000 * 2 ** Math.min(failures - 1, 5)));
          lifetime.signal.addEventListener('abort', done, { once: true });
          if (lifetime.signal.aborted) done();
        });
      }
    } finally { clearInterval(identity); clearTimeout(expiry); }
  }
  void run();
  return () => { lifetime.abort(); current?.abort(); clearInterval(identity); clearTimeout(expiry); };
}
