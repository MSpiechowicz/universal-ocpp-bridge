import { boundedText, identityKey, object, parseIdentity } from './identity';
import type { Identity } from './identity';

export class ApiError extends Error {
  constructor(public readonly status: number, public readonly kind = 'request') {
    super(kind === 'identity' ? 'Service identity changed. Disconnect and authenticate again.'
      : status === 401 || status === 403 ? 'Access denied. Check the credential and resource scope.'
      : status === 503 ? 'Management data is unavailable on this service configuration.'
      : status === 429 ? 'Service is busy. Retrying with a delay.'
      : 'Request failed. Data may be stale.');
  }
}

export async function readJson(response: Response): Promise<unknown> {
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
    return JSON.parse(new TextDecoder('utf-8', { fatal: true }).decode(bytes.subarray(0, length)));
  } finally {
    await reader.cancel().catch(() => {});
    reader.releaseLock();
  }
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

  close() { this.token = ''; this.lifetime.abort(); }

  static async identify(origin: string, signal?: AbortSignal, transport: typeof fetch = fetch): Promise<Identity> {
    const response = await transport(`${origin}/api/v1/identity`, {
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

  async request(path: string, options: RequestInit = {}, commandToken?: string): Promise<unknown> {
    const url = this.path(path);
    if (this.activeReads >= 2) throw new ApiError(429);
    this.activeReads++;
    try {
      const signal = AbortSignal.any([this.lifetime.signal, AbortSignal.timeout(5000), ...(options.signal ? [options.signal] : [])]);
      await this.verifyIdentity(signal);
      const response = await this.transport(url, {
        ...options, cache: 'no-store', credentials: 'omit', redirect: 'error', signal,
        headers: { Authorization: `Bearer ${commandToken === undefined ? this.token : credential(commandToken)}`,
          ...(options.body ? { 'Content-Type': 'application/json' } : {}) },
      });
      if (!response.ok) { await response.body?.cancel(); throw new ApiError(response.status); }
      return await readJson(response);
    } finally { this.activeReads--; }
  }

  async stations() {
    const page = object(await this.request('/api/v1/stations?limit=10'));
    if (!Array.isArray(page.items) || page.items.length > 10) throw new ApiError(0);
    return { count: page.items.length, more: typeof page.next_cursor === 'string' };
  }

  station(id: string) {
    return this.request(`/api/v1/stations/${encodeURIComponent(boundedText(id))}`);
  }

  commandStatus(requestId: string) {
    return this.request(`/api/v1/commands/${encodeURIComponent(boundedText(requestId))}`);
  }

  // Explicit control credential; the read credential is never promoted to command authority.
  // Admission and result fields are passed through, never interpreted as physical success.
  async submitCommand(request: unknown, controlCredential: string) {
    const control = credential(controlCredential);
    const command = object(request);
    if (object(command.resource).bridge_id !== this.identity.bridge_id) throw new ApiError(0, 'identity');
    boundedText(command.request_id);
    boundedText(command.expires_at);
    const body = JSON.stringify(request);
    if (new TextEncoder().encode(body).length > 64 * 1024) throw new ApiError(0, 'limit');
    return this.request('/api/v1/commands', { method: 'POST', body }, control);
  }

  async openEvents(station: string, cursor: string | undefined, signal: AbortSignal) {
    await this.verifyIdentity(signal);
    const query = station ? `?station_id=${encodeURIComponent(boundedText(station))}` : '';
    return this.transport(`${this.origin}/api/v1/events${query}`, {
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
