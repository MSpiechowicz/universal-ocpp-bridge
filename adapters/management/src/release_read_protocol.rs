use serde::{Deserialize, Serialize};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct EventsQuery {
    #[serde(default)]
    pub(crate) after: u64,
}

#[derive(Serialize)]
#[serde(tag = "operation", rename_all = "snake_case")]
pub(crate) enum Request {
    Status {},
    Events { after: u64 },
}

#[derive(Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Code {
    Ok,
    Forbidden,
    InvalidRequest,
    Busy,
    ArtifactRejected,
    QualificationRequired,
    EvidenceRejected,
    PreflightRejected,
    ActivationBlocked,
    RecoveryRequired,
    StorageFailure,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Response {
    protocol: u32,
    manager_version: String,
    pub(crate) code: Code,
    #[serde(skip_serializing_if = "Option::is_none")]
    status: Option<Status>,
    #[serde(skip_serializing_if = "Option::is_none")]
    events: Option<Events>,
}

impl Response {
    pub(crate) fn valid(&self) -> bool {
        self.protocol == 1
            && valid_version(&self.manager_version)
            && self.status.as_ref().is_none_or(Status::valid)
            && self.events.as_ref().is_none_or(Events::valid)
    }

    pub(crate) fn status_result(&self) -> bool {
        self.events.is_none()
            && match self.code {
                Code::Ok | Code::RecoveryRequired => self.status.is_some(),
                Code::Busy | Code::Forbidden => self.status.is_none(),
                _ => false,
            }
    }

    pub(crate) fn events_result(&self, after: u64) -> bool {
        self.status.is_none()
            && match self.code {
                Code::Ok => self
                    .events
                    .as_ref()
                    .is_some_and(|events| events.for_cursor(after)),
                Code::Busy | Code::Forbidden => self.events.is_none(),
                _ => false,
            }
    }
}

fn valid_version(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b".-+".contains(&byte))
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Status {
    rollback: Option<serde_json::Value>,
    probation: Option<serde_json::Value>,
    promotion: Option<serde_json::Value>,
    failures: Option<serde_json::Value>,
    sequence: u64,
    failed_operations: u64,
    staged_verified_digest: Option<String>,
    last_operation: Option<Record>,
    qualification: Option<serde_json::Value>,
}

impl Status {
    fn valid(&self) -> bool {
        [
            &self.rollback,
            &self.probation,
            &self.promotion,
            &self.failures,
            &self.qualification,
        ]
        .into_iter()
        .all(|value| value.as_ref().is_none_or(serde_json::Value::is_object))
            && self
                .staged_verified_digest
                .as_ref()
                .is_none_or(|digest| digest_name(digest))
            && self.last_operation.as_ref().is_none_or(Record::valid)
    }
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Events {
    records: Vec<Record>,
    oldest_sequence: u64,
    latest_sequence: u64,
    truncated: bool,
}

impl Events {
    fn valid(&self) -> bool {
        (self.oldest_sequence == 0 && self.latest_sequence == 0
            || self.oldest_sequence != 0 && self.oldest_sequence <= self.latest_sequence)
            && self.records.len() <= 64
            && self.records.iter().all(Record::valid)
            && self.records.iter().all(|record| {
                record.sequence >= self.oldest_sequence && record.sequence <= self.latest_sequence
            })
            && self
                .records
                .windows(2)
                .all(|records| records[0].sequence < records[1].sequence)
    }

    fn for_cursor(&self, after: u64) -> bool {
        self.valid()
            && self.truncated
                == (self.oldest_sequence != 0 && after < self.oldest_sequence.saturating_sub(1))
            && if self.latest_sequence == 0 || after >= self.latest_sequence {
                self.records.is_empty()
            } else {
                !self.records.is_empty()
                    && self
                        .records
                        .first()
                        .is_some_and(|record| record.sequence > after)
                    && self
                        .records
                        .last()
                        .is_some_and(|record| record.sequence == self.latest_sequence)
                    && (!self.truncated
                        || self
                            .records
                            .first()
                            .is_some_and(|record| record.sequence == self.oldest_sequence))
            }
    }
}

#[derive(Default, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum Actor {
    #[default]
    Operator,
    Supervisor,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Record {
    sequence: u64,
    uid: u32,
    request: AuditRequest,
    result: Code,
    #[serde(default)]
    actor: Actor,
    #[serde(skip_serializing_if = "Option::is_none")]
    decision: Option<Decision>,
}

#[derive(Deserialize, Serialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
enum AuditRequest {
    Stage {
        digest: String,
    },
    Qualify {
        digest: String,
        evidence_digest: String,
    },
    Promote {
        digest: String,
    },
    Rollback {},
}

impl Record {
    fn valid(&self) -> bool {
        if self.sequence == 0 || !self.request.valid() {
            return false;
        }
        match (&self.actor, &self.decision, &self.request) {
            (Actor::Operator, None, _) => true,
            (Actor::Supervisor, Some(decision), request) => decision.matches_request(request),
            _ => false,
        }
    }
}

impl AuditRequest {
    fn valid(&self) -> bool {
        match self {
            Self::Stage { digest } | Self::Promote { digest } => digest_name(digest),
            Self::Qualify {
                digest,
                evidence_digest,
            } => digest_name(digest) && digest_name(evidence_digest),
            Self::Rollback {} => true,
        }
    }
}

#[derive(Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Decision {
    Promote {
        candidate_digest: String,
        previous_good_digest: Option<String>,
        evidence_digest: Option<String>,
        configuration_digest: Option<String>,
        compatibility: Compatibility,
        drain: Drain,
        health: Health,
        outcome: PromotionOutcome,
    },
    Failure {
        candidate_digest: Option<String>,
        previous_good_digest: Option<String>,
        observation: Observation,
        decision: FailureDecision,
        trigger_id: Option<u64>,
    },
    Rollback {
        quarantined_digest: Option<String>,
        previous_good_digest: Option<String>,
        step: RollbackStep,
        reason: RollbackReason,
    },
}

impl Decision {
    fn matches_request(&self, request: &AuditRequest) -> bool {
        match (self, request) {
            (
                Self::Promote {
                    candidate_digest, ..
                },
                AuditRequest::Promote { digest },
            ) => candidate_digest == digest && self.valid(),
            (Self::Failure { .. } | Self::Rollback { .. }, AuditRequest::Rollback {}) => {
                self.valid()
            }
            _ => false,
        }
    }

    fn valid(&self) -> bool {
        match self {
            Self::Promote {
                candidate_digest,
                previous_good_digest,
                evidence_digest,
                configuration_digest,
                ..
            } => [
                Some(candidate_digest),
                previous_good_digest.as_ref(),
                evidence_digest.as_ref(),
                configuration_digest.as_ref(),
            ]
            .into_iter()
            .flatten()
            .all(|digest| digest_name(digest)),
            Self::Failure {
                candidate_digest,
                previous_good_digest,
                observation,
                trigger_id,
                ..
            } => {
                [candidate_digest.as_ref(), previous_good_digest.as_ref()]
                    .into_iter()
                    .flatten()
                    .all(|digest| digest_name(digest))
                    && observation.id != 0
                    && trigger_id.is_none_or(|id| id != 0)
            }
            Self::Rollback {
                quarantined_digest,
                previous_good_digest,
                ..
            } => [quarantined_digest.as_ref(), previous_good_digest.as_ref()]
                .into_iter()
                .flatten()
                .all(|digest| digest_name(digest)),
        }
    }
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum Compatibility {
    NotChecked,
    Accepted,
    Rejected,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum Drain {
    NotRequested,
    Granted,
    Rejected,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum Health {
    NotObserved,
    Probation,
    Healthy,
    Rejected,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum PromotionOutcome {
    Continuing,
    Rejected,
    RecoveryRequired,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Observation {
    id: u64,
    at_seconds: u64,
    signal: Signal,
    resource_pressure: bool,
}

#[derive(Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Signal {
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

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum FailureDecision {
    Observe,
    Degraded,
    StopStaging,
    RecheckAfterStaging,
    RollbackRequired,
    RecoveryRequired,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum RollbackStep {
    Attempting,
    Restored,
    RecoveryRequired,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum RollbackReason {
    EligibleFailure,
    NoPreviousGood,
    EligibilityRejected,
    ProcessFailed,
}

fn digest_name(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}
