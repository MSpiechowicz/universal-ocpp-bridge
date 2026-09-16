import { object } from '../identity';

export const releaseStatusPath = '/api/v1/release/status';
export const releaseEventsPath = '/api/v1/release/events?after=0';

const DIGEST = /^[a-f0-9]{64}$/;
const PHASES = ['installed', 'staging', 'qualified', 'promoting', 'probation', 'healthy', 'quarantined', 'rolling-back', 'previous-good'] as const;
const CODES = ['ok', 'recovery_required'] as const;
const PROMOTION_STEPS = ['stopping', 'starting', 'probation', 'recovered_previous', 'recovery_required'] as const;
const ROLLBACK_STEPS = ['attempting', 'restored', 'recovery_required'] as const;
const ROLLBACK_REASONS = ['eligible_failure', 'no_previous_good', 'eligibility_rejected', 'process_failed'] as const;
const FAILURE_DECISIONS = ['observe', 'degraded', 'stop_staging', 'recheck_after_staging', 'rollback_required', 'recovery_required'] as const;
const SIGNALS = ['started', 'core_ready', 'startup_pending', 'internal_readiness_failure', 'exit', 'watchdog', 'oom', 'fatal_invariant', 'mqtt_outage', 'ems_outage', 'external_database_outage', 'credential_rejection', 'malformed_charger_traffic', 'no_chargers', 'corrupt_storage', 'full_storage', 'os_kernel_failure', 'both_versions_failed'] as const;
const CHECKS = ['core_progress', 'storage_progress', 'readiness', 'memory_budget', 'cpu_budget', 'response_latency'] as const;

type Phase = typeof PHASES[number];
type PromotionStep = typeof PROMOTION_STEPS[number];
type RollbackStep = typeof ROLLBACK_STEPS[number];
type RollbackReason = typeof ROLLBACK_REASONS[number];
type FailureDecision = typeof FAILURE_DECISIONS[number];

export interface ActivationState {
  production: ReleasePointer | undefined;
  previousGood: string | undefined;
  candidate: ReleasePointer | undefined;
}
export interface ReleasePointer { digest: string; phase: Phase; }
export interface Qualification { candidate: string; evidence: string; piMeasurements: string | undefined; }
export interface Promotion { candidate: string; configuration: string; inputs: string; device: number; inode: number; step: PromotionStep; recoveryAttempted: boolean; }
export interface Rollback { quarantined: string | undefined; previousGood: string | undefined; step: RollbackStep; reason: RollbackReason; }
export interface Probation {
  requiredSeconds: number; maximumGapSeconds: number; profile: string; verified: number;
  interruptions: number; observation: { at: number; candidate: string; configuration: string; checks: Record<typeof CHECKS[number], boolean | undefined> };
}
export interface Failure { decision: FailureDecision; last: FailureObservation | undefined; }
export interface FailureObservation { signal: typeof SIGNALS[number]; }
export interface ReleaseStatus {
  qualification: Qualification | undefined;
  promotion: Promotion | undefined; rollback: Rollback | undefined; probation: Probation | undefined; failures: Failure | undefined;
}
export interface ReleaseAudit {
  sequence: number; uid: number; actor: 'operator' | 'supervisor'; operation: 'stage' | 'qualify' | 'promote' | 'rollback'; result: string;
  digest: string | undefined; evidence: string | undefined; decision: PromotionDecision | FailureDecisionRecord | RollbackDecision | undefined;
}
export interface PromotionDecision { kind: 'promote'; candidate: string; previousGood: string | undefined; evidence: string | undefined; configuration: string | undefined; compatibility: string; drain: string; health: string; outcome: string; }
export interface FailureDecisionRecord { kind: 'failure'; candidate: string | undefined; previousGood: string | undefined; decision: FailureDecision; observation: FailureObservation; }
export interface RollbackDecision { kind: 'rollback'; quarantined: string | undefined; previousGood: string | undefined; step: RollbackStep; reason: RollbackReason; }
export interface ReleaseSnapshot { code: 'ok' | 'recovery_required'; status: ReleaseStatus; activation: ActivationState | undefined; audit: ReleaseAudit[]; oldestSequence: number; latestSequence: number; truncated: boolean; }

export function parseReleaseStatus(value: unknown): Omit<ReleaseSnapshot, 'audit' | 'oldestSequence' | 'latestSequence' | 'truncated'> {
  const data = object(value);
  envelope(data, true);
  return { code: closed(data.code, CODES), status: parseStatus(object(data.status)), activation: data.activation === undefined ? undefined : parseActivation(data.activation) };
}

export function parseReleaseEvents(value: unknown): Pick<ReleaseSnapshot, 'audit' | 'oldestSequence' | 'latestSequence' | 'truncated'> {
  const data = object(value);
  envelope(data, false);
  if (data.code !== 'ok') invalid();
  const events = object(data.events);
  const oldestSequence = number(events.oldest_sequence);
  const latestSequence = number(events.latest_sequence);
  if (!((oldestSequence === 0 && latestSequence === 0) || (oldestSequence > 0 && oldestSequence <= latestSequence)) || typeof events.truncated !== 'boolean' || !Array.isArray(events.records) || events.records.length > 64) invalid();
  const audit = events.records.map(parseAudit);
  if (audit.some((record, index) => record.sequence < oldestSequence || record.sequence > latestSequence || (index > 0 && audit[index - 1].sequence >= record.sequence))) invalid();
  return { audit, oldestSequence, latestSequence, truncated: events.truncated };
}

function envelope(data: Record<string, unknown>, status: boolean) {
  if (number(data.protocol) !== 1 || typeof data.code !== 'string' || status !== (data.status !== undefined) || !status && data.events === undefined) invalid();
}
function parseActivation(value: unknown): ActivationState {
  const state = object(value);
  return { production: pointer(state.production), previousGood: optionalDigest(state.previous_good), candidate: pointer(state.candidate) };
}
function pointer(value: unknown): ReleasePointer | undefined {
  if (value == null) return undefined;
  const release = object(value);
  return { digest: digest(release.digest), phase: closed(release.phase, PHASES) };
}
function parseStatus(value: Record<string, unknown>): ReleaseStatus {
  return {
    qualification: value.qualification == null ? undefined : qualification(value.qualification),
    promotion: value.promotion == null ? undefined : promotion(value.promotion), rollback: value.rollback == null ? undefined : rollback(value.rollback),
    probation: value.probation == null ? undefined : probation(value.probation), failures: value.failures == null ? undefined : failures(value.failures),
  };
}
function qualification(value: unknown): Qualification {
  const item = object(value);
  return { candidate: digest(item.candidate_digest), evidence: digest(item.evidence_digest), piMeasurements: optionalDigest(item.pi_measurements_digest) };
}
function promotion(value: unknown): Promotion {
  const item = object(value);
  return { candidate: digest(item.candidate), configuration: digest(item.configuration_digest), inputs: digest(item.production_inputs_digest), device: number(item.database_device), inode: number(item.database_inode), step: closed(item.step, PROMOTION_STEPS), recoveryAttempted: boolean(item.recovery_attempted) };
}
function rollback(value: unknown): Rollback {
  const item = object(value);
  return { quarantined: optionalDigest(item.quarantined_digest), previousGood: optionalDigest(item.previous_good), step: closed(item.step, ROLLBACK_STEPS), reason: closed(item.reason, ROLLBACK_REASONS) };
}
function probation(value: unknown): Probation {
  const item = object(value); const policy = object(item.policy); const last = object(item.last); const source = object(last.checks);
  const checks = {} as Probation['observation']['checks'];
  for (const check of CHECKS) checks[check] = source[check] === undefined ? undefined : boolean(source[check]);
  return { requiredSeconds: positive(policy.required_seconds), maximumGapSeconds: positive(policy.maximum_sample_gap_seconds), profile: digest(policy.profile_digest), verified: number(item.verified_seconds), interruptions: number(item.interrupted_intervals), observation: { at: number(last.unix_seconds), candidate: digest(last.candidate), configuration: digest(last.configuration_digest), checks } };
}
function failures(value: unknown): Failure {
  const item = object(value);
  return { decision: closed(item.decision, FAILURE_DECISIONS), last: observation(item.last) };
}
function observation(value: unknown): FailureObservation | undefined {
  if (value == null) return undefined;
  const item = object(value); const signal = object(item.signal);
  return { signal: closed(signal.kind, SIGNALS) };
}
function parseAudit(value: unknown): ReleaseAudit {
  const item = object(value); const request = object(item.request); const operation = closed(request.operation, ['stage', 'qualify', 'promote', 'rollback'] as const);
  const decision = item.decision == null ? undefined : auditDecision(item.decision);
  const digestValue = operation === 'rollback' ? undefined : digest(request.digest);
  const evidence = operation === 'qualify' ? digest(request.evidence_digest) : undefined;
  if (typeof item.result !== 'string' || !['ok', 'forbidden', 'invalid_request', 'busy', 'artifact_rejected', 'qualification_required', 'evidence_rejected', 'preflight_rejected', 'activation_blocked', 'recovery_required', 'storage_failure'].includes(item.result) || (item.actor !== 'operator' && item.actor !== 'supervisor') || (item.actor === 'supervisor') !== (decision !== undefined) || (decision?.kind === 'promote' && operation !== 'promote') || ((decision?.kind === 'failure' || decision?.kind === 'rollback') && operation !== 'rollback')) invalid();
  const uid = number(item.uid);
  if (uid > 4294967295 || decision?.kind === 'promote' && decision.candidate !== digestValue) invalid();
  return { sequence: positive(item.sequence), uid, actor: item.actor, operation, result: item.result, digest: digestValue, evidence, decision };
}
function auditDecision(value: unknown): PromotionDecision | FailureDecisionRecord | RollbackDecision {
  const item = object(value);
  if (item.kind === 'promote') return { kind: 'promote', candidate: digest(item.candidate_digest), previousGood: optionalDigest(item.previous_good_digest), evidence: optionalDigest(item.evidence_digest), configuration: optionalDigest(item.configuration_digest), compatibility: closed(item.compatibility, ['not_checked', 'accepted', 'rejected'] as const), drain: closed(item.drain, ['not_requested', 'granted', 'rejected'] as const), health: closed(item.health, ['not_observed', 'probation', 'healthy', 'rejected'] as const), outcome: closed(item.outcome, ['continuing', 'rejected', 'recovery_required'] as const) };
  if (item.kind === 'failure') {
    const failed = observation(item.observation);
    if (!failed) invalid();
    return { kind: 'failure', candidate: optionalDigest(item.candidate_digest), previousGood: optionalDigest(item.previous_good_digest), decision: closed(item.decision, FAILURE_DECISIONS), observation: failed };
  }
  if (item.kind === 'rollback') return { kind: 'rollback', quarantined: optionalDigest(item.quarantined_digest), previousGood: optionalDigest(item.previous_good_digest), step: closed(item.step, ROLLBACK_STEPS), reason: closed(item.reason, ROLLBACK_REASONS) };
  invalid();
}
function digest(value: unknown): string { if (typeof value !== 'string' || !DIGEST.test(value)) invalid(); return value; }
function optionalDigest(value: unknown): string | undefined { return value == null ? undefined : digest(value); }
function number(value: unknown): number { if (!Number.isSafeInteger(value) || (value as number) < 0) invalid(); return value as number; }
function positive(value: unknown): number { const parsed = number(value); if (!parsed) invalid(); return parsed; }
function boolean(value: unknown): boolean { if (typeof value !== 'boolean') invalid(); return value; }
function closed<T extends readonly string[]>(value: unknown, allowed: T): T[number] { if (typeof value !== 'string' || !allowed.includes(value)) invalid(); return value as T[number]; }
function invalid(): never { throw new Error('Invalid release evidence'); }
