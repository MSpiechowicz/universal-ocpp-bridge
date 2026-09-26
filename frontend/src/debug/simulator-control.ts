import { ApiClient, ApiError, readJson } from '../http';
import { correlationId } from '../diagnostics/store';
import { boundedText, identityKey, object } from '../identity';
import type { Identity } from '../identity';
import { decimal, simulatorOrigin } from './simulator';

export type Intervention = { kind: 'disconnect' | 'reconnect' } | { kind: 'fault'; fault: string; delay_ms: number };
export type ScenarioStep = { id: string; station: string; action: string; controls: string[] };
export type Scenario = { id: string; seed: string; stations: string[]; steps: ScenarioStep[] };
export type RunEntry = { id: string; scenario: string; seed: string; terminal: boolean };
export type ControlStep = ScenarioStep & {
  status: string; expected?: string; actual?: string; passed?: boolean; fault?: string;
  selected?: boolean; intervention?: Intervention; failure?: string; category?: string;
  effectStatus?: string; responseDelayScope?: string; correlation?: string;
};
export type ControlRun = { id: string; environment: string; scenario: string; seed: string;
  status: 'running' | 'stopping' | 'passed' | 'failed'; steps: ControlStep[];
  failure?: { category: string; code: string };
};
const optional = (value: unknown) => value == null ? undefined : boundedText(value, 256);
const supported: Record<string, true> = { disconnect: true, reconnect: true, response_delay: true, missing_response: true, out_of_order_response: true };

export function controlSeed(value: string): string {
  return decimal(value);
}
function station(value: unknown, environment: string): string {
  const id = boundedText(value, 64);
  if (!id.startsWith(`${environment}-`)) throw new Error('Invalid station');
  return id;
}
function steps(value: unknown, environment: string): ScenarioStep[] {
  if (!Array.isArray(value) || value.length > 256) throw new Error('Invalid steps');
  const result = value.map(item => {
    const data = object(item);
    if (!Array.isArray(data.eligible_controls) || data.eligible_controls.length > 16) throw new Error('Invalid controls');
    return { id: boundedText(data.step_id, 64), station: station(data.station_id, environment),
      action: boundedText(data.action, 64), controls: data.eligible_controls.map(name => boundedText(name, 64)) as string[] };
  });
  if (new Set(result.map(step => step.id)).size !== result.length) throw new Error('Duplicate step');
  return result;
}
export function parseCatalog(value: unknown, environment: string): Scenario[] {
  const data = object(value);
  if (data.environment !== environment || !Array.isArray(data.scenarios) || data.scenarios.length > 16) throw new Error('Invalid catalog');
  const result = data.scenarios.map(item => {
    const scenario = object(item);
    if (!Array.isArray(scenario.stations) || scenario.stations.length > 64) throw new Error('Invalid stations');
    const authored = steps(scenario.steps, environment);
    const stations = scenario.stations.map(id => station(id, environment));
    if (new Set(stations).size !== stations.length || authored.some(step => !stations.includes(step.station))) throw new Error('Invalid station set');
    return { id: boundedText(scenario.id, 64), seed: decimal(scenario.seed), stations, steps: authored };
  });
  if (new Set(result.map(scenario => scenario.id)).size !== result.length) throw new Error('Duplicate scenario');
  return result;
}
export function parseRunList(value: unknown, environment: string): RunEntry[] {
  const data = object(value);
  if (data.environment !== environment || !Array.isArray(data.runs) || data.runs.length > 128) throw new Error('Invalid runs');
  return data.runs.map(value => {
    const run = object(value);
    if (typeof run.terminal !== 'boolean') throw new Error('Invalid terminal');
    return { id: decimal(run.run_id), scenario: boundedText(run.scenario, 64), seed: decimal(run.seed), terminal: run.terminal };
  });
}
export function parseControlRun(value: unknown, environment: string, runId: string): ControlRun {
  const data = object(value);
  if (data.environment !== environment || decimal(data.run_id) !== runId || !['running', 'stopping', 'passed', 'failed'].includes(String(data.status))) throw new Error('Invalid run');
  const authored = steps(data.steps, environment);
  const rows = data.steps as unknown[];
  const parsed = authored.map((step, index) => {
    const row = object(rows[index]);
    if (!['pending', 'preparing', 'running', 'passed', 'failed'].includes(String(row.status))) throw new Error('Invalid step state');
    const intervention = row.intervention == null ? undefined : object(row.intervention);
    let selectedIntervention: Intervention | undefined;
    if (intervention) {
      if (intervention.kind === 'disconnect' || intervention.kind === 'reconnect') selectedIntervention = { kind: intervention.kind };
      else if (intervention.kind === 'fault') selectedIntervention = { kind: 'fault', fault: boundedText(intervention.fault, 64), delay_ms: delay(intervention.delay_ms) };
      else throw new Error('Invalid intervention');
    }
    if (row.assertion_passed != null && typeof row.assertion_passed !== 'boolean') throw new Error('Invalid assertion');
    if (row.fault_selected != null && typeof row.fault_selected !== 'boolean') throw new Error('Invalid selection');
    return { ...step, status: row.status as string, expected: optional(row.expectation), actual: optional(row.actual_event),
      passed: row.assertion_passed as boolean | undefined, fault: optional(row.fault), selected: row.fault_selected as boolean | undefined,
      intervention: selectedIntervention, failure: optional(row.failure_code), category: optional(row.failure_category),
      effectStatus: optional(row.effect_status), responseDelayScope: optional(row.response_delay_scope),
      correlation: correlationId(row.correlation_id) };
  });
  const failure = data.failure == null ? undefined : object(data.failure);
  return { id: runId, environment, scenario: boundedText(data.scenario, 64), seed: decimal(data.seed),
    status: data.status as ControlRun['status'], steps: parsed,
    failure: failure ? { category: boundedText(failure.category, 64), code: boundedText(failure.code, 128) } : undefined };
}
function delay(value: unknown): number {
  if (typeof value !== 'number' || !Number.isInteger(value) || value < 0 || value > 30000) throw new Error('Invalid delay');
  return value;
}
export function availableControls(step: ControlStep): string[] {
  return step.status === 'pending' && !step.intervention ? step.controls.filter(name => Object.hasOwn(supported, name)) : [];
}

export class SimulatorController {
  private token: string;
  private lifetime = new AbortController();
  private pending = false;
  readonly origin: string;
  readonly environment: string;
  constructor(origin: string, token: string, private identity: Identity, private transport: typeof fetch = (input, init) => fetch(input, init)) {
    if (!['demo', 'staging'].includes(identity.runtime.environment) || !/^[a-f0-9]{64}$/i.test(token)) throw new Error('Invalid control access');
    this.origin = simulatorOrigin(origin);
    this.environment = identity.runtime.environment;
    this.token = token;
  }
  close() { this.token = ''; this.lifetime.abort(); }
  get closed() { return this.lifetime.signal.aborted; }
  private async request(bridgeOrigin: string, path: string, method: 'GET' | 'POST' | 'DELETE' = 'GET', body?: object): Promise<unknown> {
    if (this.pending || this.lifetime.signal.aborted) throw new Error('Controller unavailable');
    this.pending = true;
    const signal = AbortSignal.any([this.lifetime.signal, AbortSignal.timeout(5000)]);
    const verify = async () => {
      if (identityKey(await ApiClient.identify(bridgeOrigin, signal, this.transport)) !== identityKey(this.identity)) {
        this.close(); throw new ApiError(0, 'identity');
      }
    };
    try {
      await verify();
      const response = await this.transport(`${this.origin}${path}`, {
        method, body: body && JSON.stringify(body), credentials: 'omit', redirect: 'error', cache: 'no-store', signal,
        headers: { Authorization: `Bearer ${this.token}`, ...(body ? { 'Content-Type': 'application/json' } : {}) },
      });
      if (!response.ok) { await response.body?.cancel(); throw new ApiError(response.status); }
      const result = await readJson(response);
      await verify();
      return result;
    } finally { this.pending = false; }
  }
  catalog(bridge: string) { return this.request(bridge, '/api/v1/scenarios').then(value => parseCatalog(value, this.environment)); }
  list(bridge: string) { return this.request(bridge, '/api/v1/runs').then(value => parseRunList(value, this.environment)); }
  status(bridge: string, id: string) { const run = decimal(id); return this.request(bridge, `/api/v1/runs/${run}`).then(value => parseControlRun(value, this.environment, run)); }
  async start(bridge: string, scenario: Scenario, seed?: string): Promise<string> {
    const value = object(await this.request(bridge, '/api/v1/runs', 'POST', { scenario: scenario.id, ...(seed === undefined ? {} : { seed: decimal(seed) }) }));
    return decimal(value.run_id);
  }
  async stop(bridge: string, id: string) {
    const run = decimal(id);
    const response = object(await this.request(bridge, `/api/v1/runs/${run}/stop`, 'POST'));
    if (decimal(response.run_id) !== run || response.stop_requested !== true) throw new Error('Unconfirmed stop');
  }
  async remove(bridge: string, id: string) {
    const run = decimal(id);
    const response = object(await this.request(bridge, `/api/v1/runs/${run}`, 'DELETE'));
    if (decimal(response.removed) !== run) throw new Error('Unconfirmed removal');
  }
  async intervene(bridge: string, runId: string, step: ControlStep, intervention: Intervention) {
    const run = decimal(runId);
    const name = intervention.kind === 'fault' ? intervention.fault : intervention.kind;
    if (!availableControls(step).includes(name) || (intervention.kind !== 'fault' && step.action !== 'wait')
      || (intervention.kind === 'fault' && step.action === 'wait')) throw new Error('Control unavailable');
    if (intervention.kind === 'fault') {
      const milliseconds = delay(intervention.delay_ms);
      if ((name === 'response_delay' || name === 'out_of_order_response') && milliseconds === 0) throw new Error('Invalid delay');
    }
    const response = object(await this.request(bridge, `/api/v1/runs/${run}/controls`, 'POST', { step_id: step.id, intervention }));
    if (decimal(response.run_id) !== run || response.step_id !== step.id || response.status !== 'scheduled') throw new Error('Unconfirmed schedule');
  }
}
