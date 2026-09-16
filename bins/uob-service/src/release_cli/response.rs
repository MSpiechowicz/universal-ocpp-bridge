use super::{Code, Request, digest_name};
use serde::{Deserialize, Serialize};

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Response {
    pub(super) protocol: u32,
    pub(super) manager_version: String,
    pub(super) code: Code,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) status: Option<Status>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) events: Option<Events>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) activation: Option<Activation>,
}

impl Response {
    pub(super) fn valid(&self) -> bool {
        self.protocol == 1
            && !self.manager_version.is_empty()
            && self.manager_version.len() <= 64
            && self
                .manager_version
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b".-+".contains(&byte))
            && self.status.as_ref().is_none_or(Status::valid)
            && self.events.as_ref().is_none_or(Events::valid)
            && self.activation.as_ref().is_none_or(Activation::valid)
            && self.activation.as_ref().is_none_or(|_| {
                self.events.is_none() && self.status.is_some() && self.code.is_status_result()
            })
    }
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Status {
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
pub(super) struct Activation {
    sequence: u64,
    production: Option<Release>,
    previous_good: Option<String>,
    candidate: Option<Release>,
}

impl Activation {
    fn valid(&self) -> bool {
        self.production.as_ref().is_none_or(Release::valid)
            && self.candidate.as_ref().is_none_or(Release::valid)
            && self
                .previous_good
                .as_ref()
                .is_none_or(|digest| digest_name(digest))
    }
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Release {
    digest: String,
    phase: Phase,
}

impl Release {
    fn valid(&self) -> bool {
        digest_name(&self.digest)
    }
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
enum Phase {
    Installed,
    Staging,
    Qualified,
    Promoting,
    Probation,
    Healthy,
    Quarantined,
    RollingBack,
    PreviousGood,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Events {
    pub(super) records: Vec<Record>,
    pub(super) oldest_sequence: u64,
    pub(super) latest_sequence: u64,
    pub(super) truncated: bool,
}

impl Events {
    fn valid(&self) -> bool {
        self.oldest_sequence <= self.latest_sequence.saturating_add(1)
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
pub(super) struct Record {
    sequence: u64,
    uid: u32,
    request: Request,
    result: Code,
    #[serde(default)]
    actor: Actor,
    #[serde(skip_serializing_if = "Option::is_none")]
    decision: Option<serde_json::Value>,
}

impl Record {
    fn valid(&self) -> bool {
        if self.sequence == 0
            || self
                .decision
                .as_ref()
                .is_some_and(|decision| !decision.is_object())
        {
            return false;
        }
        match &self.request {
            Request::Stage { digest } | Request::Promote { digest } => digest_name(digest),
            Request::Qualify {
                digest,
                evidence_digest,
            } => digest_name(digest) && digest_name(evidence_digest),
            Request::Status {} | Request::Rollback {} | Request::Events { .. } => true,
        }
    }
}
