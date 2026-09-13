import { ApiClient, ApiError, readJson } from '../http';
import { boundedText, identityKey, object } from '../identity';
import type { Identity } from '../identity';
import { correlationId } from '../diagnostics/store';

const optional = (value: unknown) => value == null ? undefined : boundedText(value, 256);
function decimal(value: unknown): string {
  if (typeof value !== 'string' || !/^(0|[1-9][0-9]{0,19})$/.test(value) || BigInt(value) > 18446744073709551615n) throw new Error('Invalid integer');
  return value;
}
function triState(value: unknown): boolean | undefined {
  if (value == null) return undefined;
  if (typeof value !== 'boolean') throw new Error('Invalid observation');
  return value;
}
export function simulatorOrigin(value: string) {
  const url = new URL(value);
  if (url.origin !== value || url.protocol !== 'http:' || url.hostname !== '127.0.0.1') throw new Error('Invalid simulator origin');
  return value;
}
export function parseSimulator(value: unknown, environment: string, run: string) {
  const data = object(value);
  if (!['demo', 'staging'].includes(environment) || data.environment !== environment || data.schema_version !== 1 || decimal(data.run_id) !== run) throw new Error('Invalid simulator identity');
  if (!Array.isArray(data.steps) || data.steps.length > 256 || !Array.isArray(data.events) || data.events.length > 770) throw new Error('Evidence limit');
  const steps = data.steps.map(value => {
    const step = object(value);
    const intervention = step.intervention == null ? undefined : object(step.intervention);
    const station = boundedText(step.station_id, 64);
    if (!station.startsWith(`${environment}-`)) throw new Error('Invalid station');
    return {
      id: boundedText(step.step_id, 64), station, action: boundedText(step.action, 64), status: boundedText(step.status, 32),
      expected: optional(step.expectation), actual: optional(step.actual_event), passed: triState(step.assertion_passed),
      detailAssertion: triState(step.detail_assertion), failure: optional(step.failure_code), category: optional(step.failure_category),
      fault: optional(step.fault), selected: triState(step.fault_selected),
      intervention: intervention ? boundedText(intervention.kind, 64) : undefined,
      interventionFault: intervention ? optional(intervention.fault) : undefined,
      delay: intervention && typeof intervention.delay_ms === 'number' && Number.isInteger(intervention.delay_ms) && intervention.delay_ms >= 0 && intervention.delay_ms <= 30000 ? intervention.delay_ms : undefined,
      correlation: correlationId(step.correlation_id),
    };
  });
  if (new Set(steps.map(step => step.id)).size !== steps.length) throw new Error('Duplicate step');
  const events = data.events.map(value => {
    const event = object(value);
    return { id: boundedText(event.id, 128), event: boundedText(event.event, 64), status: boundedText(event.status, 32),
      step: optional(event.step_id), failure: optional(event.failure_code), category: optional(event.failure_category), correlation: correlationId(event.correlation_id) };
  });
  return { run, environment, scenario: boundedText(data.scenario, 64), seed: decimal(data.seed), status: boundedText(data.status, 32), steps, events };
}
export type SimulatorEvidence = ReturnType<typeof parseSimulator>;

export class SimulatorReader {
  private token: string;
  private lifetime = new AbortController();
  private pending = false;
  readonly origin: string;
  readonly run: string;
  constructor(origin: string, run: string, token: string, private identity: Identity, private transport: typeof fetch = (input, init) => fetch(input, init)) {
    if (!['demo', 'staging'].includes(identity.runtime.environment) || !/^[a-f0-9]{64}$/i.test(token)) throw new Error('Invalid access');
    this.origin = simulatorOrigin(origin); this.run = decimal(run); this.token = token;
  }
  close() { this.token = ''; this.lifetime.abort(); }
  async read(bridgeOrigin: string): Promise<SimulatorEvidence> {
    if (this.pending || this.lifetime.signal.aborted) throw new Error('Reader unavailable');
    this.pending = true;
    const signal = AbortSignal.any([this.lifetime.signal, AbortSignal.timeout(5000)]);
    const verify = async () => {
      if (identityKey(await ApiClient.identify(bridgeOrigin, signal, this.transport)) !== identityKey(this.identity)) {
        this.close(); throw new ApiError(0, 'identity');
      }
    };
    try {
      await verify();
      const response = await this.transport(`${this.origin}/api/v1/debug/runs/${this.run}`, {
        method: 'GET', credentials: 'omit', redirect: 'error', cache: 'no-store', signal,
        headers: { Authorization: `Bearer ${this.token}` },
      });
      if (!response.ok) { await response.body?.cancel(); throw new ApiError(response.status); }
      const data = parseSimulator(await readJson(response), this.identity.runtime.environment, this.run);
      await verify();
      return data;
    } finally { this.pending = false; }
  }
}
