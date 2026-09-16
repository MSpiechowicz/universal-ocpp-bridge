use super::{Actor, Code, Decision, Events, Record, Request, Status};
use crate::artifacts::{InstallError, filesystem as disk, manifest};
use std::{
    fs,
    fs::File,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
};

const HISTORY_CAPACITY: usize = 64;

pub struct Ledger {
    root: PathBuf,
    state: Status,
    history: Vec<Record>,
    recovery: bool,
    _lock: File,
}

impl Ledger {
    pub fn open(root: &Path) -> Result<Self, InstallError> {
        disk::directory(root)?;
        if fs::metadata(root)?.permissions().mode() & 0o777 != 0o700 {
            return Err(InstallError::Rejected(
                "supervisor state must be private mode 0700",
            ));
        }
        let lock_path = root.join("owner.lock");
        let lock = match disk::open(&lock_path, true, true) {
            Ok(file) => file,
            Err(InstallError::Io(e)) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                disk::open(&lock_path, true, false)?
            }
            Err(error) => return Err(error),
        };
        lock.try_lock()
            .map_err(|_| InstallError::Rejected("supervisor already running"))?;
        let path = root.join("state.json");
        let (state, history) = match fs::symlink_metadata(&path) {
            Ok(_) => decode(&disk::bounded_read(&path, 65536)?)?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (Status::default(), Vec::new()),
            Err(e) => return Err(e.into()),
        };
        validate(&state, &history)?;
        // A partial publication never becomes authoritative on restart. Keep status
        // readable, block mutation, and retain evidence for explicit recovery.
        let recovery = match fs::symlink_metadata(root.join("state.next")) {
            Ok(_) => true,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
            Err(e) => return Err(e.into()),
        };
        Ok(Self {
            root: root.to_owned(),
            state,
            history,
            recovery,
            _lock: lock,
        })
    }

    pub const fn status(&self) -> &Status {
        &self.state
    }
    pub const fn needs_recovery(&self) -> bool {
        self.recovery
    }

    pub fn events(&self, after: u64) -> Events {
        let oldest_sequence = self.history.first().map_or(0, |record| record.sequence);
        let latest_sequence = self.history.last().map_or(0, |record| record.sequence);
        let start = self
            .history
            .partition_point(|record| record.sequence <= after);
        Events {
            records: self.history[start..].to_vec(),
            oldest_sequence,
            latest_sequence,
            truncated: oldest_sequence != 0 && after < oldest_sequence.saturating_sub(1),
        }
    }

    pub fn record(&mut self, uid: u32, request: Request, result: Code) -> Result<(), InstallError> {
        let mut next = self.state.clone();
        if result != Code::Ok {
            next.failed_operations = next.failed_operations.saturating_add(1);
        }
        if let Request::Stage { digest } = &request {
            // A failed recheck invalidates the previous staging observation.
            next.staged_verified_digest = (result == Code::Ok).then(|| digest.clone());
            next.qualification = None;
        }
        if let Request::Qualify {
            digest,
            evidence_digest,
        } = &request
        {
            next.qualification = (result == Code::Ok).then(|| crate::qualification::Qualified {
                candidate_digest: digest.clone(),
                evidence_digest: evidence_digest.clone(),
                // Recomputed from authenticated evidence for every status/promotion read.
                pi_measurements_digest: None,
            });
        }
        self.append(
            next,
            Record {
                sequence: 0,
                uid,
                request,
                result,
                actor: Actor::Operator,
                decision: None,
            },
            true,
        )
    }

    pub fn record_failures(
        &mut self,
        failures: super::failures::State,
        decision: Decision,
    ) -> Result<(), InstallError> {
        let mut next = self.state.clone();
        next.failures = Some(failures);
        self.record_supervisor(next, Request::Rollback {}, Code::Ok, decision)
    }

    pub fn record_promotion(
        &mut self,
        promotion: super::promotion::Record,
        result: Code,
        decision: Decision,
    ) -> Result<(), InstallError> {
        let mut next = self.state.clone();
        if promotion.step == super::promotion::Step::Stopping {
            next.probation = None;
        }
        let request = Request::Promote {
            digest: promotion.candidate.clone(),
        };
        next.promotion = Some(promotion);
        self.record_supervisor(next, request, result, decision)
    }

    pub fn record_probation(
        &mut self,
        evidence: super::probation::State,
        decision: Decision,
    ) -> Result<(), InstallError> {
        let mut next = self.state.clone();
        let request = Request::Promote {
            digest: evidence.last.candidate.clone(),
        };
        next.probation = Some(evidence);
        self.record_supervisor(next, request, Code::Ok, decision)
    }

    pub fn record_rollback(
        &mut self,
        record: super::rollback::Record,
        result: Code,
        decision: Decision,
    ) -> Result<(), InstallError> {
        let mut next = self.state.clone();
        next.rollback = Some(record);
        self.record_supervisor(next, Request::Rollback {}, result, decision)
    }

    pub fn record_supervisor(
        &mut self,
        next: Status,
        request: Request,
        result: Code,
        decision: Decision,
    ) -> Result<(), InstallError> {
        self.append(
            next,
            Record {
                sequence: 0,
                uid: rustix::process::geteuid().as_raw(),
                request,
                result,
                actor: Actor::Supervisor,
                decision: Some(decision),
            },
            false,
        )
    }

    fn append(
        &mut self,
        mut next: Status,
        mut record: Record,
        last_operator: bool,
    ) -> Result<(), InstallError> {
        next.sequence = next
            .sequence
            .checked_add(1)
            .ok_or(InstallError::Rejected("supervisor sequence exhausted"))?;
        record.sequence = next.sequence;
        if last_operator {
            next.last_operation = Some(record.clone());
        }
        let mut history = self.history.clone();
        history.push(record);
        // The status snapshot contains the current incident and is never evicted. Older
        // event records yield first when detailed decisions exceed the byte envelope.
        if history.len() > HISTORY_CAPACITY {
            history.remove(0);
        }
        let bytes = loop {
            let bytes = encode(&next, &history)?;
            if bytes.len() <= 65536 {
                break bytes;
            }
            if history.len() <= 1 {
                return Err(InstallError::Rejected("supervisor ledger exceeds bound"));
            }
            history.remove(0);
        };
        self.persist(next, history, &bytes)
    }

    fn persist(
        &mut self,
        next: Status,
        history: Vec<Record>,
        bytes: &[u8],
    ) -> Result<(), InstallError> {
        validate(&next, &history)?;
        let temporary = self.root.join("state.next");
        self.recovery = true;
        disk::write_new(&temporary, bytes)?;
        fs::rename(&temporary, self.root.join("state.json"))?;
        disk::sync_dir(&self.root)?;
        self.state = next;
        self.history = history;
        self.recovery = false;
        Ok(())
    }
}

fn decode(bytes: &[u8]) -> Result<(Status, Vec<Record>), InstallError> {
    let mut state: serde_json::Value = serde_json::from_slice(bytes)?;
    let object = state
        .as_object_mut()
        .ok_or(InstallError::Rejected("invalid supervisor ledger"))?;
    let history: Option<Vec<Record>> = object
        .remove("history")
        .map(serde_json::from_value)
        .transpose()?;
    let state: Status = serde_json::from_value(state)?;
    let history = history.unwrap_or_else(|| state.last_operation.clone().into_iter().collect());
    Ok((state, history))
}

fn encode(state: &Status, history: &[Record]) -> Result<Vec<u8>, InstallError> {
    #[derive(serde::Serialize)]
    struct Persisted<'a> {
        #[serde(flatten)]
        state: &'a Status,
        history: &'a [Record],
    }
    Ok(serde_json::to_vec(&Persisted { state, history })?)
}

fn validate_record(record: &Record) -> bool {
    record.sequence != 0
        && match &record.request {
            Request::Stage { digest } | Request::Promote { digest } => {
                manifest::digest_name(digest)
            }
            Request::Qualify {
                digest,
                evidence_digest,
            } => manifest::digest_name(digest) && manifest::digest_name(evidence_digest),
            Request::Rollback {} => true,
            Request::Status {} | Request::Events { .. } => false,
        }
        && match record.actor {
            Actor::Operator => record.decision.is_none(),
            Actor::Supervisor => record.decision.as_ref().is_some_and(Decision::valid),
        }
}

fn validate(state: &Status, history: &[Record]) -> Result<(), InstallError> {
    if let Some(record) = &state.rollback {
        record.validate()?;
    }
    if let Some(evidence) = &state.probation {
        evidence.validate()?;
        if state.promotion.as_ref().is_none_or(|p| {
            p.candidate != evidence.last.candidate
                || p.configuration_digest != evidence.last.configuration_digest
        }) {
            return Err(InstallError::Rejected("probation activation mismatch"));
        }
    }
    if let Some(promotion) = &state.promotion {
        promotion.validate()?;
    }
    if let Some(failures) = &state.failures {
        failures.validate()?;
    }
    let bad_digest = state
        .staged_verified_digest
        .as_ref()
        .is_some_and(|digest| !manifest::digest_name(digest));
    let bad_last_operation = state.last_operation.as_ref().is_some_and(|record| {
        record.actor != Actor::Operator
            || record.decision.is_some()
            || record.sequence > state.sequence
            || !validate_record(record)
    });
    let bad_history = history.len() > HISTORY_CAPACITY
        || history.iter().any(|record| !validate_record(record))
        || history
            .windows(2)
            .any(|records| records[0].sequence.checked_add(1) != Some(records[1].sequence))
        || history
            .last()
            .is_some_and(|latest| latest.sequence != state.sequence)
        || (history.is_empty() && state.sequence != 0);
    if state.qualification.as_ref().is_some_and(|q| {
        !manifest::digest_name(&q.candidate_digest) || !manifest::digest_name(&q.evidence_digest)
    }) || bad_digest
        || bad_last_operation
        || bad_history
        || state.failed_operations > state.sequence
        || (state.sequence == 0) != state.last_operation.is_none() && history.is_empty()
    {
        return Err(InstallError::Rejected("invalid supervisor ledger"));
    }
    Ok(())
}
