//! One ordered handler joining socket metadata, actual persistence, and safe state projection.
use super::IncomingCall;
use crate::{OcppCallError, v16, v201};
use uob_application::{
    DiagnosticState, DiagnosticStore, FlowEvidence, FlowStage, OperationalStore,
    registration::RegistrationDecision,
};
use uob_contracts::{ProtocolEdition, StationSnapshot, UtcTimestamp};
impl IncomingCall {
    /// Applies a registration/status call with its original trace and bounded socket responder.
    /// # Errors
    /// Returns the unchanged lifecycle error, or a sanitized response-queue failure.
    pub async fn complete_registration<
        C: Send + 'static,
        E: Send + 'static,
        D: Send + 'static,
        R: Send + 'static,
    >(
        self,
        store: &dyn OperationalStore<C, E, D, R>,
        snapshot: &mut StationSnapshot,
        decision: RegistrationDecision,
        interval_seconds: u32,
        now: UtcTimestamp,
    ) -> Result<(), OcppCallError> {
        let protocol = self.responder.protocol;
        let before = DiagnosticState::capture(snapshot);
        let source_time = match &self.call.observation {
            uob_application::ChargerObservation::ConnectorStatus(status) => status.source_time,
            uob_application::ChargerObservation::EvseConnectorStatus(status) => {
                Some(status.source_time)
            }
            _ => None,
        };
        if let Some(time) = source_time {
            self.trace.source_time(FlowStage::Application, time);
        }
        let store = DiagnosticStore::new(store, self.trace.clone());
        let result = match protocol {
            ProtocolEdition::Ocpp16j => {
                v16::complete_registration(
                    self.call,
                    &store,
                    snapshot,
                    decision,
                    interval_seconds,
                    now,
                )
                .await
            }
            ProtocolEdition::Ocpp201 => {
                v201::complete_registration(
                    self.call,
                    &store,
                    snapshot,
                    decision,
                    interval_seconds,
                    now,
                )
                .await
            }
        };
        match result {
            Ok(response) => {
                self.trace.emit(
                    FlowStage::Application,
                    if source_time.is_some() && !store.committed() {
                        FlowEvidence::Stale
                    } else {
                        FlowEvidence::Completed
                    },
                );
                before.emit_changes(snapshot, &self.trace);
                self.responder
                    .respond(&response[2])
                    .map_err(|_| crate::OcppCallError {
                        protocol,
                        code: crate::OcppErrorCode::InternalError,
                        description: "response queue unavailable",
                        field_path: None,
                    })
            }
            Err(error) => {
                self.trace
                    .emit(FlowStage::Application, FlowEvidence::Rejected);
                // Preserve the existing responder's bounded error path.
                let _ = self.responder.reject(error);
                Err(error)
            }
        }
    }
}
