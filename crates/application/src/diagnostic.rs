//! Single diagnostic serialization boundary.

use std::{collections::BTreeMap, error::Error, fmt, sync::Arc};

use uob_contracts::{
    ContractVersion, CorrelationId, ProcessInstanceId, ProtocolActionName, RedactedTraceDetails,
    StationId, TargetInstanceId, TargetKind, TraceDirection, TraceId, TraceOutcome, TraceRecord,
    TraceSequence, TraceStage, TraceTarget, UtcTimestamp,
};

const REDACTED: &str = "[REDACTED]";
const OMITTED_VENDOR_PAYLOAD: &str = "[OMITTED: unknown_vendor_payload]";

/// A closed set of sensitive values accepted by the diagnostic boundary.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum SensitiveDataClass {
    /// An authorization header, bearer token, id token, or equivalent secret.
    AuthorizationToken,
    /// A password, private key, client secret, or transport credential.
    Credential,
    /// Credential material embedded in or associated with an endpoint.
    EndpointSecret,
    /// Payment data that is not an explicitly safe provider reference.
    PaymentSensitive,
}

impl SensitiveDataClass {
    const fn audit_name(self) -> &'static str {
        match self {
            Self::AuthorizationToken => "authorization_token",
            Self::Credential => "credential",
            Self::EndpointSecret => "endpoint_secret",
            Self::PaymentSensitive => "payment_sensitive",
        }
    }
}

/// Sensitive source data which cannot be serialized or printed by diagnostics.
pub struct SensitiveDiagnosticValue {
    class: SensitiveDataClass,
    _value: Box<str>,
}

impl SensitiveDiagnosticValue {
    /// Classifies a raw value before it reaches the observation boundary.
    #[must_use]
    pub fn new(class: SensitiveDataClass, value: impl Into<Box<str>>) -> Self {
        Self {
            class,
            _value: value.into(),
        }
    }
}

impl fmt::Debug for SensitiveDiagnosticValue {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SensitiveDiagnosticValue")
            .field("class", &self.class)
            .field("_value", &REDACTED)
            .finish()
    }
}

/// Opaque vendor data which is never inspected or serialized by diagnostics.
pub struct UnknownVendorPayload {
    contents: Box<[u8]>,
}

impl UnknownVendorPayload {
    /// Wraps vendor data before diagnostic handling.
    #[must_use]
    pub fn new(contents: impl Into<Box<[u8]>>) -> Self {
        Self {
            contents: contents.into(),
        }
    }

    /// Reports source size without exposing source contents.
    #[must_use]
    pub fn original_size(&self) -> usize {
        self.contents.len()
    }
}

impl fmt::Debug for UnknownVendorPayload {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("UnknownVendorPayload")
            .field("original_size", &self.original_size())
            .finish_non_exhaustive()
    }
}

/// A configured label deliberately separated from a raw endpoint URI.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SafeEndpointLabel(String);

impl SafeEndpointLabel {
    /// Creates a display label, rejecting URI- and credential-shaped input.
    ///
    /// # Errors
    ///
    /// Returns [`SafeEndpointLabelError`] for empty, oversized, or address-shaped input.
    pub fn new(value: impl Into<String>) -> Result<Self, SafeEndpointLabelError> {
        let value = value.into();
        if value.trim().is_empty() {
            return Err(SafeEndpointLabelError::Empty);
        }
        if value.len() > 128 {
            return Err(SafeEndpointLabelError::TooLong);
        }
        if value.chars().any(char::is_whitespace)
            || value.contains("://")
            || value.contains(['@', '?', '#', '=', '&', '/', '\\', ':'])
        {
            return Err(SafeEndpointLabelError::AddressOrCredentialMaterial);
        }
        Ok(Self(value))
    }

    /// Returns the inert configured label.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Invalid safe endpoint label.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SafeEndpointLabelError {
    /// The label contains no visible characters.
    Empty,
    /// The label exceeds the bounded diagnostic representation.
    TooLong,
    /// The value resembles an address, query string, user-info, or credential material.
    AddressOrCredentialMaterial,
}

impl fmt::Display for SafeEndpointLabelError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Empty => "safe endpoint label cannot be empty",
            Self::TooLong => "safe endpoint label is too long",
            Self::AddressOrCredentialMaterial => {
                "safe endpoint label cannot contain address or credential material"
            }
        })
    }
}

impl Error for SafeEndpointLabelError {}

/// Explicitly safe scalar fields supported by diagnostic rendering.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SafeDiagnosticField {
    /// Stable protocol operation name.
    Action(ProtocolActionName),
    /// Canonical station identity.
    Station(StationId),
    /// Correlation identity already approved for external contracts.
    Correlation(CorrelationId),
    /// Configured display label, never a raw destination address.
    EndpointLabel(SafeEndpointLabel),
    /// Source payload size without payload contents.
    PayloadBytes(u64),
}

impl SafeDiagnosticField {
    fn render(&self) -> (&'static str, String) {
        match self {
            Self::Action(value) => ("action", value.as_str().to_owned()),
            Self::Station(value) => ("station_id", value.as_str().to_owned()),
            Self::Correlation(value) => ("correlation_id", value.as_str().to_owned()),
            Self::EndpointLabel(value) => ("endpoint_label", value.as_str().to_owned()),
            Self::PayloadBytes(value) => ("payload_bytes", value.to_string()),
        }
    }
}

/// Raw attribute variants accepted at the service observation boundary.
#[derive(Debug)]
pub enum DiagnosticAttribute {
    /// A closed, explicitly safe scalar representation.
    Safe(SafeDiagnosticField),
    /// A classified value replaced with a stable marker.
    Sensitive(SensitiveDiagnosticValue),
    /// Unknown vendor content omitted without field-name inspection.
    UnknownVendorPayload(UnknownVendorPayload),
}

/// Fixed summaries which cannot interpolate raw external input.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DiagnosticSummary {
    /// A payload crossed an observation boundary.
    PayloadObserved,
    /// Validation rejected an input.
    ValidationRejected,
    /// A bounded operation completed.
    OperationCompleted,
    /// Policy rejected an operation.
    OperationRejected,
}

impl DiagnosticSummary {
    const fn as_str(self) -> &'static str {
        match self {
            Self::PayloadObserved => "payload observed",
            Self::ValidationRejected => "validation rejected",
            Self::OperationCompleted => "operation completed",
            Self::OperationRejected => "operation rejected",
        }
    }
}

/// Stable outcomes which cannot carry an arbitrary external error string.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DiagnosticOutcome {
    /// The observed stage succeeded.
    Succeeded,
    /// Input validation failed.
    InvalidInput,
    /// Authorization or runtime policy denied the operation.
    PolicyDenied,
    /// A bounded operation timed out.
    Timeout,
    /// Optional diagnostic work was dropped under resource pressure.
    ResourcePressure,
}

impl DiagnosticOutcome {
    fn into_contract(self) -> TraceOutcome {
        match self {
            Self::Succeeded => TraceOutcome::Succeeded,
            Self::InvalidInput => TraceOutcome::Failed {
                reason: Some("invalid_input".to_owned()),
            },
            Self::PolicyDenied => TraceOutcome::Failed {
                reason: Some("policy_denied".to_owned()),
            },
            Self::Timeout => TraceOutcome::Uncertain {
                reason: Some("timeout".to_owned()),
            },
            Self::ResourcePressure => TraceOutcome::Dropped {
                reason: Some("resource_pressure".to_owned()),
            },
        }
    }
}

/// Trusted metadata used to construct one diagnostic trace record.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DiagnosticTraceContext {
    /// Unique trace identity.
    pub trace_id: TraceId,
    /// Process that owns the trace sequence.
    pub process_instance_id: ProcessInstanceId,
    /// Monotonic sequence within the process.
    pub trace_sequence: TraceSequence,
    /// Optional selected-target context.
    pub target: Option<(TargetInstanceId, TargetKind)>,
    /// Optional cross-pipeline correlation.
    pub correlation_id: Option<CorrelationId>,
    /// Optional causal trace identity.
    pub parent_trace_id: Option<TraceId>,
    /// Stable pipeline stage.
    pub stage: TraceStage,
    /// Direction at the stage.
    pub direction: TraceDirection,
    /// Server-observed timestamp.
    pub observed_at: UtcTimestamp,
    /// Optional monotonic duration.
    pub duration_micros: Option<u64>,
    /// Closed sanitized outcome.
    pub outcome: DiagnosticOutcome,
}

/// Source details consumed exactly once by [`DiagnosticBoundary`].
#[derive(Debug)]
pub struct DiagnosticObservation {
    /// Fixed safe summary.
    pub summary: DiagnosticSummary,
    /// Typed fields, sensitive values, and opaque vendor content.
    pub attributes: Vec<DiagnosticAttribute>,
}

/// Safe evidence of every disclosure decision made for a record.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct DiagnosticDisclosureAudit {
    exposed_fields: Vec<&'static str>,
    redacted_classes: Vec<&'static str>,
    omitted_unknown_vendor_payloads: usize,
}

impl DiagnosticDisclosureAudit {
    /// Names the safe field classes exposed in the inert record.
    #[must_use]
    pub fn exposed_fields(&self) -> &[&'static str] {
        &self.exposed_fields
    }

    /// Names the sensitive classes replaced by markers.
    #[must_use]
    pub fn redacted_classes(&self) -> &[&'static str] {
        &self.redacted_classes
    }

    /// Counts unknown vendor payloads omitted without inspection.
    #[must_use]
    pub const fn omitted_unknown_vendor_payloads(&self) -> usize {
        self.omitted_unknown_vendor_payloads
    }
}

/// Inert bytes shared unchanged by logs, trace rings, broadcasts, exports, and API errors.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SanitizedDiagnostic {
    encoded_json: Arc<[u8]>,
    audit: DiagnosticDisclosureAudit,
}

impl SanitizedDiagnostic {
    /// Returns the one centrally serialized representation for every downstream sink.
    #[must_use]
    pub fn encoded_json(&self) -> &[u8] {
        &self.encoded_json
    }

    /// Returns safe disclosure evidence without source values.
    #[must_use]
    pub const fn audit(&self) -> &DiagnosticDisclosureAudit {
        &self.audit
    }
}

/// Central service boundary that redacts and serializes before any sink sees a record.
#[derive(Clone, Copy, Debug, Default)]
pub struct DiagnosticBoundary;

impl DiagnosticBoundary {
    /// Consumes raw details and returns only a shared inert serialization.
    ///
    /// # Errors
    ///
    /// Returns [`DiagnosticSerializationError`] if the closed trace contract cannot serialize.
    pub fn serialize(
        self,
        context: DiagnosticTraceContext,
        observation: DiagnosticObservation,
    ) -> Result<SanitizedDiagnostic, DiagnosticSerializationError> {
        let mut fields = BTreeMap::new();
        let mut audit = DiagnosticDisclosureAudit::default();
        let mut omitted = false;

        for attribute in observation.attributes {
            match attribute {
                DiagnosticAttribute::Safe(field) => {
                    let (name, value) = field.render();
                    fields.insert(name.to_owned(), value);
                    audit.exposed_fields.push(name);
                }
                DiagnosticAttribute::Sensitive(value) => {
                    let class = value.class.audit_name();
                    fields.insert(format!("redacted.{class}"), REDACTED.to_owned());
                    audit.redacted_classes.push(class);
                }
                DiagnosticAttribute::UnknownVendorPayload(_) => {
                    fields.insert(
                        "vendor_payload".to_owned(),
                        OMITTED_VENDOR_PAYLOAD.to_owned(),
                    );
                    audit.omitted_unknown_vendor_payloads += 1;
                    omitted = true;
                }
            }
        }

        fields.insert(
            "audit.exposed_fields".to_owned(),
            audit.exposed_fields.join(","),
        );
        fields.insert(
            "audit.redacted_classes".to_owned(),
            audit.redacted_classes.join(","),
        );
        fields.insert(
            "audit.omitted_unknown_vendor_payloads".to_owned(),
            audit.omitted_unknown_vendor_payloads.to_string(),
        );

        let target = context
            .target
            .map(|(instance_id, kind)| TraceTarget { instance_id, kind });
        let record = TraceRecord {
            schema_version: ContractVersion::V1_INITIAL,
            trace_id: context.trace_id,
            process_instance_id: context.process_instance_id,
            trace_sequence: context.trace_sequence,
            target,
            correlation_id: context.correlation_id,
            parent_trace_id: context.parent_trace_id,
            stage: context.stage,
            direction: context.direction,
            observed_at: context.observed_at,
            duration_micros: context.duration_micros,
            outcome: context.outcome.into_contract(),
            redacted_details: Some(RedactedTraceDetails {
                summary: Some(observation.summary.as_str().to_owned()),
                fields,
                truncated: omitted,
            }),
        };
        let encoded_json = serde_json::to_vec(&record).map_err(DiagnosticSerializationError)?;
        Ok(SanitizedDiagnostic {
            encoded_json: Arc::from(encoded_json),
            audit,
        })
    }
}

/// Closed-contract serialization failure.
#[derive(Debug)]
pub struct DiagnosticSerializationError(serde_json::Error);

impl fmt::Display for DiagnosticSerializationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("failed to serialize sanitized diagnostic record")
    }
}

impl Error for DiagnosticSerializationError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        Some(&self.0)
    }
}

#[cfg(test)]
mod tests;
