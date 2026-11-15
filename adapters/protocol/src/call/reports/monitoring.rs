//! `NotifyMonitoringReport` fragments (N02). The charger numbers its own fragments, so the
//! native `seqNo` orders them, unlike `ReportChargingProfiles`, which has none.
use super::{FragmentMetadata, Ingress, ReportKind, RouteContext, SharedReports, enqueue_fragment};
use crate::command_registry::monitoring201::{NOTIFY_REPORT_SCHEMA, valid_schema};
use crate::multipart::{ReportFailure, ReportFragment};
use crate::v201::remote_control::monitoring_values::{SanitizedFragment, fragment};
use uob_application::{RuntimeResourceBudget, WorkClass};

/// Returns whether the fragment is acknowledged; wire acknowledgement is never completion.
pub(super) fn route(
    registry: &SharedReports,
    payload: &serde_json::Value,
    bytes: usize,
    budget: &RuntimeResourceBudget,
) -> bool {
    let valid = valid_schema(NOTIFY_REPORT_SCHEMA, payload)
        && payload["requestId"]
            .as_i64()
            .is_some_and(|id| i32::try_from(id).is_ok())
        && payload["seqNo"]
            .as_u64()
            .is_some_and(|seq| u32::try_from(seq).is_ok());
    let request = payload["requestId"]
        .as_i64()
        .and_then(|id| i32::try_from(id).ok());
    let mut current = registry
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let Some(entry) =
        request.and_then(|request| current.routes.get_mut(&(ReportKind::Monitoring, request)))
    else {
        // Unsolicited or retired: acknowledged when well formed, never collected.
        return valid;
    };
    if !valid {
        entry
            .failure
            .send_replace(Some(ReportFailure::InvalidFragment));
        return false;
    }
    let RouteContext::Monitoring(query) = &entry.context else {
        entry
            .failure
            .send_replace(Some(ReportFailure::CorrelationMismatch));
        return true;
    };

    // Reserve the worst-case escaped expansion and typed descriptors before sanitizing/copying.
    let reservation = match budget.try_reserve(
        WorkClass::PendingRequest,
        bytes.saturating_mul(8).saturating_add(4096),
    ) {
        Ok(reservation) => reservation,
        Err(error) => {
            entry
                .failure
                .send_replace(Some(ReportFailure::Capacity(error)));
            return true;
        }
    };
    let SanitizedFragment { items, metadata } = match fragment(payload, query) {
        Ok(sanitized) => sanitized,
        Err(reason) => {
            entry.failure.send_replace(Some(reason));
            return reason != ReportFailure::InvalidFragment;
        }
    };

    let ingress = Ingress {
        fragment: ReportFragment {
            key: entry.key.clone(),
            sequence: metadata.sequence,
            more: metadata.more,
            items,
        },
        metadata: FragmentMetadata::Monitoring(metadata),
        reservation,
    };
    enqueue_fragment(entry, ingress);

    true
}
