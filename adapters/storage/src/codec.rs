use serde::{Deserialize, Serialize, de::DeserializeOwned};
use sha2::{Digest, Sha256};
use uob_application::{
    AcknowledgementScope, AtomicStoreWrite, AuthorizationChange, AuthorizationReference,
    AuthorizationState, COMMAND_DEDUPLICATION_RETENTION_SECONDS, CommittedRecord,
    CommittedRecordId, DeliveryAttempt, DeliveryAttemptResolution, DeliveryId, DeliveryOutcome,
    Durability, PendingDelivery, RecordedDeliveryAttempt, StorageError, StorageErrorCode,
};
use uob_contracts::{
    Command, CommandLifecycle, CommandResult, EventEnvelope, EventId, ResourceRef, StationSnapshot,
    TargetInstanceId,
};

#[derive(Debug)]
pub(crate) struct EncodedWrite {
    pub snapshot: Option<(String, String)>,
    pub authorization: Vec<EncodedAuthorization>,
    pub command: Option<EncodedCommand>,
    pub command_result: Option<EncodedCommandResult>,
    pub events: Vec<EncodedEvent>,
    pub deliveries: Vec<EncodedDelivery>,
    pub records: Vec<EncodedRecord>,
}

#[derive(Debug)]
pub(crate) struct EncodedCommand {
    pub request_id: String,
    pub fingerprint: String,
    pub admitted_at: i64,
    pub retain_until: i64,
    pub payload: String,
}

#[derive(Debug)]
pub(crate) struct EncodedCommandResult {
    pub request_id: String,
    pub unresolved: bool,
    pub payload: String,
}

#[derive(Debug)]
pub(crate) struct EncodedAuthorization {
    pub reference: String,
    pub resource: String,
    pub state: i64,
    pub revision: i64,
    pub changed_at: String,
}

#[derive(Debug)]
pub(crate) struct EncodedEvent {
    pub event_id: String,
    pub resource: String,
    pub sequence: i64,
    pub payload: String,
}

#[derive(Debug)]
pub(crate) struct EncodedDelivery {
    pub target_instance_id: String,
    pub target_revision: i64,
    pub event_id: String,
    pub delivery_id: String,
    pub ordering_key: String,
    pub deadline: String,
    pub durability: i64,
    pub payload: String,
}

#[derive(Debug)]
pub(crate) struct EncodedRecord {
    pub record_id: String,
    pub durability: i64,
    pub committed_at: String,
    pub payload: String,
}

#[derive(Debug)]
pub(crate) struct EncodedDeliveryAttempt {
    pub delivery_id: String,
    pub outcome: String,
    pub reported_at: String,
    pub resolution: i64,
    pub retry_at: Option<String>,
}

#[derive(Deserialize, Serialize)]
enum StoredDeliveryOutcome {
    LocallyExposed { surface: String },
    Acknowledged { peer: String, scope: String },
    RetryableFailure { reason: String },
    PermanentFailure { reason: String },
    Uncertain { reason: String },
}

pub(crate) fn encode_write<C, E, D, R>(
    write: AtomicStoreWrite<C, E, D, R>,
) -> Result<EncodedWrite, StorageError>
where
    C: Serialize,
    E: Serialize,
    D: Serialize,
    R: Serialize,
{
    let snapshot = write
        .station_snapshot
        .map(|value| Ok((json(&value.station)?, json(&value)?)))
        .transpose()?;
    let authorization = write
        .authorization_changes
        .into_iter()
        .map(|value| {
            Ok(EncodedAuthorization {
                reference: value.reference.as_str().to_owned(),
                resource: json(&value.resource)?,
                state: durability_or_state(value.state),
                revision: unsigned(value.revision, "authorization revision")?,
                changed_at: json(&value.changed_at)?,
            })
        })
        .collect::<Result<_, StorageError>>()?;
    let command = write.command.as_ref().map(encode_command).transpose()?;
    let command_result = write
        .command_result
        .map(|value| {
            Ok(EncodedCommandResult {
                request_id: value.return_route.request_id.as_str().to_owned(),
                unresolved: matches!(
                    value.lifecycle,
                    CommandLifecycle::Admitted
                        | CommandLifecycle::Dispatched
                        | CommandLifecycle::TransmissionUncertain { .. }
                ),
                payload: json(&value)?,
            })
        })
        .transpose()?;
    let events = write
        .journal_events
        .into_iter()
        .map(|value| {
            Ok(EncodedEvent {
                event_id: value.event_id.as_str().to_owned(),
                resource: json(&value.resource)?,
                sequence: unsigned(value.sequence, "event sequence")?,
                payload: json(&value)?,
            })
        })
        .collect::<Result<_, StorageError>>()?;
    let deliveries = write
        .required_deliveries
        .into_iter()
        .map(|value| {
            Ok(EncodedDelivery {
                target_instance_id: value.target_instance_id.as_str().to_owned(),
                target_revision: unsigned(value.target_configuration_revision, "target revision")?,
                event_id: value.event_id.as_str().to_owned(),
                delivery_id: value.delivery_id.as_str().to_owned(),
                ordering_key: json(&value.ordering_key)?,
                deadline: json(&value.deadline)?,
                durability: durability(value.durability),
                payload: json(&value.payload)?,
            })
        })
        .collect::<Result<_, StorageError>>()?;
    let records = write
        .committed_records
        .into_iter()
        .map(|value| {
            Ok(EncodedRecord {
                record_id: value.record_id.as_str().to_owned(),
                durability: durability(value.durability),
                committed_at: json(&value.committed_at)?,
                payload: json(&value.record)?,
            })
        })
        .collect::<Result<_, StorageError>>()?;
    Ok(EncodedWrite {
        snapshot,
        authorization,
        command,
        command_result,
        events,
        deliveries,
        records,
    })
}

pub(crate) fn encode_command<P: Serialize>(
    value: &Command<P>,
) -> Result<EncodedCommand, StorageError> {
    let admitted_at = value.admitted_at.into_inner().unix_timestamp();
    let retain_until = admitted_at
        .checked_add(COMMAND_DEDUPLICATION_RETENTION_SECONDS)
        .ok_or_else(|| {
            StorageError::new(
                StorageErrorCode::InvalidRequest,
                "command retention timestamp exceeds supported range",
            )
        })?;
    let request_id = value.request_id.as_str().to_owned();
    let payload = json(&value)?;
    let fingerprint = command_fingerprint(&payload)?;
    Ok(EncodedCommand {
        request_id,
        fingerprint,
        admitted_at,
        retain_until,
        payload,
    })
}

pub(crate) fn command_fingerprint(payload: &str) -> Result<String, StorageError> {
    let mut value: serde_json::Value = from_json(payload)?;
    let object = value.as_object_mut().ok_or_else(|| {
        StorageError::new(
            StorageErrorCode::IntegrityFailure,
            "stored command is not a JSON object",
        )
    })?;
    object.remove("request_id");
    object.remove("admitted_at");
    sort_json(&mut value);
    let normalized = serde_json::to_vec(&value).map_err(|_| {
        StorageError::new(
            StorageErrorCode::InvalidRequest,
            "command fingerprint serialization failed",
        )
    })?;
    let digest = Sha256::digest(normalized);
    let alphabet = b"0123456789abcdef";
    let mut fingerprint = String::with_capacity(71);
    fingerprint.push_str("sha256:");
    for byte in digest {
        fingerprint.push(char::from(alphabet[usize::from(byte >> 4)]));
        fingerprint.push(char::from(alphabet[usize::from(byte & 0x0f)]));
    }
    Ok(fingerprint)
}

fn sort_json(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Array(values) => values.iter_mut().for_each(sort_json),
        serde_json::Value::Object(values) => {
            let mut entries = std::mem::take(values).into_iter().collect::<Vec<_>>();
            entries.sort_unstable_by(|left, right| left.0.cmp(&right.0));
            for (key, mut child) in entries {
                sort_json(&mut child);
                values.insert(key, child);
            }
        }
        _ => {}
    }
}

pub(crate) fn decode_authorization(
    reference: String,
    resource: &str,
    state: i64,
    revision: i64,
    changed_at: &str,
) -> Result<AuthorizationChange, StorageError> {
    Ok(AuthorizationChange {
        reference: AuthorizationReference::new(reference).map_err(integrity)?,
        resource: from_json(resource)?,
        state: match state {
            0 => AuthorizationState::Active,
            1 => AuthorizationState::Revoked,
            _ => return Err(corrupt("unknown authorization state")),
        },
        revision: signed(revision, "authorization revision")?,
        changed_at: from_json(changed_at)?,
    })
}

pub(crate) fn decode_delivery<D: DeserializeOwned>(
    value: &EncodedDelivery,
) -> Result<PendingDelivery<D>, StorageError> {
    Ok(PendingDelivery {
        delivery_id: DeliveryId::new(value.delivery_id.clone()).map_err(integrity)?,
        event_id: EventId::new(value.event_id.clone()).map_err(integrity)?,
        target_instance_id: TargetInstanceId::new(value.target_instance_id.clone())
            .map_err(integrity)?,
        target_configuration_revision: signed(value.target_revision, "target revision")?,
        ordering_key: from_json(&value.ordering_key)?,
        deadline: from_json(&value.deadline)?,
        durability: decode_durability(value.durability)?,
        payload: from_json(&value.payload)?,
    })
}

pub(crate) fn decode_record<R: DeserializeOwned>(
    record_id: String,
    durability_value: i64,
    committed_at: &str,
    payload: &str,
) -> Result<CommittedRecord<R>, StorageError> {
    Ok(CommittedRecord {
        record_id: CommittedRecordId::new(record_id).map_err(integrity)?,
        durability: decode_durability(durability_value)?,
        committed_at: from_json(committed_at)?,
        record: from_json(payload)?,
    })
}

pub(crate) fn encode_delivery_attempt(
    attempt: &DeliveryAttempt,
) -> Result<EncodedDeliveryAttempt, StorageError> {
    let (resolution, retry_at) = match attempt.resolution {
        DeliveryAttemptResolution::RetryAt(at) => (0, Some(json(&at)?)),
        DeliveryAttemptResolution::Final => (1, None),
    };
    Ok(EncodedDeliveryAttempt {
        delivery_id: attempt.report.delivery_id.as_str().to_owned(),
        outcome: json(&StoredDeliveryOutcome::from(&attempt.report.outcome))?,
        reported_at: json(&attempt.report.reported_at)?,
        resolution,
        retry_at,
    })
}

pub(crate) fn decode_delivery_attempt(
    delivery_id: String,
    outcome: &str,
    reported_at: &str,
    resolution: i64,
    retry_at: Option<&str>,
) -> Result<RecordedDeliveryAttempt, StorageError> {
    let resolution = match (resolution, retry_at) {
        (0, Some(retry_at)) => DeliveryAttemptResolution::RetryAt(from_json(retry_at)?),
        (1, None) => DeliveryAttemptResolution::Final,
        _ => return Err(corrupt("invalid delivery attempt resolution")),
    };
    Ok(RecordedDeliveryAttempt {
        delivery_id: DeliveryId::new(delivery_id).map_err(integrity)?,
        outcome: StoredDeliveryOutcome::into_application(from_json(outcome)?),
        reported_at: from_json(reported_at)?,
        resolution,
    })
}

impl From<&DeliveryOutcome> for StoredDeliveryOutcome {
    fn from(value: &DeliveryOutcome) -> Self {
        match value {
            DeliveryOutcome::LocallyExposed { surface } => Self::LocallyExposed {
                surface: surface.clone(),
            },
            DeliveryOutcome::Acknowledged { peer, scope } => Self::Acknowledged {
                peer: peer.clone(),
                scope: scope.0.clone(),
            },
            DeliveryOutcome::RetryableFailure { reason } => Self::RetryableFailure {
                reason: reason.clone(),
            },
            DeliveryOutcome::PermanentFailure { reason } => Self::PermanentFailure {
                reason: reason.clone(),
            },
            DeliveryOutcome::Uncertain { reason } => Self::Uncertain {
                reason: reason.clone(),
            },
        }
    }
}

impl StoredDeliveryOutcome {
    fn into_application(self) -> DeliveryOutcome {
        match self {
            Self::LocallyExposed { surface } => DeliveryOutcome::LocallyExposed { surface },
            Self::Acknowledged { peer, scope } => DeliveryOutcome::Acknowledged {
                peer,
                scope: AcknowledgementScope(scope),
            },
            Self::RetryableFailure { reason } => DeliveryOutcome::RetryableFailure { reason },
            Self::PermanentFailure { reason } => DeliveryOutcome::PermanentFailure { reason },
            Self::Uncertain { reason } => DeliveryOutcome::Uncertain { reason },
        }
    }
}

pub(crate) fn decode_snapshot(value: &str) -> Result<StationSnapshot, StorageError> {
    from_json(value)
}

pub(crate) fn decode_event<E: DeserializeOwned>(
    value: &str,
) -> Result<EventEnvelope<E>, StorageError> {
    from_json(value)
}

pub(crate) fn decode_command<C: DeserializeOwned>(value: &str) -> Result<Command<C>, StorageError> {
    from_json(value)
}

pub(crate) fn decode_result(value: &str) -> Result<CommandResult, StorageError> {
    from_json(value)
}

pub(crate) fn resource_key(value: &ResourceRef) -> Result<String, StorageError> {
    json(value)
}

pub(crate) fn timestamp_key(value: &uob_contracts::UtcTimestamp) -> Result<String, StorageError> {
    json(value)
}

fn json<T: Serialize>(value: &T) -> Result<String, StorageError> {
    serde_json::to_string(value).map_err(|_| {
        StorageError::new(
            StorageErrorCode::InvalidRequest,
            "record serialization failed",
        )
    })
}

fn from_json<T: DeserializeOwned>(value: &str) -> Result<T, StorageError> {
    serde_json::from_str(value).map_err(|_| corrupt("committed record failed typed decoding"))
}

fn unsigned(value: u64, label: &str) -> Result<i64, StorageError> {
    i64::try_from(value).map_err(|_| {
        StorageError::new(
            StorageErrorCode::InvalidRequest,
            format!("{label} exceeds SQLite integer range"),
        )
    })
}

fn signed(value: i64, label: &str) -> Result<u64, StorageError> {
    u64::try_from(value).map_err(|_| corrupt(&format!("negative {label}")))
}

const fn durability(value: Durability) -> i64 {
    match value {
        Durability::Critical => 0,
        Durability::BestEffortTelemetry => 1,
    }
}

const fn durability_or_state(value: AuthorizationState) -> i64 {
    match value {
        AuthorizationState::Active => 0,
        AuthorizationState::Revoked => 1,
    }
}

fn decode_durability(value: i64) -> Result<Durability, StorageError> {
    match value {
        0 => Ok(Durability::Critical),
        1 => Ok(Durability::BestEffortTelemetry),
        _ => Err(corrupt("unknown durability value")),
    }
}

fn corrupt(detail: &str) -> StorageError {
    StorageError::new(StorageErrorCode::IntegrityFailure, detail)
}

fn integrity(error: impl std::fmt::Display) -> StorageError {
    corrupt(&error.to_string())
}
