use uob_contracts::{
    DataPointValue, NativeProtocolReference, ProtocolEdition, StationSnapshot, TransactionId,
    TransactionProtocolState, TransactionSnapshot, TransactionState, UtcTimestamp,
};

/// Target-neutral charger observation accepted from a protocol adapter.
///
/// The application owns this shape. Concrete `rust-ocpp` message types remain in the outward
/// adapter, while protocol edition and native resource references stay explicit so mappings do
/// not erase OCPP 1.6J versus 2.0.1 meaning.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ChargerObservation {
    /// A station has announced its identity and boot reason, when the edition defines one.
    Registration(RegistrationObservation),
    /// A station keepalive was received.
    Heartbeat {
        /// Negotiated protocol edition.
        protocol: ProtocolEdition,
    },
    /// A transaction start was reported by the station.
    TransactionStarted(TransactionStartObservation),
    /// An OCPP 2.0.1 transaction lifecycle event.
    TransactionEvent(TransactionEventObservation),
    /// One or more exact meter samples reported by the station.
    Measurements(MeasurementObservation),
}

/// OCPP 2.0.1 transaction lifecycle event with version-specific sequencing evidence.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TransactionEventObservation {
    pub protocol: ProtocolEdition,
    pub event: TransactionEventKind,
    pub native_transaction_id: String,
    pub native_resource: NativeProtocolReference,
    pub sequence_number: u32,
    pub trigger_reason: String,
    pub charging_state: Option<String>,
    pub stopped_reason: Option<String>,
    pub occurred_at: UtcTimestamp,
    pub measurements: Option<MeasurementObservation>,
    pub payload_fingerprint: String,
}

/// Lifecycle operation carried by an OCPP 2.0.1 `TransactionEvent`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TransactionEventKind {
    Started,
    Updated,
    Ended,
}

/// Result of reconciling a lifecycle event with durable snapshot state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TransactionApplyOutcome {
    Applied,
    Duplicate,
}

/// Reconciles one transaction event into the snapshot that callers persist atomically with events.
///
/// # Errors
///
/// Returns [`TransactionApplyError`] for an unsupported protocol, unknown resource, invalid
/// identity, stale or conflicting sequence, or an invalid lifecycle transition. The snapshot is
/// unchanged when reconciliation fails.
pub fn apply_transaction_event(
    snapshot: &mut StationSnapshot,
    observation: &TransactionEventObservation,
    observed_at: UtcTimestamp,
) -> Result<TransactionApplyOutcome, TransactionApplyError> {
    if observation.protocol != ProtocolEdition::Ocpp201 {
        return Err(TransactionApplyError::UnsupportedProtocol);
    }
    let transaction_id = TransactionId::new(observation.native_transaction_id.clone())
        .map_err(|_| TransactionApplyError::InvalidIdentity)?;
    let resource = snapshot
        .resources
        .iter()
        .find(|item| item.resource.native_protocol_reference == Some(observation.native_resource))
        .map(|item| item.resource.clone())
        .ok_or(TransactionApplyError::UnknownResource)?;
    let current = snapshot.transactions.iter().position(|transaction| {
        transaction.protocol_state.as_ref().is_some_and(|state| {
            state.protocol == observation.protocol
                && state.native_transaction_id == observation.native_transaction_id
        })
    });
    if let Some(index) = current {
        let transaction = &snapshot.transactions[index];
        let state = transaction
            .protocol_state
            .as_ref()
            .ok_or(TransactionApplyError::InvalidTransition)?;
        if observation.sequence_number == state.last_sequence_number {
            let same = state.native_resource == observation.native_resource
                && state.last_event == event_name(observation.event)
                && state.last_trigger_reason == observation.trigger_reason
                && state.last_event_at == observation.occurred_at
                && state.last_event_fingerprint == observation.payload_fingerprint;
            return if same {
                Ok(TransactionApplyOutcome::Duplicate)
            } else {
                Err(TransactionApplyError::ConflictingReplay)
            };
        }
        if observation.sequence_number != state.last_sequence_number.saturating_add(1) {
            return Err(TransactionApplyError::OutOfOrder);
        }
        if transaction.state == TransactionState::Ended {
            return Err(TransactionApplyError::AlreadyEnded);
        }
        if observation.event == TransactionEventKind::Started {
            return Err(TransactionApplyError::InvalidTransition);
        }
    } else if observation.event != TransactionEventKind::Started {
        return Err(TransactionApplyError::MissingStart);
    }

    if let Some(measurements) = &observation.measurements {
        apply_measurements(snapshot, measurements, observed_at)
            .map_err(|_| TransactionApplyError::UnknownResource)?;
    }
    let state = transaction_state(observation);
    let ended_at =
        (observation.event == TransactionEventKind::Ended).then_some(observation.occurred_at);
    let protocol_state = TransactionProtocolState {
        protocol: observation.protocol,
        native_transaction_id: observation.native_transaction_id.clone(),
        native_resource: observation.native_resource,
        last_sequence_number: observation.sequence_number,
        last_event: event_name(observation.event).to_owned(),
        last_trigger_reason: observation.trigger_reason.clone(),
        last_event_at: observation.occurred_at,
        last_event_fingerprint: observation.payload_fingerprint.clone(),
    };
    if let Some(index) = current {
        let transaction = &mut snapshot.transactions[index];
        transaction.state = state;
        transaction.ended_at = ended_at;
        transaction.protocol_state = Some(protocol_state);
    } else {
        snapshot.transactions.push(TransactionSnapshot {
            transaction_id,
            resource,
            state,
            started_at: observation.occurred_at,
            ended_at,
            protocol_state: Some(protocol_state),
        });
    }
    snapshot.observed_at = observed_at;
    Ok(TransactionApplyOutcome::Applied)
}

fn event_name(event: TransactionEventKind) -> &'static str {
    match event {
        TransactionEventKind::Started => "Started",
        TransactionEventKind::Updated => "Updated",
        TransactionEventKind::Ended => "Ended",
    }
}

fn transaction_state(observation: &TransactionEventObservation) -> TransactionState {
    if observation.event == TransactionEventKind::Ended {
        return TransactionState::Ended;
    }
    match observation.charging_state.as_deref() {
        Some("Charging") => TransactionState::Active,
        Some("SuspendedEV" | "SuspendedEVSE") => TransactionState::Suspended,
        _ => TransactionState::Pending,
    }
}

/// Invalid or stale transaction lifecycle evidence.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TransactionApplyError {
    UnsupportedProtocol,
    UnknownResource,
    InvalidIdentity,
    MissingStart,
    OutOfOrder,
    ConflictingReplay,
    AlreadyEnded,
    InvalidTransition,
}

/// Application-owned meter samples from one OCPP operation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MeasurementObservation {
    /// Negotiated protocol edition.
    pub protocol: ProtocolEdition,
    /// Original connector or EVSE address from the protocol message.
    pub native_resource: NativeProtocolReference,
    /// Charger transaction identity when the operation carries one.
    pub native_transaction_id: Option<String>,
    /// OCPP transaction sequence number when the operation carries one.
    pub sequence_number: Option<u32>,
    /// Samples normalized to canonical exact-decimal point values.
    pub values: Vec<DataPointValue>,
    /// Structured signed-meter values serialized without interpreting their signature format.
    pub signed_values: Vec<String>,
}

/// Applies a measurement observation to authoritative snapshot state before atomic persistence.
///
/// Existing point values with the same identity are replaced; unrelated values are retained.
/// EVSE zero is the OCPP 2.0.1 station-level meter and therefore updates station values.
///
/// # Errors
///
/// Returns [`MeasurementApplyError::UnknownResource`] when a non-zero native EVSE/connector is
/// absent from the canonical snapshot.
pub fn apply_measurements(
    snapshot: &mut StationSnapshot,
    observation: &MeasurementObservation,
    observed_at: UtcTimestamp,
) -> Result<(), MeasurementApplyError> {
    let target = match observation.native_resource {
        NativeProtocolReference::Ocpp201 { evse_id: 0, .. } => &mut snapshot.current_values,
        native => {
            &mut snapshot
                .resources
                .iter_mut()
                .find(|resource| resource.resource.native_protocol_reference == Some(native))
                .ok_or(MeasurementApplyError::UnknownResource)?
                .current_values
        }
    };
    for mut value in observation.values.clone() {
        value.observed_at = observed_at;
        if let Some(current) = target
            .iter_mut()
            .find(|current| current.point_id == value.point_id)
        {
            *current = value;
        } else {
            target.push(value);
        }
    }
    snapshot.observed_at = observed_at;
    Ok(())
}

/// Failure to reconcile a native meter resource with canonical station state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MeasurementApplyError {
    /// The snapshot does not contain the reported EVSE/connector.
    UnknownResource,
}

/// Application-owned subset of a charger registration observation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RegistrationObservation {
    /// Negotiated protocol edition.
    pub protocol: ProtocolEdition,
    /// Manufacturer name as reported by the station.
    pub vendor: String,
    /// Model name as reported by the station.
    pub model: String,
    /// Version-specific boot reason, absent from OCPP 1.6J.
    pub boot_reason: Option<String>,
}

/// Application-owned transaction-start evidence without model-library types.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TransactionStartObservation {
    /// Negotiated protocol edition.
    pub protocol: ProtocolEdition,
    /// Charger-assigned transaction identity when the edition supplies it at start.
    pub native_transaction_id: Option<String>,
    /// Original connector or EVSE address from the protocol message.
    pub native_resource: NativeProtocolReference,
    /// Charger-reported event time normalized to UTC.
    pub occurred_at: UtcTimestamp,
}
