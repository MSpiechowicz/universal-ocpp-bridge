use axum::{
    Json,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde::Serialize;

/// Stable machine-readable failures returned by the EMS/SCADA integration API.
///
/// Codes stay independent of the transport status so an integration client can branch on the
/// exact reason without parsing prose.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IntegrationErrorCode {
    /// No integration credential was presented.
    Unauthenticated,
    /// The presented credential is not a configured integration principal.
    InvalidCredential,
    /// The credential is valid but lacks the permission this resource requires.
    PermissionDenied,
    /// The path is not part of the versioned integration contract.
    UnknownResource,
    /// The method is not implemented for an existing integration resource.
    UnsupportedOperation,
    /// The listener's bounded concurrent-request budget is exhausted.
    CapacityExhausted,
}

impl IntegrationErrorCode {
    /// Returns the stable wire code shared by responses, diagnostics, and documentation.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Unauthenticated => "ems_scada_http.unauthenticated",
            Self::InvalidCredential => "ems_scada_http.invalid_credential",
            Self::PermissionDenied => "ems_scada_http.permission_denied",
            Self::UnknownResource => "ems_scada_http.unknown_resource",
            Self::UnsupportedOperation => "ems_scada_http.unsupported_operation",
            Self::CapacityExhausted => "ems_scada_http.capacity_exhausted",
        }
    }

    const fn status(self) -> StatusCode {
        match self {
            Self::Unauthenticated | Self::InvalidCredential => StatusCode::UNAUTHORIZED,
            Self::PermissionDenied => StatusCode::FORBIDDEN,
            Self::UnknownResource => StatusCode::NOT_FOUND,
            Self::UnsupportedOperation => StatusCode::METHOD_NOT_ALLOWED,
            Self::CapacityExhausted => StatusCode::SERVICE_UNAVAILABLE,
        }
    }
}

/// Sanitized integration error body; it never carries credential, path, or payload echoes.
#[derive(Debug, Serialize)]
pub(crate) struct IntegrationErrorBody {
    error: &'static str,
}

impl IntoResponse for IntegrationErrorCode {
    fn into_response(self) -> Response {
        let mut response = (
            self.status(),
            Json(IntegrationErrorBody {
                error: self.as_str(),
            }),
        )
            .into_response();
        if matches!(self, Self::Unauthenticated | Self::InvalidCredential) {
            response.headers_mut().insert(
                axum::http::header::WWW_AUTHENTICATE,
                axum::http::HeaderValue::from_static("Bearer"),
            );
        }
        response
    }
}

#[cfg(test)]
mod tests {
    use super::IntegrationErrorCode;

    #[test]
    fn every_code_is_namespaced_and_distinct() {
        let codes = [
            IntegrationErrorCode::Unauthenticated,
            IntegrationErrorCode::InvalidCredential,
            IntegrationErrorCode::PermissionDenied,
            IntegrationErrorCode::UnknownResource,
            IntegrationErrorCode::UnsupportedOperation,
            IntegrationErrorCode::CapacityExhausted,
        ];
        let mut seen = std::collections::BTreeSet::new();
        for code in codes {
            assert!(code.as_str().starts_with("ems_scada_http."), "{code:?}");
            assert!(seen.insert(code.as_str()), "duplicate {code:?}");
        }
    }
}
