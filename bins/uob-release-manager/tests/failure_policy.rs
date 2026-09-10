#[allow(dead_code)]
#[path = "supervisor/support.rs"]
mod support;
use support::Fixture;
use uob_release_manager::supervisor::{
    Code, Grant, Permission, Request, Supervisor,
    failures::{Decision as D, Observation, Policy, Signal as S, StagingStop},
};

struct Stop {
    calls: usize,
    success: bool,
    state: std::path::PathBuf,
}
impl StagingStop for Stop {
    fn stop_and_confirm(&mut self) -> bool {
        // Side effects must follow durable intent, even if the host dies in this call.
        let json: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&self.state).unwrap()).unwrap();
        assert_eq!(json["failures"]["decision"], "stop_staging");
        self.calls += 1;
        self.success
    }
}
fn manager(f: &Fixture) -> Supervisor {
    f.manager(vec![Grant {
        uid: 1,
        permissions: vec![Permission::Read],
    }])
}
fn stop(f: &Fixture) -> Stop {
    Stop {
        calls: 0,
        success: true,
        state: f.state.join("state.json"),
    }
}
fn observe(m: &mut Supervisor, stop: &mut Stop, id: u64, at: u64, signal: S) -> D {
    m.observe_failure(
        Policy::default(),
        Observation {
            id,
            at_seconds: at,
            signal,
            resource_pressure: false,
        },
        stop,
    )
    .unwrap()
}

#[test]
fn startup_threshold_survives_reboot_and_latches() {
    let f = Fixture::new();
    let mut m = manager(&f);
    let mut stop = stop(&f);
    observe(&mut m, &mut stop, 1, 100, S::Started { invocation: 1 });
    assert_eq!(
        observe(&mut m, &mut stop, 2, 129, S::StartupPending),
        D::Observe
    );
    drop(m);
    let mut m = manager(&f);
    assert_eq!(
        observe(&mut m, &mut stop, 3, 130, S::StartupPending),
        D::RollbackRequired
    );
    assert_eq!(
        observe(&mut m, &mut stop, 4, 131, S::CoreReady),
        D::RollbackRequired
    );
    assert_eq!(
        observe(&mut m, &mut stop, 5, 132, S::BothVersionsFailed),
        D::RecoveryRequired
    );
    assert_eq!(m.handle(1, Request::Status {}).code, Code::RecoveryRequired);
}

#[test]
fn three_distinct_exits_include_clean_exit_and_inclusive_window() {
    let f = Fixture::new();
    let mut m = manager(&f);
    let mut stop = stop(&f);
    observe(&mut m, &mut stop, 1, 0, S::Started { invocation: 1 });
    assert_eq!(
        observe(
            &mut m,
            &mut stop,
            2,
            1,
            S::Exit {
                desired_running: true,
                unexpected: true
            }
        ),
        D::Observe
    );
    // Multiple termination notifications for one invocation count once.
    observe(&mut m, &mut stop, 3, 1, S::Watchdog);
    drop(m);
    let mut m = manager(&f);
    observe(&mut m, &mut stop, 4, 50, S::Started { invocation: 2 });
    assert_eq!(observe(&mut m, &mut stop, 5, 60, S::Watchdog), D::Observe);
    observe(&mut m, &mut stop, 6, 100, S::Started { invocation: 3 });
    assert_eq!(
        observe(&mut m, &mut stop, 7, 121, S::Oom),
        D::RollbackRequired
    );
}

#[test]
fn expired_exit_window_and_planned_exits_do_not_trigger() {
    let f = Fixture::new();
    let mut m = manager(&f);
    let mut stop = stop(&f);
    for (i, at) in [0, 60, 121].into_iter().enumerate() {
        let id = i as u64 * 2 + 1;
        observe(&mut m, &mut stop, id, at, S::Started { invocation: id });
        assert_eq!(
            observe(&mut m, &mut stop, id + 1, at, S::Watchdog),
            D::Observe
        );
    }
    observe(&mut m, &mut stop, 7, 122, S::Started { invocation: 7 });
    assert_eq!(
        observe(
            &mut m,
            &mut stop,
            8,
            122,
            S::Exit {
                desired_running: false,
                unexpected: true
            }
        ),
        D::Observe
    );
    assert_eq!(
        observe(
            &mut m,
            &mut stop,
            9,
            122,
            S::Exit {
                desired_running: true,
                unexpected: false
            }
        ),
        D::Observe
    );
}

#[test]
fn readiness_requires_grace_spacing_and_consecutive_checks_across_reboot() {
    let f = Fixture::new();
    let mut m = manager(&f);
    let mut stop = stop(&f);
    observe(&mut m, &mut stop, 1, 0, S::Started { invocation: 1 });
    for (id, at) in [(2, 29), (3, 30), (4, 39), (5, 40)] {
        assert_eq!(
            observe(&mut m, &mut stop, id, at, S::InternalReadinessFailure),
            D::Observe
        );
    }
    drop(m);
    let mut m = manager(&f);
    assert_eq!(
        observe(&mut m, &mut stop, 6, 50, S::InternalReadinessFailure),
        D::RollbackRequired
    );
}

#[test]
fn ready_and_missing_sample_reset_consecutive_failures() {
    let f = Fixture::new();
    let mut m = manager(&f);
    let mut stop = stop(&f);
    observe(&mut m, &mut stop, 1, 0, S::Started { invocation: 1 });
    observe(&mut m, &mut stop, 2, 30, S::InternalReadinessFailure);
    observe(&mut m, &mut stop, 3, 40, S::CoreReady);
    for (id, at) in [(4, 50), (5, 70), (6, 80)] {
        assert_eq!(
            observe(&mut m, &mut stop, id, at, S::InternalReadinessFailure),
            D::Observe
        );
    }
    assert_eq!(
        observe(&mut m, &mut stop, 7, 90, S::InternalReadinessFailure),
        D::RollbackRequired
    );
}

#[test]
fn external_failures_are_bounded_degradation_and_never_call_staging() {
    let f = Fixture::new();
    let mut m = manager(&f);
    let mut stop = stop(&f);
    observe(&mut m, &mut stop, 1, 0, S::Started { invocation: 1 });
    let signals = [
        S::MqttOutage,
        S::EmsOutage,
        S::ExternalDatabaseOutage,
        S::CredentialRejection,
        S::MalformedChargerTraffic,
        S::NoChargers,
    ];
    for id in 2..100 {
        assert_eq!(
            observe(
                &mut m,
                &mut stop,
                id,
                id * 30,
                signals[usize::try_from(id).unwrap() % signals.len()]
            ),
            D::Degraded
        );
    }
    assert_eq!(stop.calls, 0);
    drop(m);
    let mut m = manager(&f);
    let state = m
        .handle(1, Request::Status {})
        .status
        .unwrap()
        .failures
        .unwrap();
    assert!(state.exits.is_empty());
    assert!(state.readiness_failures.is_empty());
    assert_eq!(state.audit.len(), 64);
    assert!(!f.store.join("active").exists());
}

#[test]
fn invalid_data_and_infrastructure_failures_require_recovery() {
    for signal in [
        S::CorruptStorage,
        S::FullStorage,
        S::OsKernelFailure,
        S::BothVersionsFailed,
        S::FatalInvariant {
            data_valid_and_compatible: false,
        },
    ] {
        let f = Fixture::new();
        let mut m = manager(&f);
        let mut stop = stop(&f);
        assert_eq!(
            observe(&mut m, &mut stop, 1, 0, signal),
            D::RecoveryRequired
        );
        drop(m);
        let mut m = manager(&f);
        assert_eq!(m.handle(1, Request::Status {}).code, Code::RecoveryRequired);
    }
}

#[test]
fn compatible_fatal_invariant_is_immediate() {
    let f = Fixture::new();
    let mut m = manager(&f);
    let mut stop = stop(&f);
    observe(&mut m, &mut stop, 1, 0, S::Started { invocation: 1 });
    assert_eq!(
        observe(
            &mut m,
            &mut stop,
            2,
            1,
            S::FatalInvariant {
                data_valid_and_compatible: true
            }
        ),
        D::RollbackRequired
    );
}

#[test]
fn pressure_stops_staging_before_fresh_readiness_classification() {
    let f = Fixture::new();
    let mut m = manager(&f);
    let mut stop = stop(&f);
    observe(&mut m, &mut stop, 1, 0, S::Started { invocation: 1 });
    observe(&mut m, &mut stop, 2, 30, S::InternalReadinessFailure);
    observe(&mut m, &mut stop, 3, 40, S::InternalReadinessFailure);
    let decision = m
        .observe_failure(
            Policy::default(),
            Observation {
                id: 4,
                at_seconds: 50,
                signal: S::InternalReadinessFailure,
                resource_pressure: true,
            },
            &mut stop,
        )
        .unwrap();
    assert_eq!(decision, D::RecheckAfterStaging);
    assert_eq!(stop.calls, 1);
    drop(m);
    let mut m = manager(&f);
    for (id, at) in [(5, 60), (6, 70)] {
        assert_eq!(
            observe(&mut m, &mut stop, id, at, S::InternalReadinessFailure),
            D::Observe
        );
    }
    assert_eq!(
        observe(&mut m, &mut stop, 7, 80, S::InternalReadinessFailure),
        D::RollbackRequired
    );
}

#[test]
fn unavailable_staging_control_is_recovery_not_rollback() {
    let f = Fixture::new();
    let mut m = manager(&f);
    let mut stop = stop(&f);
    stop.success = false;
    observe(&mut m, &mut stop, 1, 0, S::Started { invocation: 1 });
    assert_eq!(
        m.observe_failure(
            Policy::default(),
            Observation {
                id: 2,
                at_seconds: 30,
                signal: S::InternalReadinessFailure,
                resource_pressure: true
            },
            &mut stop
        )
        .unwrap(),
        D::RecoveryRequired
    );
}

#[test]
fn custom_thresholds_and_invalid_or_replayed_input() {
    let f = Fixture::new();
    let mut m = manager(&f);
    let mut stop = stop(&f);
    let policy = Policy {
        startup_seconds: 5,
        ..Policy::default()
    };
    let o = Observation {
        id: 1,
        at_seconds: 10,
        signal: S::Started { invocation: 1 },
        resource_pressure: false,
    };
    m.observe_failure(policy, o, &mut stop).unwrap();
    assert!(m.observe_failure(policy, o, &mut stop).is_err());
    assert!(
        m.observe_failure(Policy::default(), Observation { id: 2, ..o }, &mut stop)
            .is_err()
    );
    assert!(
        m.observe_failure(
            policy,
            Observation {
                id: 2,
                at_seconds: 9,
                ..o
            },
            &mut stop
        )
        .is_err()
    );
    assert_eq!(
        m.observe_failure(
            policy,
            Observation {
                id: 2,
                at_seconds: 15,
                signal: S::StartupPending,
                ..o
            },
            &mut stop
        )
        .unwrap(),
        D::RollbackRequired
    );
}

#[test]
fn pressure_related_termination_is_counted_after_staging_shutdown() {
    let f = Fixture::new();
    let mut m = manager(&f);
    let mut stop = stop(&f);
    let policy = Policy {
        exit_count: 1,
        ..Policy::default()
    };
    m.observe_failure(
        policy,
        Observation {
            id: 1,
            at_seconds: 0,
            signal: S::Started { invocation: 1 },
            resource_pressure: false,
        },
        &mut stop,
    )
    .unwrap();
    assert_eq!(
        m.observe_failure(
            policy,
            Observation {
                id: 2,
                at_seconds: 1,
                signal: S::Oom,
                resource_pressure: true
            },
            &mut stop
        )
        .unwrap(),
        D::RollbackRequired
    );
    assert_eq!(stop.calls, 1);
}

struct InterruptedStop;
impl StagingStop for InterruptedStop {
    fn stop_and_confirm(&mut self) -> bool {
        panic!("simulated supervisor interruption");
    }
}
#[test]
fn interrupted_staging_shutdown_retries_before_classifying() {
    let f = Fixture::new();
    let mut m = manager(&f);
    let mut stop = stop(&f);
    observe(&mut m, &mut stop, 1, 0, S::Started { invocation: 1 });
    let o = Observation {
        id: 2,
        at_seconds: 30,
        signal: S::InternalReadinessFailure,
        resource_pressure: true,
    };
    let interrupted = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        m.observe_failure(Policy::default(), o, &mut InterruptedStop)
            .unwrap();
    }));
    assert!(interrupted.is_err());
    drop(m);
    let mut m = manager(&f);
    assert!(
        m.observe_failure(
            Policy::default(),
            Observation {
                id: 3,
                at_seconds: 40,
                signal: S::Started { invocation: 2 },
                resource_pressure: false
            },
            &mut stop
        )
        .is_err()
    );
    assert_eq!(
        observe(&mut m, &mut stop, 3, 40, S::InternalReadinessFailure),
        D::RecheckAfterStaging
    );
    assert_eq!(stop.calls, 1);
}

#[test]
fn failed_write_never_invokes_host_side_effect() {
    let f = Fixture::new();
    let mut m = manager(&f);
    let mut stop = stop(&f);
    observe(&mut m, &mut stop, 1, 0, S::Started { invocation: 1 });
    std::fs::write(f.state.join("state.next"), b"interrupted").unwrap();
    assert!(
        m.observe_failure(
            Policy::default(),
            Observation {
                id: 2,
                at_seconds: 30,
                signal: S::InternalReadinessFailure,
                resource_pressure: true
            },
            &mut stop
        )
        .is_err()
    );
    assert_eq!(stop.calls, 0);
    assert_eq!(m.handle(1, Request::Status {}).code, Code::RecoveryRequired);
    drop(m);
    let mut m = manager(&f);
    assert_eq!(m.handle(1, Request::Status {}).code, Code::RecoveryRequired);
}
