use super::{Auth, Clock, Store, identity, time};
use serde_json::json;
use std::time::Duration;
use uob_application::{
    CommandClock, TransactionApplyOutcome, record_transaction_event,
    transaction16::TransactionContext,
};
use uob_contracts::{EventId, StationSnapshot, TargetInstanceId};
use uob_protocol_adapter::{IncomingCall, v16, v201};
use uob_provider_adapter::{LocalAuthorizationProvider, LocalChargingIdentityProvider};

pub(super) async fn handle_16(
    incoming: IncomingCall,
    store: &Store,
    auth: &Auth,
    snapshot: &mut StationSnapshot,
    sequence: u64,
) {
    let context = TransactionContext {
        identity: identity(),
        event_id: EventId::new(format!("issue87-16-{sequence}")).unwrap(),
        sequence,
        correlation_id: None,
        target: Some((TargetInstanceId::new("main").unwrap(), 1)),
        delivery_deadline: time("2026-09-02T00:00:00Z"),
    };
    let services = v16::TransactionServices {
        store,
        authorization: auth,
        provider: &LocalAuthorizationProvider,
        clock: &Clock,
        authorization_timeout: Duration::from_secs(1),
    };
    let response = v16::complete_transaction(incoming.call, snapshot, &services, context)
        .await
        .unwrap();
    incoming.responder.respond(&response[2]).unwrap();
}

pub(super) async fn handle_201(
    incoming: IncomingCall,
    store: &Store,
    auth: &Auth,
    snapshot: &mut StationSnapshot,
    sequence: u64,
) {
    use uob_application::ChargerObservation;

    if incoming.call.action.as_str() == "Authorize" {
        let reply = v201::complete_authorization(
            incoming.call,
            &snapshot.resources[0].resource,
            auth,
            &LocalChargingIdentityProvider,
            &Clock,
            Duration::from_secs(1),
        )
        .await
        .unwrap();
        incoming.responder.respond(&reply[2]).unwrap();
        return;
    }
    let ChargerObservation::TransactionEvent(observation) = &incoming.call.observation else {
        panic!("unexpected 201 station observation");
    };
    let context = TransactionContext {
        identity: identity(),
        event_id: EventId::new(format!("issue87-201-{sequence}")).unwrap(),
        sequence,
        correlation_id: None,
        target: Some((TargetInstanceId::new("main").unwrap(), 1)),
        delivery_deadline: time("2026-09-02T00:00:00Z"),
    };
    let effect = record_transaction_event(store, snapshot, observation, context, Clock.now())
        .await
        .unwrap();
    assert_eq!(effect, TransactionApplyOutcome::Applied);
    let response = if observation.event == uob_application::TransactionEventKind::Started {
        json!({"idTokenInfo":{"status":"Accepted"}})
    } else {
        json!({})
    };
    incoming.responder.respond(&response).unwrap();
}
