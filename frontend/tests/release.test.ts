import { test } from 'node:test';
import assert from 'node:assert/strict';
import { parseReleaseEvents, parseReleaseStatus } from '../src/debug/release';

const candidate = 'a'.repeat(64);
const previousGood = 'b'.repeat(64);
const evidence = 'c'.repeat(64);
const configuration = 'd'.repeat(64);

function status(activation: unknown = {
  sequence: 42,
  production: { digest: previousGood, phase: 'previous-good' },
  previous_good: previousGood,
  candidate: { digest: candidate, phase: 'quarantined' },
}) {
  return {
    protocol: 1, manager_version: '0.28.0', code: 'ok', activation,
    status: {
      sequence: 91, failed_operations: 1, staged_verified_digest: candidate, qualification: null,
      promotion: null, probation: null, failures: {
        decision: 'rollback_required', last: observation(), trigger: { observation: observation() },
      },
      rollback: { trigger_id: 17, quarantined_digest: candidate, previous_good: previousGood, step: 'restored', reason: 'eligible_failure' },
    },
  };
}

function observation() {
  return { id: 17, at_seconds: 1710000300, signal: { kind: 'watchdog' }, resource_pressure: false };
}

function events() {
  return {
    protocol: 1, manager_version: '0.28.0', code: 'ok',
    events: {
      oldest_sequence: 7, latest_sequence: 9, truncated: false,
      records: [
        {
          sequence: 7, uid: 1000, request: { operation: 'promote', digest: candidate }, result: 'ok', actor: 'supervisor',
          decision: { kind: 'promote', candidate_digest: candidate, previous_good_digest: previousGood, evidence_digest: evidence, configuration_digest: configuration, compatibility: 'accepted', drain: 'granted', health: 'probation', outcome: 'continuing' },
        },
        {
          sequence: 8, uid: 0, request: { operation: 'rollback' }, result: 'ok', actor: 'supervisor',
          decision: { kind: 'failure', candidate_digest: candidate, previous_good_digest: previousGood, observation: observation(), decision: 'rollback_required', trigger_id: 17 },
        },
        {
          sequence: 9, uid: 0, request: { operation: 'rollback' }, result: 'ok', actor: 'supervisor',
          decision: { kind: 'rollback', quarantined_digest: candidate, previous_good_digest: previousGood, step: 'restored', reason: 'eligible_failure' },
        },
      ],
    },
  };
}

test('release evidence excludes unrecognized private fields while retaining public failure evidence', () => {
  const input = status();
  const snapshot = parseReleaseStatus({
    ...input,
    status: { ...input.status, private_path: '/private/secret', credential: 'private-secret' },
  });
  assert.equal(snapshot.status.failures?.last?.signal, 'watchdog');
  assert.equal(snapshot.status.rollback?.reason, 'eligible_failure');
  assert.ok(!JSON.stringify(snapshot).includes('private'));
});

test('release audit rejects overflow, reordering and promotion evidence for a different artifact', () => {
  const oversized = events();
  oversized.events.records = Array(65).fill(oversized.events.records[0]);
  assert.throws(() => parseReleaseEvents(oversized));
  const reordered = events();
  reordered.events.records.reverse();
  assert.throws(() => parseReleaseEvents(reordered));
  const mismatched = events();
  mismatched.events.records[0].request.digest = previousGood;
  assert.throws(() => parseReleaseEvents(mismatched));
});

test('missing activation remains unavailable rather than inferred from persisted promotion and invalid pointers are rejected', () => {
  const { activation: _activation, ...withoutActivation } = status();
  const missingActivation = {
    ...withoutActivation,
    status: {
      ...withoutActivation.status,
      promotion: {
        candidate, previous: previousGood, configuration_digest: configuration, production_inputs_digest: evidence,
        database_device: 2049, database_inode: 1048577, step: 'probation', recovery_attempted: false,
      },
    },
  };
  assert.equal(parseReleaseStatus(missingActivation).activation, undefined);

  const invalidActivation = status({
    sequence: 42,
    production: { digest: previousGood.toUpperCase(), phase: 'previous-good' },
    previous_good: previousGood,
    candidate: { digest: candidate, phase: 'quarantined' },
  });
  assert.throws(() => parseReleaseStatus(invalidActivation), /Invalid release evidence/);
});

test('missing probation measurements remain unavailable, never passed or an unreadable incident', () => {
  const input = status();
  const snapshot = parseReleaseStatus({
    ...input,
    code: 'recovery_required',
    status: { ...input.status, probation: {
      policy: { required_seconds: 86400, maximum_sample_gap_seconds: 60, profile_digest: evidence },
      started_unix_seconds: 100, verified_seconds: 0, interrupted_intervals: 1,
      last: { id: 1, unix_seconds: 100, uptime_seconds: 10, invocation: evidence,
        candidate, configuration_digest: configuration, checks: { readiness: false } },
    } },
  });
  assert.equal(snapshot.code, 'recovery_required');
  assert.equal(snapshot.status.probation?.observation.checks.readiness, false);
  assert.equal(snapshot.status.probation?.observation.checks.memory_budget, undefined);
  assert.equal(snapshot.status.rollback?.reason, 'eligible_failure');
});
