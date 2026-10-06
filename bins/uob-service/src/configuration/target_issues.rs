//! Sanitized validation issues: stable codes and names, never rejected values.
use uob_application::ConfigurationErrorCode;
use uob_management_adapter::ValidationIssue;
use uob_target_adapter::{TargetChangeError, TargetSelectionError, TransportPolicyError};

use super::targets::TargetSection;

pub(super) fn change_issue(
    error: TargetChangeError,
    section: &TargetSection,
    selected: &str,
) -> ValidationIssue {
    match error {
        TargetChangeError::InvalidSelection(error) => selection_issue(error, section, selected),
        TargetChangeError::PendingDestinationChange => {
            issue("target.pending_destination_change", None, None)
        }
        TargetChangeError::InvalidBacklogState => issue("target.invalid_backlog_state", None, None),
        TargetChangeError::InvalidDispositionAudit => {
            issue("target.invalid_disposition_audit", None, None)
        }
        TargetChangeError::DispositionWithoutPendingChange => {
            issue("target.disposition_without_pending_change", None, None)
        }
        TargetChangeError::DeliveryDestinationMismatch => {
            issue("target.delivery_destination_mismatch", None, None)
        }
    }
}

fn selection_issue(
    error: TargetSelectionError,
    section: &TargetSection,
    selected: &str,
) -> ValidationIssue {
    let by_kind = |kind: &str| {
        section
            .targets
            .iter()
            .find(|entry| entry.kind == kind)
            .map(|entry| entry.id.clone())
    };
    match error {
        TargetSelectionError::DuplicateInstanceId(id) => {
            issue("target.duplicate_instance", Some(&id), None)
        }
        TargetSelectionError::MissingActiveTarget => {
            issue("target.missing_active_target", None, None)
        }
        TargetSelectionError::MultipleActiveTargets => {
            issue("target.multiple_active_targets", None, None)
        }
        TargetSelectionError::TargetIdMismatch => {
            issue("target.selection_mismatch", Some(selected), None)
        }
        TargetSelectionError::MissingTransportSecurity => {
            issue("target.missing_transport_security", Some(selected), None)
        }
        TargetSelectionError::InvalidTransportSecurity(error) => issue(
            match error {
                TransportPolicyError::PlaintextRequiresIsolatedDemo => {
                    "target.plaintext_requires_isolated_demo"
                }
                TransportPolicyError::ProductionVerificationRequired => {
                    "target.production_verification_required"
                }
                TransportPolicyError::CredentialsRequired => "target.credentials_required",
                TransportPolicyError::NonLoopbackListenerRequiresTlsAndCredentials => {
                    "target.non_loopback_listener_requires_tls_and_credentials"
                }
                TransportPolicyError::NonLoopbackListenerRequiresScopedCredentials => {
                    "target.non_loopback_listener_requires_scoped_credentials"
                }
            },
            Some(selected),
            None,
        ),
        TargetSelectionError::UnknownKind(kind) => {
            issue("target.unknown_kind", by_kind(&kind).as_deref(), None)
        }
        TargetSelectionError::UnavailableKind(kind) => {
            issue("target.unavailable_kind", by_kind(&kind).as_deref(), None)
        }
        TargetSelectionError::InvalidConfiguration { source, .. } => issue(
            match source.code() {
                ConfigurationErrorCode::MissingField => "target.missing_field",
                ConfigurationErrorCode::InvalidField => "target.invalid_field",
                ConfigurationErrorCode::UnknownField => "target.unknown_field",
                ConfigurationErrorCode::Unsupported => "target.unsupported_setting",
            },
            Some(selected),
            source.field_name(),
        ),
    }
}

pub(super) fn issue(code: &str, target_id: Option<&str>, field: Option<&str>) -> ValidationIssue {
    ValidationIssue {
        code: code.to_owned(),
        target_id: target_id.map(bounded_label),
        field: field.map(bounded_label),
    }
}

/// Echoed identifiers are client-chosen names, never values; still bound what is reflected.
pub(super) fn bounded_label(value: &str) -> String {
    value.chars().take(64).collect()
}
