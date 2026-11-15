//! Charger-initiated OCPP 2.0.1 `NotifyEvent` (N07, N08). Every call of an accepted station is
//! answered with the empty response. The station's gate either admits it as one bounded journal
//! record, committed before the answer, or sheds it: a shed call writes nothing and raises the
//! shared resource pressure signal, and the next durable record reports the loss. Journal events
//! cannot be shed by retention, so the flood budget belongs to the station for the life of the
//! process, not to one connection. Event contents are charger text; only counts and identities
//! reach traces.
use super::{CallContext, Clock, call_error, calls::commit_error, context};
use serde_json::json;
use std::{collections::BTreeMap, sync::Arc};
use tokio::sync::{Mutex, OwnedMutexGuard};
use uob_application::{
    ChargerObservation, CommandClock, FlowEvidence, FlowStage, MonitoringEventDecision201,
    MonitoringEventGate201, SafeDiagnosticField, record_monitoring_event_shed_201,
    record_monitoring_events_201,
};
use uob_contracts::{ProtocolEdition, StationId, StationSnapshot};
use uob_protocol_adapter::{IncomingCall, OcppCallError, OcppErrorCode, v201};

/// One gate per configured station, created at startup. Unknown stations never reach the call
/// loop, and nothing is inserted afterwards, so the map is as small as the configuration.
#[derive(Clone)]
pub(super) struct MonitoringGates(Arc<BTreeMap<StationId, Arc<Mutex<MonitoringEventGate201>>>>);

impl MonitoringGates {
    pub(super) fn new<'a>(stations: impl Iterator<Item = &'a StationId>) -> Self {
        let gates = stations
            .map(|station| {
                let gate = Arc::new(Mutex::new(MonitoringEventGate201::default()));
                (station.clone(), gate)
            })
            .collect();

        Self(Arc::new(gates))
    }

    /// Hands the station's gate to a new connection for as long as it runs. A previous
    /// connection that is still finishing its last call holds the lock, so the two never judge
    /// calls concurrently; the new connection starts a new series and keeps the budget.
    pub(super) async fn begin(
        &self,
        station: &StationId,
    ) -> Option<OwnedMutexGuard<MonitoringEventGate201>> {
        let mut gate = self.0.get(station)?.clone().lock_owned().await;
        gate.connection_started();

        Some(gate)
    }
}

pub(super) async fn complete(
    incoming: &IncomingCall,
    snapshot: &mut StationSnapshot,
    services: &mut CallContext<'_>,
) -> Result<serde_json::Value, OcppCallError> {
    let protocol = ProtocolEdition::Ocpp201;
    let ChargerObservation::MonitoringEvents201(observation) = &incoming.call.observation else {
        return Err(call_error(protocol, OcppErrorCode::ProtocolError));
    };

    // An unregistered station is refused before it can spend any of its flood budget.
    if uob_application::registration::v201::accepted(snapshot).is_err() {
        return Err(call_error(protocol, OcppErrorCode::ProtocolError));
    }

    let now = Clock.now();
    match services.monitoring.admit(observation.clone(), now) {
        MonitoringEventDecision201::Record { record, receipt } => {
            let context = context(
                services.store,
                services.identity,
                incoming,
                services.target.clone(),
            )
            .await
            .map_err(|_| call_error(protocol, OcppErrorCode::InternalError))?;

            record_monitoring_events_201(services.store, snapshot, record, context, now)
                .await
                .map_err(|error| commit_error(protocol, &error))?;
            services.monitoring.committed(receipt);
        }
        MonitoringEventDecision201::Shed { .. } => {
            record_monitoring_event_shed_201(services.resources);
            incoming.trace.emit_fields(
                FlowStage::Application,
                FlowEvidence::Rejected,
                vec![SafeDiagnosticField::Action(incoming.call.action.clone())],
            );
        }
    }

    Ok(json!([
        3,
        incoming.call.message_id,
        v201::monitoring_event::response()
    ]))
}
