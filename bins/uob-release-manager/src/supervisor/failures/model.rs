use crate::artifacts::InstallError;
use serde::{Deserialize, Serialize};

/// Administrator-owned thresholds, persisted with the incident to prevent reboot resets.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Policy {
    pub startup_seconds: u64,
    pub exit_window_seconds: u64,
    pub exit_count: usize,
    pub readiness_grace_seconds: u64,
    pub readiness_interval_seconds: u64,
    pub readiness_count: usize,
}
impl Default for Policy {
    fn default() -> Self {
        Self {
            startup_seconds: 30,
            exit_window_seconds: 120,
            exit_count: 3,
            readiness_grace_seconds: 30,
            readiness_interval_seconds: 10,
            readiness_count: 3,
        }
    }
}
impl Policy {
    pub(super) fn validate(self) -> Result<(), InstallError> {
        if [
            self.startup_seconds,
            self.exit_window_seconds,
            self.readiness_interval_seconds,
        ]
        .iter()
        .any(|v| !(1..=86400).contains(v))
            || self.readiness_grace_seconds > 86400
            || !(1..=64).contains(&self.exit_count)
            || !(1..=64).contains(&self.readiness_count)
        {
            return Err(InstallError::Rejected("invalid failure policy"));
        }
        Ok(())
    }
}

/// Classification uses trusted host facts, not raw exception strings.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Signal {
    /// Invocation IDs increase across service and supervisor restarts.
    Started {
        invocation: u64,
    },
    CoreReady,
    StartupPending,
    InternalReadinessFailure,
    Exit {
        desired_running: bool,
        unexpected: bool,
    },
    Watchdog,
    Oom,
    FatalInvariant {
        data_valid_and_compatible: bool,
    },
    MqttOutage,
    EmsOutage,
    ExternalDatabaseOutage,
    CredentialRejection,
    MalformedChargerTraffic,
    NoChargers,
    CorruptStorage,
    FullStorage,
    OsKernelFailure,
    BothVersionsFailed,
}

/// Ordered observation from the host's persistent event cursor and UTC clock.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Observation {
    pub id: u64,
    pub at_seconds: u64,
    pub signal: Signal,
    pub resource_pressure: bool,
}

/// Recommendation only: artifact switching remains a separate guarded operation.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Decision {
    #[default]
    Observe,
    Degraded,
    StopStaging,
    RecheckAfterStaging,
    RollbackRequired,
    RecoveryRequired,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Audit {
    pub observation: Observation,
    pub decision: Decision,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Staging {
    #[default]
    Unchecked,
    Pending,
    Stopped,
}

/// Bounded durable incident state, included in authenticated supervisor status.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct State {
    pub policy: Option<Policy>,
    pub decision: Decision,
    pub last: Option<Observation>,
    pub invocation: u64,
    pub started_at: Option<u64>,
    pub core_ready: bool,
    pub exit_recorded: bool,
    pub exits: Vec<u64>,
    pub readiness_failures: Vec<u64>,
    pub staging: Staging,
    pub pending_pressure: Option<Observation>,
    pub trigger: Option<Audit>,
    pub audit: Vec<Audit>,
}
impl State {
    pub(super) fn observe(
        &mut self,
        policy: Policy,
        o: Observation,
    ) -> Result<Decision, InstallError> {
        policy.validate()?;
        if self.policy.is_some_and(|p| p != policy)
            || o.id == 0
            || self
                .last
                .is_some_and(|last| o.id <= last.id || o.at_seconds < last.at_seconds)
        {
            return Err(InstallError::Rejected(
                "changed policy or reordered failure observation",
            ));
        }
        self.policy = Some(policy);
        let decision = self.classify(policy, o)?;
        if matches!(
            decision,
            Decision::RollbackRequired | Decision::RecoveryRequired
        ) && self.decision != decision
        {
            self.trigger = Some(Audit {
                observation: o,
                decision,
            });
        }
        self.decision = decision;
        self.last = Some(o);
        if self.audit.len() == 64 {
            self.audit.remove(0);
        }
        self.audit.push(Audit {
            observation: o,
            decision,
        });
        Ok(decision)
    }

    fn classify(&mut self, p: Policy, o: Observation) -> Result<Decision, InstallError> {
        use Signal as S;
        if matches!(
            o.signal,
            S::CorruptStorage
                | S::FullStorage
                | S::OsKernelFailure
                | S::BothVersionsFailed
                | S::FatalInvariant {
                    data_valid_and_compatible: false
                }
        ) || self.decision == Decision::RecoveryRequired
        {
            return Ok(Decision::RecoveryRequired);
        }
        if self.decision == Decision::RollbackRequired {
            return Ok(self.decision);
        }
        if matches!(
            o.signal,
            S::MqttOutage
                | S::EmsOutage
                | S::ExternalDatabaseOutage
                | S::CredentialRejection
                | S::MalformedChargerTraffic
                | S::NoChargers
        ) {
            return Ok(if self.staging == Staging::Pending {
                Decision::StopStaging
            } else {
                Decision::Degraded
            });
        }
        if let S::Started { invocation } = o.signal {
            if self.staging == Staging::Pending {
                return Err(InstallError::Rejected(
                    "resolve pending staging shutdown before a new invocation",
                ));
            }
            if invocation <= self.invocation {
                return Err(InstallError::Rejected("stale invocation"));
            }
            self.invocation = invocation;
            self.started_at = Some(o.at_seconds);
            self.core_ready = false;
            self.exit_recorded = false;
            self.readiness_failures.clear();
            return Ok(if self.staging == Staging::Pending {
                Decision::StopStaging
            } else {
                Decision::Observe
            });
        }
        let started = self
            .started_at
            .ok_or(InstallError::Rejected("missing invocation start"))?;
        let pressure_signal = matches!(
            o.signal,
            S::InternalReadinessFailure
                | S::StartupPending
                | S::Exit {
                    desired_running: true,
                    unexpected: true
                }
                | S::Oom
                | S::Watchdog
                | S::FatalInvariant { .. }
        );
        if self.staging == Staging::Pending
            || (o.resource_pressure && pressure_signal && self.staging != Staging::Stopped)
        {
            self.staging = Staging::Pending;
            self.pending_pressure.get_or_insert(o);
            self.readiness_failures.clear();
            return Ok(Decision::StopStaging);
        }
        Ok(self.evaluate(p, o, started))
    }

    pub(super) fn staging_stopped(&mut self, success: bool) {
        self.staging = if success {
            Staging::Stopped
        } else {
            Staging::Pending
        };
        self.decision = if success {
            Decision::RecheckAfterStaging
        } else {
            Decision::RecoveryRequired
        };
        if success
            && let Some(o) = self.pending_pressure.take()
            && matches!(
                o.signal,
                Signal::Exit { .. }
                    | Signal::Oom
                    | Signal::Watchdog
                    | Signal::FatalInvariant { .. }
            )
        {
            self.decision = self.evaluate(
                self.policy.expect("validated policy"),
                o,
                self.started_at.expect("validated invocation"),
            );
        }
        if matches!(
            self.decision,
            Decision::RollbackRequired | Decision::RecoveryRequired
        ) {
            self.trigger = self.last.map(|observation| Audit {
                observation,
                decision: self.decision,
            });
        }
        if let Some(last) = self.audit.last_mut() {
            last.decision = self.decision;
        }
    }
}
