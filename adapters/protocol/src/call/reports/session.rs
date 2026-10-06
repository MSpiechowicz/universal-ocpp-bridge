//! Socket-handle entry points for registering native report routes.
use super::{ReportKind, ReportRoute, RouteContext, disconnect, enable, register};
use crate::call::types::{CallSessionHandle, OutboundCall, PendingCall, SessionSubmitError};
use tokio::time::Instant;
use uob_contracts::CorrelationId;

/// Each requesting action may open only its own native report action.
fn paired(request: &str, context: &RouteContext) -> bool {
    match context {
        RouteContext::Device => ["GetBaseReport", "GetReport"].contains(&request),
        RouteContext::ChargingProfiles(_) => {
            request == crate::command_registry::charging_profiles201::ACTION
        }
    }
}

impl CallSessionHandle {
    pub(crate) fn enable_notify_reports(&self) {
        enable(&self.reports, ReportKind::Device);
    }
    pub(crate) fn enable_profile_reports(&self) {
        enable(&self.reports, ReportKind::ChargingProfiles);
    }
    pub(crate) fn report_store_failed(&self, correlation: CorrelationId) {
        self.diagnostics
            .span(Some(correlation), Some(self.station_id.clone()), None)
            .emit(
                uob_application::FlowStage::DurableCommit,
                uob_application::FlowEvidence::Failed,
            );
    }
    pub(crate) fn disconnect_reports(&self) {
        disconnect(&self.reports);
    }
    /// Registers the shared native report route before enqueuing its CALL.
    pub(crate) fn try_report_call_before(
        &self,
        request: OutboundCall,
        deadline: Instant,
        key: crate::multipart::ReportKey,
        resource: uob_contracts::ResourceRef,
        context: RouteContext,
    ) -> Result<(PendingCall, ReportRoute), SessionSubmitError> {
        if self.is_closed() {
            return Err(SessionSubmitError::Closed);
        }
        if key.station != self.station_id
            || resource.station_id != self.station_id
            || key.connection != self.connection_id
            || key.protocol != uob_contracts::ProtocolEdition::Ocpp201
            || self.protocol != key.protocol
            || ReportKind::from_action(key.action.as_str()) != Some(context.kind())
            || key.correlation != request.correlation_id
            || !paired(request.action.as_str(), &context)
            || request.payload["requestId"].as_i64() != Some(i64::from(key.request_id))
        {
            return Err(SessionSubmitError::InvalidRequest);
        }
        let admission = crate::multipart::reserve_report(
            key.clone(),
            crate::multipart::ReportLimits::default(),
            &self.budget,
        )
        .map_err(|_| SessionSubmitError::Full)?;
        let (route, started) = register(&self.reports, key, resource, context, admission)?;
        let pending = self.enqueue_observed(request, Some(deadline), None, Some(started))?;
        Ok((pending, route))
    }

    /// Budget used by the single socket owner, shared with report collection.
    pub(crate) fn resource_budget(&self) -> uob_application::RuntimeResourceBudget {
        self.budget.clone()
    }
    /// Opaque identity of the authenticated connection, never shared across reconnects.
    pub(crate) fn connection_id(&self) -> CorrelationId {
        self.connection_id.clone()
    }
}
