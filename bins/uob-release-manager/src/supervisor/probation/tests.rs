use super::*;
use crate::test_support as support;
use crate::{
    artifacts::ArtifactStore,
    supervisor::{Request, promotion::Record},
};
use std::fs;
use support::Fixture;

fn policy() -> Policy {
    Policy {
        required_seconds: MINIMUM_SECONDS,
        maximum_sample_gap_seconds: 300,
        profile_digest: "a".repeat(64),
    }
}
fn sample(f: &Fixture, id: u64) -> Observation {
    Observation {
        id,
        unix_seconds: 1_000_000 + id * 300,
        uptime_seconds: id * 300,
        invocation: "b".repeat(64),
        candidate: f.artifact.digest().into(),
        configuration_digest: "c".repeat(64),
        checks: Check::REQUIRED.into_iter().map(|c| (c, true)).collect(),
    }
}
fn setup() -> (Fixture, Supervisor) {
    let f = Fixture::new();
    let mut manager = f.manager();
    for transition in [
        Transition::BeginStaging,
        Transition::Qualify,
        Transition::BeginPromotion,
        Transition::BeginProbation,
    ] {
        manager
            .activation
            .transition(transition, &f.artifact.policy)
            .unwrap();
    }
    manager
        .ledger
        .record_promotion(Record {
            candidate: f.artifact.digest().into(),
            previous: f.previous.compatibility.artifact_digest.as_str().into(),
            configuration_digest: "c".repeat(64),
            production_inputs_digest: "d".repeat(64),
            database_device: 1,
            database_inode: 2,
            step: Step::Probation,
            recovery_attempted: false,
        })
        .unwrap();
    (f, manager)
}
fn evidence(manager: &mut Supervisor) -> State {
    manager
        .handle(100, Request::Status {})
        .status
        .unwrap()
        .probation
        .unwrap()
}
fn retained(f: &Fixture, manager: &Supervisor, phase: Phase) {
    assert_eq!(
        manager
            .activation
            .state()
            .production
            .as_ref()
            .unwrap()
            .phase,
        phase
    );
    assert_eq!(
        fs::read_to_string(f.store.join("previous-good")).unwrap(),
        f.previous.compatibility.artifact_digest.as_str()
    );
    ArtifactStore::open(&f.store)
        .unwrap()
        .verify_installed(
            f.previous.compatibility.artifact_digest.as_str(),
            &f.artifact.policy,
        )
        .unwrap();
}

#[test]
fn full_day_of_all_checks_advances_and_retains_previous_across_restart() {
    let _serial = crate::TEST_SERIAL.lock().unwrap();
    let (f, mut manager) = setup();
    assert!(
        manager
            .handle(100, Request::Status {})
            .status
            .unwrap()
            .probation
            .is_none()
    );
    for id in 1..=288 {
        assert!(!manager.observe_probation(policy(), sample(&f, id)).unwrap());
    }
    retained(&f, &manager, Phase::Probation);
    assert_eq!(evidence(&mut manager).verified_seconds, 86100);
    assert!(
        manager
            .observe_probation(policy(), sample(&f, 289))
            .unwrap()
    );
    retained(&f, &manager, Phase::Healthy);
    drop(manager);
    let mut manager = f.manager();
    assert!(evidence(&mut manager).complete());
    retained(&f, &manager, Phase::Healthy);
}

#[test]
fn every_missing_or_failed_check_resets_progress_after_nominal_day() {
    let _serial = crate::TEST_SERIAL.lock().unwrap();
    let (f, mut manager) = setup();
    let mut id = 0;
    for check in Check::REQUIRED {
        for missing in [false, true] {
            for _ in 0..2 {
                id += 1;
                assert!(!manager.observe_probation(policy(), sample(&f, id)).unwrap());
            }
            id += 300;
            let mut bad = sample(&f, id);
            if missing {
                bad.checks.remove(&check);
            } else {
                bad.checks.insert(check, false);
            }
            assert!(!manager.observe_probation(policy(), bad).unwrap());
            assert_eq!(evidence(&mut manager).verified_seconds, 0);
            retained(&f, &manager, Phase::Probation);
        }
    }
}

#[test]
fn restart_preserves_committed_progress_but_never_credits_unobserved_time() {
    let _serial = crate::TEST_SERIAL.lock().unwrap();
    let (f, mut manager) = setup();
    for id in 1..=10 {
        manager.observe_probation(policy(), sample(&f, id)).unwrap();
    }
    assert_eq!(evidence(&mut manager).verified_seconds, 2700);
    drop(manager);
    let mut manager = f.manager();
    manager
        .observe_probation(policy(), sample(&f, 1000))
        .unwrap();
    assert_eq!(evidence(&mut manager).verified_seconds, 2700);
    manager
        .observe_probation(policy(), sample(&f, 1001))
        .unwrap();
    assert_eq!(evidence(&mut manager).verified_seconds, 3000);
    drop(manager);
    let mut manager = f.manager();
    let mut rebooted = sample(&f, 1002);
    rebooted.invocation = "e".repeat(64);
    rebooted.uptime_seconds = 1;
    manager.observe_probation(policy(), rebooted).unwrap();
    assert_eq!(evidence(&mut manager).verified_seconds, 0);
    retained(&f, &manager, Phase::Probation);
}

#[test]
fn gaps_clock_jumps_and_reordered_or_mismatched_evidence_cannot_qualify() {
    let _serial = crate::TEST_SERIAL.lock().unwrap();
    let (f, mut manager) = setup();
    manager.observe_probation(policy(), sample(&f, 1)).unwrap();
    manager.observe_probation(policy(), sample(&f, 2)).unwrap();
    assert!(manager.observe_probation(policy(), sample(&f, 2)).is_err());
    let mut wrong = sample(&f, 3);
    wrong.candidate = "e".repeat(64);
    assert!(manager.observe_probation(policy(), wrong).is_err());
    let mut wrong = sample(&f, 3);
    wrong.configuration_digest = "e".repeat(64);
    assert!(manager.observe_probation(policy(), wrong).is_err());
    let mut changed = policy();
    changed.profile_digest = "e".repeat(64);
    assert!(manager.observe_probation(changed, sample(&f, 3)).is_err());
    let mut short = policy();
    short.required_seconds = 1;
    assert!(manager.observe_probation(short, sample(&f, 3)).is_err());
    let mut future = sample(&f, 3);
    future.unix_seconds += MINIMUM_SECONDS;
    assert!(!manager.observe_probation(policy(), future).unwrap());
    assert_eq!(evidence(&mut manager).verified_seconds, 0);
    assert!(manager.observe_probation(policy(), sample(&f, 4)).is_err());
    assert!(
        !manager
            .observe_probation(policy(), sample(&f, 1000))
            .unwrap()
    );
    assert_eq!(evidence(&mut manager).verified_seconds, 0);
}

#[test]
fn failed_evidence_publication_never_changes_known_good() {
    let _serial = crate::TEST_SERIAL.lock().unwrap();
    let (f, mut manager) = setup();
    for id in 1..=288 {
        manager.observe_probation(policy(), sample(&f, id)).unwrap();
    }
    fs::write(f.state.join("state.next"), b"interrupted publication").unwrap();
    assert!(
        manager
            .observe_probation(policy(), sample(&f, 289))
            .is_err()
    );
    retained(&f, &manager, Phase::Probation);
    drop(manager);
    let mut manager = f.manager();
    assert_eq!(evidence(&mut manager).verified_seconds, 86100);
    assert!(
        manager
            .observe_probation(policy(), sample(&f, 290))
            .is_err()
    );
    retained(&f, &manager, Phase::Probation);
}

#[test]
fn completed_evidence_before_activation_commit_recovers_conservatively() {
    let _serial = crate::TEST_SERIAL.lock().unwrap();
    let (f, mut manager) = setup();
    // Fault boundary: evidence fsynced, healthy activation intent not yet published.
    manager
        .ledger
        .record_probation(State {
            policy: policy(),
            started_unix_seconds: 1_000_300,
            verified_seconds: MINIMUM_SECONDS,
            interrupted_intervals: 0,
            last: sample(&f, 289),
        })
        .unwrap();
    drop(manager);
    let mut manager = f.manager();
    retained(&f, &manager, Phase::Probation);
    assert!(
        manager
            .observe_probation(policy(), sample(&f, 290))
            .unwrap()
    );
    retained(&f, &manager, Phase::Healthy);
}
