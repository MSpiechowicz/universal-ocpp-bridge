//! OCPP 1.6J `DataTransfer` routing with opaque payload handling and atomic station updates.
//!
//! Vendor payloads are opaque at this boundary. They are never retained in a station snapshot,
//! diagnostic value, or default debug output.

use std::{fmt, future::Future, pin::Pin, sync::Arc};

use uob_contracts::{StationSnapshot, TypedValue, UtcTimestamp};

use crate::{AtomicStoreWrite, OperationalStore, StorageError, registration};

/// Maximum UTF-8 payload size accepted from a station or provider.
pub const MAX_DATA_BYTES: usize = 16_384;
/// Maximum exact vendor/message capabilities installed in one registry.
pub const MAX_CAPABILITIES: usize = 64;
const LAST_STATUS: &str = "ocpp16/data-transfer/status";
const RECEIVED_COUNT: &str = "ocpp16/data-transfer/received_count";
const REQUEST_BYTES: &str = "ocpp16/data-transfer/request_bytes";
const RESPONSE_BYTES: &str = "ocpp16/data-transfer/response_bytes";
const OUTBOUND_STATUS: &str = "ocpp16/data-transfer/outbound_status";
/// Opaque, bounded vendor data that is safe to carry but never to diagnose by default.
#[derive(Clone, Eq, PartialEq)]
pub struct OpaqueData(String);

impl OpaqueData {
    /// Creates one bounded opaque payload.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidPayload`] when the UTF-8 representation exceeds
    /// [`MAX_DATA_BYTES`].
    pub fn new(value: String) -> Result<Self, Error> {
        if value.len() > MAX_DATA_BYTES {
            return Err(Error::InvalidPayload);
        }
        Ok(Self(value))
    }

    /// Exposes the payload only to the selected vendor provider or protocol adapter.
    #[must_use]
    pub fn expose(&self) -> &str {
        &self.0
    }

    fn validate(&self) -> Result<(), Error> {
        if self.0.len() > MAX_DATA_BYTES {
            return Err(Error::InvalidPayload);
        }
        Ok(())
    }
}

impl fmt::Debug for OpaqueData {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("OpaqueData(REDACTED)")
    }
}

/// Validated OCPP 1.6J vendor request.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Observation {
    pub vendor_id: String,
    pub message_id: Option<String>,
    pub data: Option<OpaqueData>,
}

impl Observation {
    /// Validates OCPP schema field bounds and opaque payload bounds at the application boundary.
    /// Empty vendor and message strings are schema-valid and are deliberately not normalized.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidPayload`] when a supplied field exceeds its OCPP bound.
    pub fn validate(&self) -> Result<(), Error> {
        validate_fields(
            &self.vendor_id,
            self.message_id.as_deref(),
            self.data.as_ref(),
        )
    }
}

fn validate_fields(
    vendor_id: &str,
    message_id: Option<&str>,
    data: Option<&OpaqueData>,
) -> Result<(), Error> {
    if vendor_id.chars().count() > 255 || message_id.is_some_and(|value| value.chars().count() > 50)
    {
        return Err(Error::InvalidPayload);
    }
    data.map_or(Ok(()), OpaqueData::validate)
}

/// OCPP 1.6J `DataTransfer` outcome.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Status {
    Accepted,
    Rejected,
    UnknownVendorId,
    UnknownMessageId,
}

impl Status {
    /// Returns the exact OCPP status token.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Accepted => "Accepted",
            Self::Rejected => "Rejected",
            Self::UnknownVendorId => "UnknownVendorId",
            Self::UnknownMessageId => "UnknownMessageId",
        }
    }
}

/// One vendor response, retaining opaque response data only for the protocol adapter.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Reply {
    pub status: Status,
    pub data: Option<OpaqueData>,
}

impl Reply {
    fn validate(&self) -> Result<(), Error> {
        if self.status == Status::UnknownVendorId && self.data.is_some() {
            return Err(Error::InvalidPayload);
        }
        self.data.as_ref().map_or(Ok(()), OpaqueData::validate)
    }

    fn validate_provider_response(&self) -> Result<(), Error> {
        if !matches!(self.status, Status::Accepted | Status::Rejected) {
            return Err(Error::InvalidPayload);
        }
        self.validate()
    }
}

/// One exact vendor/message pair supported by a provider.
///
/// `None` is an exact omitted `messageId`, not a wildcard.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct Capability {
    pub vendor_id: String,
    pub message_id: Option<String>,
}

impl Capability {
    fn validate(&self) -> Result<(), Error> {
        validate_fields(&self.vendor_id, self.message_id.as_deref(), None)
    }
}

/// Provider installed by composition for one or more exact vendor capabilities.
///
/// `handle` is evaluated only after exact routing. It MUST have no automatic retries or external
/// side effects because its caller may cancel the future to enforce a bounded deadline.
pub trait Provider: Send + Sync {
    fn handle<'a>(
        &'a self,
        station: &'a StationSnapshot,
        request: &'a Observation,
    ) -> Pin<Box<dyn Future<Output = Result<Reply, Error>> + Send + 'a>>;
}

/// Immutable exact capability routing table.
pub struct Registry {
    entries: Vec<(Capability, Arc<dyn Provider>)>,
}

impl Registry {
    /// Validates a bounded, duplicate-free provider routing table.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidRegistry`] for an over-limit, invalid, or duplicate capability.
    pub fn new(entries: Vec<(Capability, Arc<dyn Provider>)>) -> Result<Self, Error> {
        if entries.len() > MAX_CAPABILITIES {
            return Err(Error::InvalidRegistry);
        }
        for (index, (capability, _)) in entries.iter().enumerate() {
            capability.validate().map_err(|_| Error::InvalidRegistry)?;
            if entries[..index]
                .iter()
                .any(|(other, _)| other == capability)
            {
                return Err(Error::InvalidRegistry);
            }
        }
        Ok(Self { entries })
    }

    /// Returns whether a request has one exact installed vendor/message capability.
    ///
    /// `None` only matches an omitted `messageId`; it never expands to another capability.
    #[must_use]
    pub fn supports(&self, request: &Observation) -> bool {
        request.validate().is_ok()
            && self.entries.iter().any(|(capability, _)| {
                capability.vendor_id == request.vendor_id
                    && capability.message_id == request.message_id
            })
    }

    /// Routes a validated request to one exact provider.
    ///
    /// The caller MUST apply the five-second cancellation deadline to this provider evaluation before
    /// calling [`commit_reply`]. This runtime-neutral application crate deliberately never times out
    /// the subsequent authoritative storage commit.
    ///
    /// Unknown-vendor precedence is preserved even when its `messageId` is also unknown.
    ///
    /// # Errors
    /// Rejects invalid input, provider failures, and invalid provider response statuses.
    pub async fn route(
        &self,
        station: &StationSnapshot,
        request: &Observation,
    ) -> Result<Reply, Error> {
        request.validate()?;
        let vendor_exists = self
            .entries
            .iter()
            .any(|(capability, _)| capability.vendor_id == request.vendor_id);
        if !vendor_exists {
            return Ok(Reply {
                status: Status::UnknownVendorId,
                data: None,
            });
        }
        let Some((_, provider)) = self.entries.iter().find(|(capability, _)| {
            capability.vendor_id == request.vendor_id && capability.message_id == request.message_id
        }) else {
            return Ok(Reply {
                status: Status::UnknownMessageId,
                data: None,
            });
        };
        let response = provider.handle(station, request).await?;
        response.validate_provider_response()?;
        Ok(response)
    }
}

/// Sanitized application failures from the `DataTransfer` boundary.
#[derive(Debug)]
pub enum Error {
    InvalidPayload,
    InvalidRegistry,
    ProviderUnavailable,
    NotRegistered,
    Storage(StorageError),
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::InvalidPayload => "invalid data transfer payload",
            Self::InvalidRegistry => "invalid data transfer registry",
            Self::ProviderUnavailable => "data transfer provider unavailable",
            Self::NotRegistered => "station is not registered for OCPP 1.6J",
            Self::Storage(_) => "data transfer storage failure",
        })
    }
}

impl std::error::Error for Error {}

/// Sanitized outbound `DataTransfer` delivery outcome retained without vendor request content.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OutboundStatus {
    TransmissionUncertain,
    NotTransmitted,
    TimedOut,
    CallError,
    Reply(Status),
}

impl OutboundStatus {
    /// Returns a fixed OCPP outcome token safe for station snapshots.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::TransmissionUncertain => "TransmissionUncertain",
            Self::NotTransmitted => "NotTransmitted",
            Self::TimedOut => "TimedOut",
            Self::CallError => "CallError",
            Self::Reply(status) => status.as_str(),
        }
    }
}

/// Atomically records a previously routed OCPP 1.6J `DataTransfer` reply.
///
/// Vendor identifiers, message identifiers, and opaque data are never persisted. The caller's
/// snapshot changes only after the complete storage write commits successfully.
///
/// # Errors
///
/// Returns [`Error::NotRegistered`] for any non-current accepted OCPP 1.6J registration and
/// leaves the supplied snapshot unchanged for validation or storage failures.
pub async fn commit_reply<
    C: Send + 'static,
    E: Send + 'static,
    D: Send + 'static,
    R: Send + 'static,
>(
    store: &dyn OperationalStore<C, E, D, R>,
    snapshot: &mut StationSnapshot,
    request: &Observation,
    reply: Reply,
    now: UtcTimestamp,
) -> Result<Reply, Error> {
    registration::accepted(snapshot).map_err(|_| Error::NotRegistered)?;
    request.validate()?;
    reply.validate()?;
    let mut next = snapshot.clone();
    registration::set(
        &mut next.current_values,
        LAST_STATUS,
        Some(TypedValue::Text(reply.status.as_str().to_owned())),
        None,
        now,
    );
    registration::set(
        &mut next.current_values,
        RECEIVED_COUNT,
        Some(TypedValue::UnsignedInteger(received_count(snapshot))),
        None,
        now,
    );
    registration::set(
        &mut next.current_values,
        REQUEST_BYTES,
        Some(TypedValue::UnsignedInteger(request_bytes(request))),
        None,
        now,
    );
    registration::set(
        &mut next.current_values,
        RESPONSE_BYTES,
        Some(TypedValue::UnsignedInteger(response_bytes(&reply))),
        None,
        now,
    );
    registration::activity(&mut next, now);
    let mut write = AtomicStoreWrite::empty();
    write.station_snapshot = Some(next.clone());
    store.write_atomic(write).await.map_err(Error::Storage)?;
    *snapshot = next;
    Ok(reply)
}

/// Atomically records one sanitized outbound `DataTransfer` outcome.
///
/// This is intended for an adapter to persist [`OutboundStatus::TransmissionUncertain`] before
/// enqueueing the CALL and a final status after transport completion. It never records the vendor,
/// message ID, or opaque request/response data, and does not retry transport.
///
/// # Errors
///
/// Returns [`Error::NotRegistered`] unless the snapshot has a current accepted OCPP 1.6J
/// registration. The caller's snapshot remains unchanged when validation or storage fails.
pub async fn commit_outbound<
    C: Send + 'static,
    E: Send + 'static,
    D: Send + 'static,
    R: Send + 'static,
>(
    store: &dyn OperationalStore<C, E, D, R>,
    snapshot: &mut StationSnapshot,
    status: OutboundStatus,
    now: UtcTimestamp,
) -> Result<(), Error> {
    registration::accepted(snapshot).map_err(|_| Error::NotRegistered)?;
    let mut next = snapshot.clone();
    registration::set(
        &mut next.current_values,
        OUTBOUND_STATUS,
        Some(TypedValue::Text(status.as_str().to_owned())),
        None,
        now,
    );
    registration::activity(&mut next, now);
    let mut write = AtomicStoreWrite::empty();
    write.station_snapshot = Some(next.clone());
    store.write_atomic(write).await.map_err(Error::Storage)?;
    *snapshot = next;
    Ok(())
}

fn received_count(snapshot: &StationSnapshot) -> u64 {
    snapshot
        .current_values
        .iter()
        .find(|value| value.point_id.as_str() == RECEIVED_COUNT)
        .and_then(|value| match value.value {
            Some(TypedValue::UnsignedInteger(count)) => Some(count),
            _ => None,
        })
        .unwrap_or(0)
        .saturating_add(1)
}

fn request_bytes(request: &Observation) -> u64 {
    (request.vendor_id.len()
        + request.message_id.as_ref().map_or(0, String::len)
        + request.data.as_ref().map_or(0, |data| data.expose().len())) as u64
}

fn response_bytes(reply: &Reply) -> u64 {
    (reply.status.as_str().len() + reply.data.as_ref().map_or(0, |data| data.expose().len())) as u64
}
