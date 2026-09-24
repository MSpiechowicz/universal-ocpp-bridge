import { diagnostics, requestArea } from './diagnostics/store';
import { boundedText, identityKey, object, parseIdentity } from './identity';
import type { Identity } from './identity';
import { parsePage, parseStation } from './stations/schema';

export class ApiError extends Error {
  constructor(public readonly status: number, public readonly kind = 'request') {
    super(kind === 'destination' ? 'Confirm the visible destination before this control operation.'
      : kind === 'identity' ? 'Service identity changed. Disconnect and authenticate again.'
      : status === 401 || status === 403 ? 'Access denied. Check the credential and resource scope.'
      : status === 503 ? 'Management data is unavailable on this service configuration.'
      : status === 429 ? 'Service is busy. Retrying with a delay.'
      : 'Request failed. Data may be stale.');
  }
}

export async function readJson(response: Response, exactIntegers = false): Promise<unknown> {
  const reader = response.body?.getReader();
  if (!reader) throw new ApiError(0);
  const bytes = new Uint8Array(1024 * 1024);
  let length = 0;
  try {
    while (true) {
      const part = await reader.read();
      if (part.done) break;
      length += part.value.byteLength;
      if (length > 1024 * 1024) throw new ApiError(0, 'limit');
      bytes.set(part.value, length - part.value.length);
    }
    const text = new TextDecoder('utf-8', { fatal: true }).decode(bytes.subarray(0, length));
    return JSON.parse(exactIntegers ? preserveLargeIntegers(text) : text);
  } finally {
    await reader.cancel().catch(() => {});
    reader.releaseLock();
  }
}
// JSON.parse rounds i64/u64 beyond Number.MAX_SAFE_INTEGER. Only station snapshots
// need lossless integer tokens; quote those lexemes before parsing, outside JSON strings.
const numberToken = /-?(?:0|[1-9]\d*)(?:\.\d+)?(?:[eE][+-]?\d+)?/y;
function preserveLargeIntegers(json: string): string {
  let result = '';
  let quoted = false;
  for (let index = 0; index < json.length;) {
    const char = json[index];
    if (char === '"') { quoted = !quoted; result += char; index++; continue; }
    if (quoted && char === '\\') { result += json.slice(index, index + 2); index += 2; continue; }
    if (!quoted && (char === '-' || (char >= '0' && char <= '9'))) {
      numberToken.lastIndex = index;
      const token = numberToken.exec(json)?.[0];
      if (token) {
        result += !/[.eE]/.test(token) && !Number.isSafeInteger(Number(token)) ? `"${token}"` : token;
        index += token.length;
        continue;
      }
    }
    result += char; index++;
  }
  return result;
}

export class ApiClient {
  private token = '';
  private readonly lifetime = new AbortController();
  private activeReads = 0;
  constructor(
    public readonly origin: string,
    public readonly identity: Identity,
    token: string,
    private readonly transport: typeof fetch = (input, init) => fetch(input, init),
  ) {
    const url = new URL(origin);
    const loopback = ['localhost', '127.0.0.1', '[::1]'].includes(url.hostname);
    if (url.origin !== origin || (url.protocol !== 'https:' && !(url.protocol === 'http:' && loopback))) {
      throw new ApiError(0, 'origin');
    }
    this.token = credential(token);
  }

  static async healthSnapshot(origin: string, identity: Identity, signal: AbortSignal, transport: typeof fetch = fetch) {
    const current = await ApiClient.identify(origin, signal, transport);
    if (identityKey(current) !== identityKey(identity)) throw new ApiError(0, 'identity');
    const response = await observedFetch(transport, `${origin}/api/v1/health`, {
      cache: 'no-store', credentials: 'omit', redirect: 'error',
      signal: AbortSignal.any([signal, AbortSignal.timeout(5000)]),
    });
    if (!response.ok && response.status !== 503) { await response.body?.cancel(); throw new ApiError(response.status); }
    const data = await readJson(response);
    const after = await ApiClient.identify(origin, signal, transport);
    if (identityKey(after) !== identityKey(identity)) throw new ApiError(0, 'identity');
    return data;
  }

  close() { this.token = ''; this.lifetime.abort(); }

  static async identify(origin: string, signal?: AbortSignal, transport: typeof fetch = fetch): Promise<Identity> {
    const response = await observedFetch(transport, `${origin}/api/v1/identity`, {
      cache: 'no-store', credentials: 'omit', redirect: 'error',
      signal: AbortSignal.any([AbortSignal.timeout(5000), ...(signal ? [signal] : [])]),
    });
    if (!response.ok) throw new ApiError(response.status);
    return parseIdentity(await readJson(response));
  }

  async verifyIdentity(signal?: AbortSignal) {
    const current = await ApiClient.identify(this.origin,
      AbortSignal.any([this.lifetime.signal, ...(signal ? [signal] : [])]), this.transport);
    if (identityKey(current) !== identityKey(this.identity)) {
      this.close();
      throw new ApiError(0, 'identity');
    }
  }

  private path(path: string): string {
    const url = new URL(path, this.origin);
    if (!path.startsWith('/api/v1/') || !url.pathname.startsWith('/api/v1/') || url.origin !== this.origin || url.hash || url.username || url.password) {
      throw new ApiError(0, 'origin');
    }
    return url.href;
  }

  get destinationKey(): string { return JSON.stringify([this.origin, identityKey(this.identity)]); }

  async request(path: string, options: RequestInit = {}, commandToken?: string, confirmedDestination?: string, exactIntegers = false): Promise<unknown> {
    if (!['GET', 'HEAD'].includes((options.method ?? 'GET').toUpperCase()) && confirmedDestination !== this.destinationKey) {
      throw new ApiError(0, 'destination');
    }
    const url = this.path(path);
    if (this.activeReads >= 2) throw new ApiError(429);
    this.activeReads++;
    try {
      const signal = AbortSignal.any([this.lifetime.signal, AbortSignal.timeout(5000), ...(options.signal ? [options.signal] : [])]);
      await this.verifyIdentity(signal);
      const response = await observedFetch(this.transport, url, {
        ...options, cache: 'no-store', credentials: 'omit', redirect: 'error', signal,
        headers: { Authorization: `Bearer ${commandToken === undefined ? this.token : credential(commandToken)}`,
          ...(options.body ? { 'Content-Type': 'application/json' } : {}) },
      });
      if (!response.ok) { await response.body?.cancel(); throw new ApiError(response.status); }
      if (response.status === 204) { await response.body?.cancel(); return undefined; }
      return await readJson(response, exactIntegers);
    } finally { this.activeReads--; }
  }

  async stations(after?: string) {
    const query = new URLSearchParams({ limit: '10' });
    if (after !== undefined) query.set('after', boundedText(after, 512));
    return parsePage(await this.request(`/api/v1/stations?${query}`, {}, undefined, undefined, true), this.identity.bridge_id);
  }

  async station(id: string) {
    const stationId = boundedText(id);
    const snapshot = parseStation(await this.request(`/api/v1/stations/${encodeURIComponent(stationId)}`, {}, undefined, undefined, true), this.identity.bridge_id);
    if (snapshot.station.station_id !== stationId) throw new ApiError(0, 'identity');
    return snapshot;
  }

  commandStatus(requestId: string) {
    return this.request(`/api/v1/commands/${encodeURIComponent(boundedText(requestId))}`);
  }

  // Explicit control credential; the read credential is never promoted to command authority.
  // Admission and result fields are passed through, never interpreted as physical success.
  async submitCommand(request: unknown, controlCredential: string, confirmedDestination: string) {
    const control = credential(controlCredential);
    const command = object(request);
    if (object(command.resource).bridge_id !== this.identity.bridge_id) throw new ApiError(0, 'identity');
    boundedText(command.request_id);
    boundedText(command.expires_at);
    const body = JSON.stringify(request);
    if (new TextEncoder().encode(body).length > 64 * 1024) throw new ApiError(0, 'limit');
    return this.request('/api/v1/commands', { method: 'POST', body }, control, confirmedDestination);
  }

  async openEvents(station: string, cursor: string | undefined, signal: AbortSignal) {
    const query = station ? `?station_id=${encodeURIComponent(boundedText(station))}` : '';
    return this.openStream(`/api/v1/events${query}`, cursor, signal);
  }

  async openStream(path: string, cursor: string | undefined, signal: AbortSignal) {
    const url = this.path(path);
    await this.verifyIdentity(signal);
    return observedFetch(this.transport, url, {
      cache: 'no-store', credentials: 'omit', redirect: 'error',
      signal: AbortSignal.any([signal, this.lifetime.signal]),
      headers: { Authorization: `Bearer ${this.token}`, Accept: 'text/event-stream',
        ...(cursor ? { 'Last-Event-ID': boundedText(cursor, 512) } : {}) },
    });
  }
}

function credential(value: string): string {
  if (typeof value !== 'string' || !/^[\x21-\x7e]{1,8000}$/.test(value)) throw new ApiError(401);
  return value;
}

async function observedFetch(transport: typeof fetch, url: string, options: RequestInit): Promise<Response> {
  const area = requestArea(new URL(url).pathname);
  diagnostics.request();
  try {
    const response = await transport(url, options);
    if (!response.ok) diagnostics.fail(area, response.status, response.headers.get('x-correlation-id') ?? undefined);
    return response;
  } catch (error) {
    const timeout = options.signal?.reason instanceof DOMException && options.signal.reason.name === 'TimeoutError';
    if (!options.signal?.aborted || timeout) diagnostics.fail(area, 0);
    throw error;
  }
}
