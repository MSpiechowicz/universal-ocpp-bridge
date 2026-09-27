use super::*;

#[test]
fn failed_or_interrupted_fallback_is_never_retried_after_reboot() {
    let _serial = crate::TEST_SERIAL.lock().unwrap();
    run(async {
        for interrupted in [false, true] {
            let (f, mut manager, p) = setup().await;
            trigger(&mut manager);
            let mut process = Process {
                fail: !interrupted,
                hang: interrupted,
                ..Process::default()
            };
            if interrupted {
                assert!(
                    tokio::time::timeout(
                        Duration::from_millis(500),
                        manager.rollback_automatically(&mut process)
                    )
                    .await
                    .is_err()
                );
            } else {
                assert_eq!(
                    manager.rollback_automatically(&mut process).await,
                    Code::RecoveryRequired
                );
            }
            assert_eq!(process.starts, 1);
            drop(manager);
            let mut manager = f.manager().with_preflight_policy(p).unwrap();
            let mut process = Process::default();
            assert_eq!(
                manager.rollback_automatically(&mut process).await,
                Code::RecoveryRequired
            );
            assert_eq!((process.stops, process.starts), (0, 0));
            assert_eq!(
                manager.activation.state().candidate.as_ref().unwrap().phase,
                Phase::Quarantined
            );
        }
    });
}

#[test]
fn unsafe_eligibility_or_data_never_starts_a_fallback() {
    let _serial = crate::TEST_SERIAL.lock().unwrap();
    run(async {
        for bad in [
            "revoked", "floor", "config", "database", "corrupt", "evidence", "formats",
        ] {
            let (f, mut manager, p) = setup().await;
            trigger(&mut manager);
            match bad {
                "revoked" => {
                    manager
                        .policy
                        .security
                        .revoked_artifacts
                        .insert(f.previous.compatibility.artifact_digest.clone());
                }
                "floor" => manager.policy.security.minimum_release_sequence = u64::MAX,
                "config" => fs::write(&p.configuration, "changed").unwrap(),
                "database" => {
                    fs::rename(
                        &p.operational_database,
                        p.operational_database.with_extension("old"),
                    )
                    .unwrap();
                    fs::write(&p.operational_database, "replacement").unwrap();
                }
                "corrupt" => fs::write(&p.operational_database, "corrupt").unwrap(),
                "formats" => {
                    manager.policy.current_formats.external_database =
                        crate::SchemaVersion::new(999);
                }
                "evidence" => {
                    let q = manager.ledger.status().qualification.as_ref().unwrap();
                    fs::write(
                        f.state
                            .join("evidence")
                            .join(format!("{}.sig", q.evidence_digest)),
                        [0; 64],
                    )
                    .unwrap();
                }
                _ => unreachable!(),
            }
            let mut process = Process::default();
            assert_eq!(
                manager.rollback_automatically(&mut process).await,
                Code::RecoveryRequired,
                "{bad}"
            );
            assert_eq!(process.starts, 0, "{bad}");
        }
    });
}
