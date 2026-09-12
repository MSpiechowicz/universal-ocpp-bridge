use super::{ActivationJournal, Phase, Transition, disk};
use crate::artifacts::ArtifactStore;
use crate::test_support::artifacts as support;
use std::fs;
use support::Fixture;

fn install(f: &Fixture) {
    let (bytes, signature) = f.signed();
    ArtifactStore::open(&f.root)
        .unwrap()
        .install(&bytes, &signature, &mut f.payload.as_slice(), &f.policy)
        .unwrap();
}

fn advance(journal: &mut ActivationJournal, f: &Fixture, steps: &[Transition]) {
    for step in steps {
        journal.transition(*step, &f.policy).unwrap();
    }
}

fn established() -> (Fixture, ActivationJournal, String) {
    let mut f = Fixture::new();
    install(&f);
    let mut journal = ActivationJournal::open(&f.root, &f.policy).unwrap();
    advance(
        &mut journal,
        &f,
        &[
            Transition::BeginStaging,
            Transition::Qualify,
            Transition::BeginPromotion,
            Transition::BeginProbation,
            Transition::MarkHealthy,
        ],
    );
    let old = f.digest().to_owned();
    f.next_payload();
    install(&f);
    journal.observe_installed(&f.policy).unwrap();
    (f, journal, old)
}

#[test]
fn ordered_lifecycle_retains_previous_and_staging_failure_never_touches_production() {
    let (mut f, mut journal, old) = established();
    assert!(
        journal
            .transition(Transition::BeginPromotion, &f.policy)
            .is_err()
    );
    advance(&mut journal, &f, &[Transition::BeginStaging]);
    let production = journal.state().production.clone();
    let active = fs::read(f.root.join("active")).unwrap();
    advance(&mut journal, &f, &[Transition::FailStaging]);
    assert_eq!(journal.state().production, production);
    assert_eq!(fs::read(f.root.join("active")).unwrap(), active);
    assert!(!f.root.join("previous-good").exists());
    assert!(journal.transition(Transition::Qualify, &f.policy).is_err());
    drop(journal);
    let mut journal = ActivationJournal::open(&f.root, &f.policy).unwrap();
    assert_eq!(
        journal.state().candidate.as_ref().unwrap().phase,
        Phase::Quarantined
    );
    f.next_payload();
    install(&f);
    journal.observe_installed(&f.policy).unwrap();
    advance(
        &mut journal,
        &f,
        &[
            Transition::BeginStaging,
            Transition::Qualify,
            Transition::BeginPromotion,
            Transition::BeginProbation,
        ],
    );
    assert_eq!(
        fs::read(f.root.join("previous-good")).unwrap(),
        old.as_bytes()
    );
    assert_eq!(
        fs::read(f.root.join("active")).unwrap(),
        f.digest().as_bytes()
    );
    let (bytes, signature) = f.signed();
    assert!(
        ArtifactStore::open(&f.root)
            .unwrap()
            .install(&bytes, &signature, &mut f.payload.as_slice(), &f.policy,)
            .is_err()
    );
    advance(
        &mut journal,
        &f,
        &[Transition::BeginRollback, Transition::FinishRollback],
    );
    assert_eq!(fs::read(f.root.join("active")).unwrap(), old.as_bytes());
    assert_eq!(
        journal.state().production.as_ref().unwrap().phase,
        Phase::PreviousGood
    );
    assert_eq!(
        journal.state().candidate.as_ref().unwrap().phase,
        Phase::Quarantined
    );
}

#[test]
fn every_promotion_boundary_recovers_to_one_verified_pointer_and_exclusive_owner() {
    for stop in 0..22 {
        let (f, mut journal, old) = established();
        advance(
            &mut journal,
            &f,
            &[Transition::BeginStaging, Transition::Qualify],
        );
        let before = journal.state().clone();
        assert!(ActivationJournal::open(&f.root, &f.policy).is_err());
        disk::FAULT.set(Some(stop));
        assert!(
            journal
                .transition(Transition::BeginPromotion, &f.policy)
                .is_err(),
            "boundary {stop}"
        );
        disk::FAULT.set(None);
        assert!(
            journal
                .transition(Transition::BeginPromotion, &f.policy)
                .is_err()
        );
        let committed = f.root.join(".activation-intent").exists()
            || fs::read(f.root.join("active")).unwrap() == f.digest().as_bytes();
        drop(journal);
        let recovered = ActivationJournal::open(&f.root, &f.policy).unwrap();
        let expected = if committed { f.digest() } else { &old };
        assert_eq!(
            fs::read(f.root.join("active")).unwrap(),
            expected.as_bytes(),
            "boundary {stop}"
        );
        assert_eq!(
            recovered.state().production.as_ref().unwrap().digest,
            expected
        );
        if !committed {
            assert_eq!(recovered.state(), &before);
        }
        ArtifactStore::open(&f.root)
            .unwrap()
            .verify_installed(expected, &f.policy)
            .unwrap();
        assert!(ActivationJournal::open(&f.root, &f.policy).is_err());
        assert!(!f.root.join(".activation-intent").exists());
        drop(recovered);
        ActivationJournal::open(&f.root, &f.policy).unwrap();
    }
}

#[test]
fn interrupted_recovery_and_rollback_are_idempotent_at_every_boundary() {
    for stop in 0..12 {
        let (f, mut journal, old) = established();
        advance(
            &mut journal,
            &f,
            &[
                Transition::BeginStaging,
                Transition::Qualify,
                Transition::BeginPromotion,
                Transition::BeginProbation,
            ],
        );
        disk::FAULT.set(Some(4)); // Durable rollback intent, before first pointer write.
        assert!(
            journal
                .transition(Transition::BeginRollback, &f.policy)
                .is_err()
        );
        disk::FAULT.set(None);
        drop(journal);
        disk::FAULT.set(Some(stop));
        let result = ActivationJournal::open(&f.root, &f.policy);
        assert!(result.is_err(), "recovery boundary {stop}");
        disk::FAULT.set(None);
        drop(result);
        let mut recovered = ActivationJournal::open(&f.root, &f.policy).unwrap();
        assert_eq!(fs::read(f.root.join("active")).unwrap(), old.as_bytes());
        assert_eq!(
            recovered.state().production.as_ref().unwrap().phase,
            Phase::RollingBack
        );
        advance(&mut recovered, &f, &[Transition::FinishRollback]);
        assert_eq!(
            recovered.state().production.as_ref().unwrap().phase,
            Phase::PreviousGood
        );
    }
}

#[test]
fn stale_observations_corrupt_intents_and_unsafe_scratch_files_fail_closed() {
    let (mut f, mut journal, old) = established();
    advance(
        &mut journal,
        &f,
        &[Transition::BeginStaging, Transition::Qualify],
    );
    f.next_payload();
    install(&f);
    assert!(
        journal
            .transition(Transition::BeginPromotion, &f.policy)
            .is_err()
    );
    journal.observe_installed(&f.policy).unwrap();
    assert_eq!(
        journal.state().candidate.as_ref().unwrap().phase,
        Phase::Installed
    );
    drop(journal);
    fs::write(f.root.join(".activation-intent"), b"partial").unwrap();
    assert!(ActivationJournal::open(&f.root, &f.policy).is_err());
    assert!(ArtifactStore::open(&f.root).is_err());
    assert_eq!(fs::read(f.root.join("active")).unwrap(), old.as_bytes());
    fs::remove_file(f.root.join(".activation-intent")).unwrap();
    let outside = f.root.join("outside");
    fs::write(&outside, b"untouched").unwrap();
    std::os::unix::fs::symlink(&outside, f.root.join("active.next")).unwrap();
    assert!(ActivationJournal::open(&f.root, &f.policy).is_err());
    assert_eq!(fs::read(outside).unwrap(), b"untouched");
}

#[test]
fn state_only_transitions_and_rollback_recover_at_every_write_boundary() {
    let scenarios: &[(Transition, &[Transition], usize)] = &[
        (Transition::BeginStaging, &[], 12),
        (Transition::Qualify, &[Transition::BeginStaging], 12),
        (Transition::FailStaging, &[Transition::BeginStaging], 12),
        (
            Transition::BeginProbation,
            &[
                Transition::BeginStaging,
                Transition::Qualify,
                Transition::BeginPromotion,
            ],
            12,
        ),
        (
            Transition::MarkHealthy,
            &[
                Transition::BeginStaging,
                Transition::Qualify,
                Transition::BeginPromotion,
                Transition::BeginProbation,
            ],
            12,
        ),
        (
            Transition::BeginRollback,
            &[
                Transition::BeginStaging,
                Transition::Qualify,
                Transition::BeginPromotion,
                Transition::BeginProbation,
            ],
            17,
        ),
        (
            Transition::FinishRollback,
            &[
                Transition::BeginStaging,
                Transition::Qualify,
                Transition::BeginPromotion,
                Transition::BeginProbation,
                Transition::BeginRollback,
            ],
            12,
        ),
    ];
    for (step, setup, count) in scenarios {
        for stop in 0..*count {
            let (f, mut journal, _) = established();
            advance(&mut journal, &f, setup);
            let before = journal.state().clone();
            let after = before.advance(*step).unwrap();
            disk::FAULT.set(Some(stop));
            assert!(
                journal.transition(*step, &f.policy).is_err(),
                "{step:?}: {stop}"
            );
            disk::FAULT.set(None);
            drop(journal);
            let recovered = ActivationJournal::open(&f.root, &f.policy).unwrap();
            assert!(recovered.state() == &before || recovered.state() == &after);
            assert_eq!(
                fs::read(f.root.join("active")).unwrap(),
                recovered
                    .state()
                    .production
                    .as_ref()
                    .unwrap()
                    .digest
                    .as_bytes()
            );
            if matches!(step, Transition::FailStaging) {
                assert_eq!(recovered.state().production, before.production);
            }
        }
    }
}

#[test]
fn initial_journal_and_replacement_observation_recover_without_changing_production() {
    for stop in 0..5 {
        let f = Fixture::new();
        install(&f);
        disk::FAULT.set(Some(stop));
        assert!(ActivationJournal::open(&f.root, &f.policy).is_err());
        disk::FAULT.set(None);
        let journal = ActivationJournal::open(&f.root, &f.policy).unwrap();
        assert!(journal.state().production.is_none());
        assert_eq!(
            journal.state().candidate.as_ref().unwrap().digest,
            f.digest()
        );
    }
    for stop in 0..12 {
        let (mut f, mut journal, old) = established();
        f.next_payload();
        install(&f);
        disk::FAULT.set(Some(stop));
        assert!(journal.observe_installed(&f.policy).is_err());
        disk::FAULT.set(None);
        drop(journal);
        let mut recovered = ActivationJournal::open(&f.root, &f.policy).unwrap();
        recovered.observe_installed(&f.policy).unwrap();
        assert_eq!(
            recovered.state().candidate.as_ref().unwrap().digest,
            f.digest()
        );
        assert_eq!(fs::read(f.root.join("active")).unwrap(), old.as_bytes());
    }
}

#[test]
fn pointer_drift_fails_before_pruning_and_revoked_intents_never_switch() {
    let (f, mut journal, old) = established();
    fs::write(f.root.join("active"), f.digest()).unwrap();
    assert!(ArtifactStore::open(&f.root).is_err());
    assert!(f.root.join("artifacts").join(&old).exists());
    fs::write(f.root.join("active"), &old).unwrap();
    advance(
        &mut journal,
        &f,
        &[Transition::BeginStaging, Transition::Qualify],
    );
    disk::FAULT.set(Some(4));
    assert!(
        journal
            .transition(Transition::BeginPromotion, &f.policy)
            .is_err()
    );
    disk::FAULT.set(None);
    drop(journal);
    let mut revoked = f.policy.clone();
    revoked
        .security
        .revoked_artifacts
        .insert(f.manifest.compatibility.artifact_digest.clone());
    assert!(ActivationJournal::open(&f.root, &revoked).is_err());
    assert_eq!(fs::read(f.root.join("active")).unwrap(), old.as_bytes());
    assert!(f.root.join(".activation-intent").exists());
    ActivationJournal::open(&f.root, &f.policy).unwrap();
}
