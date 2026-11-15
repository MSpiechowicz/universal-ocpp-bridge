//! Charger-initiated OCPP 1.6 `SecurityEventNotification` (Security Whitepaper section 5.15).
//! Every call of a registered station is answered with the empty response. A redelivered event is
//! acknowledged without a write and without spending budget; a new one is either admitted by the
//! station's gate as one bounded journal record, committed before the answer, or shed. A shed call
//! writes nothing and raises the shared resource pressure signal, and the next durable record
//! reports the loss. Journal events cannot be shed by retention, so the flood budget belongs to
//! the station for the life of the process, not to one connection. The observation holds only
//! redacted text; traces carry identities and counts.
use super::{CallContext, Clock, call_error, calls::commit_error, context};
use serde_json::json;
use std::{collections::BTreeMap, sync::Arc};
use tokio::sync::{Mutex, OwnedMutexGuard};
use uob_application::{
    ChargerObservation, CommandClock, FlowEvidence, FlowStage, SafeDiagnosticField,
    SecurityEventDecision16, SecurityEventGate16, SecurityEventStore16, record_security_event_16,
    record_security_event_shed_16,
};
use uob_contracts::{ProtocolEdition, StationId, StationSnapshot};
use uob_protocol_adapter::{IncomingCall, OcppCallError, OcppErrorCode, v16};

/// One gate per configured station, created at startup. Unknown stations never reach the call
/// loop, and nothing is inserted afterwards, so the map is as small as the configuration.
#[derive(Clone)]
pub(super) struct SecurityGates16(Arc<BTreeMap<StationId, Arc<Mutex<SecurityEventGate16>>>>);

impl SecurityGates16 {
    pub(super) fn new<'a>(stations: impl Iterator<Item = &'a StationId>) -> Self {
        let gates = stations
            .map(|station| {
                let gate = Arc::new(Mutex::new(SecurityEventGate16::default()));
                (station.clone(), gate)
            })
            .collect();

        Self(Arc::new(gates))
    }

    /// Hands the station's gate to a new connection for as long as it runs. A previous
    /// connection that is still finishing its last call holds the lock, so the two never judge
    /// calls concurrently; the new connection keeps the budget. Take it after the monitoring
    /// gate, never before.
    pub(super) async fn begin(
        &self,
        station: &StationId,
    ) -> Option<OwnedMutexGuard<SecurityEventGate16>> {
        Some(self.0.get(station)?.clone().lock_owned().await)
    }
}

pub(super) async fn complete(
    incoming: &IncomingCall,
    snapshot: &mut StationSnapshot,
    services: &mut CallContext<'_>,
) -> Result<serde_json::Value, OcppCallError> {
    let protocol = ProtocolEdition::Ocpp16j;
    let ChargerObservation::SecurityEvent16(observation) = &incoming.call.observation else {
        return Err(call_error(protocol, OcppErrorCode::ProtocolError));
    };

    // An unregistered station is refused before it can spend any of its flood budget.
    if uob_application::registration::accepted(snapshot).is_err() {
        return Err(call_error(protocol, OcppErrorCode::ProtocolError));
    }

    // A redelivery is acknowledged as received, whatever the gate currently allows.
    let recorded = services
        .store
        .security_event_recorded_16(observation.key(snapshot.station.clone()))
        .await
        .map_err(|_| call_error(protocol, OcppErrorCode::InternalError))?;
    if recorded {
        return Ok(acknowledgement(incoming));
    }

    let now = Clock.now();
    match services.security.admit(now) {
        SecurityEventDecision16::Store {
            shed_calls,
            receipt,
        } => {
            let context = context(
                services.store,
                services.identity,
                incoming,
                services.target.clone(),
            )
            .await
            .map_err(|_| call_error(protocol, OcppErrorCode::InternalError))?;

            record_security_event_16(
                services.store,
                snapshot,
                observation.clone(),
                shed_calls,
                context,
                now,
            )
            .await
            .map_err(|error| commit_error(protocol, &error))?;
            services.security.committed(receipt);
        }
        SecurityEventDecision16::Shed { .. } => {
            record_security_event_shed_16(services.resources);
            incoming.trace.emit_fields(
                FlowStage::Application,
                FlowEvidence::Rejected,
                vec![SafeDiagnosticField::Action(incoming.call.action.clone())],
            );
        }
    }

    Ok(acknowledgement(incoming))
}

fn acknowledgement(incoming: &IncomingCall) -> serde_json::Value {
    json!([3, incoming.call.message_id, v16::security_event::response()])
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use uob_contracts::UtcTimestamp;

    fn at(seconds: i64) -> UtcTimestamp {
        UtcTimestamp::new(time::OffsetDateTime::UNIX_EPOCH + time::Duration::seconds(seconds))
    }

    fn station(name: &str) -> StationId {
        StationId::new(name.to_owned()).unwrap()
    }

    #[tokio::test]
    async fn the_budget_and_the_shed_count_belong_to_the_station_across_connections() {
        let id = station("station-a");
        let gates = SecurityGates16::new([&id].into_iter());

        let mut first = gates.begin(&id).await.unwrap();
        for _ in 0..60 {
            let SecurityEventDecision16::Store {
                shed_calls,
                receipt,
            } = first.admit(at(10))
            else {
                panic!("within budget");
            };
            assert_eq!(shed_calls, 0);
            first.committed(receipt);
        }
        assert_eq!(
            first.admit(at(11)),
            SecurityEventDecision16::Shed { calls: 1 }
        );
        assert_eq!(
            first.admit(at(12)),
            SecurityEventDecision16::Shed { calls: 2 }
        );
        drop(first);

        // A reconnect inside the window neither refreshes the budget nor forgets the loss.
        let mut second = gates.begin(&id).await.unwrap();
        assert_eq!(
            second.admit(at(30)),
            SecurityEventDecision16::Shed { calls: 3 }
        );

        // The next window stores again and reports the loss; a failed write keeps it for the
        // next record, a durable one clears it.
        let SecurityEventDecision16::Store {
            shed_calls,
            receipt: _failed_write,
        } = second.admit(at(71))
        else {
            panic!("a new window has budget");
        };
        assert_eq!(shed_calls, 3);
        let SecurityEventDecision16::Store {
            shed_calls,
            receipt,
        } = second.admit(at(72))
        else {
            panic!("budget remains");
        };
        assert_eq!(shed_calls, 3);
        second.committed(receipt);
        let SecurityEventDecision16::Store { shed_calls, .. } = second.admit(at(73)) else {
            panic!("budget remains");
        };
        assert_eq!(shed_calls, 0);
    }

    #[tokio::test]
    async fn one_connection_judges_calls_at_a_time_and_unknown_stations_have_no_gate() {
        let id = station("station-a");
        let gates = SecurityGates16::new([&id].into_iter());
        assert!(gates.begin(&station("station-z")).await.is_none());

        let held = gates.begin(&id).await.unwrap();
        let waiting = tokio::time::timeout(Duration::from_millis(100), gates.begin(&id)).await;
        assert!(
            waiting.is_err(),
            "the previous connection still holds the gate"
        );
        drop(held);
        assert!(gates.begin(&id).await.is_some());
    }
}
