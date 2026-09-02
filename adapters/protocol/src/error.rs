use std::{error::Error, fmt};

use uob_contracts::ProtocolEdition;

/// Stable, sanitized category for adapter decoding failures.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DecodeErrorKind {
    /// The outer OCPP CALL frame is malformed.
    MalformedFrame,
    /// The action is valid OCPP syntax but has no implemented application mapping yet.
    UnsupportedAction,
    /// The selected action payload fails typed or semantic validation.
    InvalidPayload,
}

/// Sanitized decode failure retaining the negotiated edition for versioned error mapping.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DecodeError {
    protocol: ProtocolEdition,
    kind: DecodeErrorKind,
}

impl DecodeError {
    pub(crate) const fn new(protocol: ProtocolEdition, kind: DecodeErrorKind) -> Self {
        Self { protocol, kind }
    }

    /// Returns the failure category without exposing payload data.
    #[must_use]
    pub const fn kind(&self) -> DecodeErrorKind {
        self.kind
    }

    /// Converts this adapter failure into a version-qualified OCPP CALLERROR description.
    #[must_use]
    pub const fn call_error(&self) -> OcppCallError {
        let (code, description) = match self.kind {
            DecodeErrorKind::MalformedFrame => (
                OcppErrorCode::FormationViolation,
                "OCPP CALL frame is malformed",
            ),
            DecodeErrorKind::UnsupportedAction => (
                OcppErrorCode::NotImplemented,
                "OCPP action is not implemented",
            ),
            DecodeErrorKind::InvalidPayload => (
                OcppErrorCode::PropertyConstraintViolation,
                "OCPP payload failed validation",
            ),
        };
        OcppCallError {
            protocol: self.protocol,
            code,
            description,
            field_path: match self.kind {
                DecodeErrorKind::InvalidPayload => Some("/"),
                _ => None,
            },
        }
    }
}

impl fmt::Display for DecodeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.call_error().fmt(formatter)
    }
}

impl Error for DecodeError {}

/// OCPP error codes emitted by the supported adapter mappings.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OcppErrorCode {
    FormationViolation,
    InternalError,
    NotImplemented,
    OccurrenceConstraintViolation,
    PropertyConstraintViolation,
    ProtocolError,
}

impl OcppErrorCode {
    /// Returns the exact JSON CALLERROR code.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::FormationViolation => "FormationViolation",
            Self::InternalError => "InternalError",
            Self::NotImplemented => "NotImplemented",
            Self::OccurrenceConstraintViolation => "OccurrenceConstraintViolation",
            Self::PropertyConstraintViolation => "PropertyConstraintViolation",
            Self::ProtocolError => "ProtocolError",
        }
    }
}

/// Sanitized CALLERROR fields selected for the negotiated protocol edition.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OcppCallError {
    pub protocol: ProtocolEdition,
    pub code: OcppErrorCode,
    pub description: &'static str,
    /// JSON Pointer to the rejected field, when validation can identify one safely.
    pub field_path: Option<&'static str>,
}

impl fmt::Display for OcppCallError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.code.as_str(), self.description)
    }
}
