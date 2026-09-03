use uob_contracts::{
    DataPointValue, NativeProtocolReference, ProtocolEdition, StationSnapshot, UtcTimestamp,
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
    /// One or more exact meter samples reported by the station.
    Measurements(MeasurementObservation),
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
