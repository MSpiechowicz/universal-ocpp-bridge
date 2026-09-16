//! OCPP 2.0.1 `DataTransfer` routing with opaque JSON payloads and atomic station updates.
//!
//! Vendor payloads and extensions are retained only for a selected provider or protocol adapter.
//! They never enter station snapshots, diagnostics, or ordinary debug output.

use std::{fmt, future::Future, pin::Pin, sync::Arc};

use serde_json::Value;
use uob_contracts::StationSnapshot;

use crate::StorageError;

mod persistence;

pub use persistence::{commit_outbound, commit_reply};

/// Maximum serialized JSON payload size accepted from a station or provider.
pub const MAX_DATA_BYTES: usize = 16_384;
/// Maximum exact vendor/message capabilities installed in one registry.
pub const MAX_CAPABILITIES: usize = 64;
const LAST_STATUS: &str = "ocpp201/data-transfer/status";
const RECEIVED_COUNT: &str = "ocpp201/data-transfer/received_count";
const REQUEST_BYTES: &str = "ocpp201/data-transfer/request_bytes";
const RESPONSE_BYTES: &str = "ocpp201/data-transfer/response_bytes";
const OUTBOUND_STATUS: &str = "ocpp201/data-transfer/outbound_status";

/// Opaque, bounded JSON vendor data that is safe to carry but never diagnose by default.
#[derive(Clone, Eq, PartialEq)]
pub struct OpaqueData {
    value: Value,
    encoded_bytes: usize,
}

impl OpaqueData {
    /// Creates one bounded opaque JSON value, including a deliberately present JSON `null`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidPayload`] when JSON serialization fails or exceeds
    /// [`MAX_DATA_BYTES`].
    pub fn new(value: Value) -> Result<Self, Error> {
        let mut counter = BoundedJsonSize(0);
        serde_json::to_writer(&mut counter, &value).map_err(|_| Error::InvalidPayload)?;
        Ok(Self {
            value,
            encoded_bytes: counter.0,
        })
    }

    /// Exposes the value only to the selected vendor provider or protocol adapter.
    #[must_use]
    pub const fn expose(&self) -> &Value {
        &self.value
    }

    fn validate(&self) -> Result<(), Error> {
        if self.encoded_bytes > MAX_DATA_BYTES {
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

/// Validated OCPP 2.0.1 vendor request.
#[derive(Clone, Eq, PartialEq)]
pub struct Observation {
    pub vendor_id: String,
    pub message_id: Option<String>,
    pub data: Option<OpaqueData>,
    pub custom_data: Option<OpaqueData>,
}

impl fmt::Debug for Observation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Observation(REDACTED)")
    }
}

impl Observation {
    /// Validates OCPP field bounds, opaque JSON limits, and `customData` schema shape.
    /// Empty vendor and message strings remain schema-valid and are never normalized.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidPayload`] when a field or opaque value is invalid.
    pub fn validate(&self) -> Result<(), Error> {
        validate_fields(
            &self.vendor_id,
            self.message_id.as_deref(),
            self.data.as_ref(),
            self.custom_data.as_ref(),
        )
    }
}

/// OCPP 2.0.1 `DataTransfer` outcome.
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

/// Schema-validated OCPP 2.0.1 response status detail.
#[derive(Clone, Eq, PartialEq)]
pub struct StatusInfo {
    pub reason_code: String,
    pub additional_info: Option<String>,
    pub custom_data: Option<OpaqueData>,
}

impl fmt::Debug for StatusInfo {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("StatusInfo(REDACTED)")
    }
}

impl StatusInfo {
    fn validate(&self) -> Result<(), Error> {
        if self.reason_code.chars().count() > 20
            || self
                .additional_info
                .as_deref()
                .is_some_and(|value| value.chars().count() > 512)
        {
            return Err(Error::InvalidPayload);
        }
        validate_custom_data(self.custom_data.as_ref())
    }
}

/// One vendor response, retaining opaque response fields only for the protocol adapter.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Reply {
    pub status: Status,
    pub data: Option<OpaqueData>,
    pub custom_data: Option<OpaqueData>,
    pub status_info: Option<StatusInfo>,
}

impl Reply {
    /// Validates all OCPP 2.0.1 response fields without interpreting vendor semantics.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidPayload`] when an opaque or extension field is invalid.
    pub fn validate(&self) -> Result<(), Error> {
        self.data.as_ref().map_or(Ok(()), OpaqueData::validate)?;
        validate_custom_data(self.custom_data.as_ref())?;
        self.status_info
            .as_ref()
            .map_or(Ok(()), StatusInfo::validate)
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
        validate_fields(&self.vendor_id, self.message_id.as_deref(), None, None)
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
    /// The caller MUST apply the five-second cancellation deadline before calling
    /// [`commit_reply`]. This runtime-neutral crate deliberately never times out the storage
    /// commit. Unknown-vendor precedence is preserved when the message is also unknown.
    ///
    /// # Errors
    ///
    /// Rejects invalid requests, unavailable providers, and invalid provider replies.
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
                custom_data: None,
                status_info: None,
            });
        }
        let Some((_, provider)) = self.entries.iter().find(|(capability, _)| {
            capability.vendor_id == request.vendor_id && capability.message_id == request.message_id
        }) else {
            return Ok(Reply {
                status: Status::UnknownMessageId,
                data: None,
                custom_data: None,
                status_info: None,
            });
        };
        let response = provider.handle(station, request).await?;
        response.validate_provider_response()?;
        Ok(response)
    }
}

/// Sanitized application failures from the OCPP 2.0.1 `DataTransfer` boundary.
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
            Self::NotRegistered => "station is not registered for OCPP 2.0.1",
            Self::Storage(_) => "data transfer storage failure",
        })
    }
}

impl std::error::Error for Error {}

/// Sanitized outbound `DataTransfer` delivery outcome retained without vendor content.
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

fn validate_fields(
    vendor_id: &str,
    message_id: Option<&str>,
    data: Option<&OpaqueData>,
    custom_data: Option<&OpaqueData>,
) -> Result<(), Error> {
    if vendor_id.chars().count() > 255 || message_id.is_some_and(|value| value.chars().count() > 50)
    {
        return Err(Error::InvalidPayload);
    }
    data.map_or(Ok(()), OpaqueData::validate)?;
    validate_custom_data(custom_data)
}

fn validate_custom_data(value: Option<&OpaqueData>) -> Result<(), Error> {
    let Some(value) = value else {
        return Ok(());
    };
    value.validate()?;
    let Some(object) = value.expose().as_object() else {
        return Err(Error::InvalidPayload);
    };
    let Some(vendor_id) = object.get("vendorId").and_then(Value::as_str) else {
        return Err(Error::InvalidPayload);
    };
    if vendor_id.chars().count() > 255 {
        return Err(Error::InvalidPayload);
    }
    Ok(())
}

struct BoundedJsonSize(usize);

impl std::io::Write for BoundedJsonSize {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if bytes.len() > MAX_DATA_BYTES - self.0 {
            return Err(std::io::Error::other("opaque JSON size limit exceeded"));
        }
        self.0 += bytes.len();
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
