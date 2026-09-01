use uob_contracts::{NativeProtocolReference, ProtocolEdition, UtcTimestamp};

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
