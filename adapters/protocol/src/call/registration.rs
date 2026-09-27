//! One ordered handler joining socket metadata, actual persistence, and safe state projection.
use super::{IncomingCall, IncomingCallResponder};
use crate::{OcppCallError, v16, v201};
use uob_application::{
    DiagnosticState, DiagnosticStore, FlowEvidence, FlowSpan, FlowStage, OperationalStore,
    registration::RegistrationDecision,
};
use uob_contracts::{EventEnvelope, ProtocolEdition, StationSnapshot, UtcTimestamp};
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
        self.complete_registration_inner(store, snapshot, decision, interval_seconds, now, None)
            .await
    }

    /// Completes a live call with a station-scoped invalidation in its snapshot transaction.
    /// # Errors
    /// Returns unchanged lifecycle errors or a sanitized response-queue failure.
    pub async fn complete_registration_with_invalidation<
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
        invalidation: EventEnvelope<E>,
    ) -> Result<(), OcppCallError> {
        self.complete_registration_inner(
            store,
            snapshot,
            decision,
            interval_seconds,
            now,
            Some((invalidation, None)),
        )
        .await
    }

    /// Completes an OCPP Boot/Heartbeat with both its usual invalidation and
    /// a correlated trigger marker in the same durable write.
    /// # Errors
    /// Returns unchanged lifecycle errors or a sanitized response-queue failure.
    pub async fn complete_registration_with_trigger<
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
        events: (EventEnvelope<E>, Option<EventEnvelope<E>>),
    ) -> Result<(), OcppCallError> {
        self.complete_registration_inner(
            store,
            snapshot,
            decision,
            interval_seconds,
            now,
            Some(events),
        )
        .await
    }

    async fn complete_registration_inner<
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
        events: Option<(EventEnvelope<E>, Option<EventEnvelope<E>>)>,
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
            ProtocolEdition::Ocpp16j => match events {
                Some(events @ (_, Some(_))) => {
                    v16::complete_registration_with_trigger(
                        self.call,
                        &store,
                        snapshot,
                        decision,
                        interval_seconds,
                        now,
                        events,
                    )
                    .await
                }
                Some((event, None)) => {
                    v16::complete_registration_with_invalidation(
                        self.call,
                        &store,
                        snapshot,
                        decision,
                        interval_seconds,
                        now,
                        event,
                    )
                    .await
                }
                None => {
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
            },
            ProtocolEdition::Ocpp201 => match events {
                Some(events @ (_, Some(_))) => {
                    v201::complete_registration_with_trigger(
                        self.call,
                        &store,
                        snapshot,
                        decision,
                        interval_seconds,
                        now,
                        events,
                    )
                    .await
                }
                Some((event, None)) => {
                    v201::complete_registration_with_invalidation(
                        self.call,
                        &store,
                        snapshot,
                        decision,
                        interval_seconds,
                        now,
                        event,
                    )
                    .await
                }
                None => {
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
            },
        };
        let stale = result.is_ok() && source_time.is_some() && !store.committed();
        Self::finish_registration(
            &self.trace,
            self.responder,
            snapshot,
            &before,
            stale,
            result,
        )
    }

    fn finish_registration(
        trace: &FlowSpan,
        responder: IncomingCallResponder,
        snapshot: &StationSnapshot,
        before: &DiagnosticState,
        stale: bool,
        result: Result<serde_json::Value, OcppCallError>,
    ) -> Result<(), OcppCallError> {
        let protocol = responder.protocol;
        match result {
            Ok(response) => {
                trace.emit(
                    FlowStage::Application,
                    if stale {
                        FlowEvidence::Stale
                    } else {
                        FlowEvidence::Completed
                    },
                );
                before.emit_changes(snapshot, trace);
                responder.respond(&response[2]).map_err(|_| OcppCallError {
                    protocol,
                    code: crate::OcppErrorCode::InternalError,
                    description: "response queue unavailable",
                    field_path: None,
                })
            }
            Err(error) => {
                trace.emit(FlowStage::Application, FlowEvidence::Rejected);
                // Preserve the existing responder's bounded error path.
                let _ = responder.reject(error);
                Err(error)
            }
        }
    }
}
