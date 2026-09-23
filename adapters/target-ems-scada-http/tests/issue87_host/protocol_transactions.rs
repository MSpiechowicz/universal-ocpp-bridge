use super::{Auth, Clock, Store, identity, time};
use serde_json::json;
use std::time::Duration;
use uob_application::{
    AtomicStoreWrite, CommandClock, DeliveryId, Durability, OperationalStore, PendingDelivery,
    transaction16::TransactionContext,
};
use uob_contracts::{
    EventEnvelope, EventId, StationSnapshot, TargetInstanceId, TransactionSnapshot,
};
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
    use uob_application::{ChargerObservation, TransactionApplyOutcome, apply_transaction_event};

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
    let effect = apply_transaction_event(snapshot, observation, Clock.now()).unwrap();
    if effect == TransactionApplyOutcome::Applied {
        let mut write = AtomicStoreWrite::empty();
        write.station_snapshot = Some(snapshot.clone());
        let transaction = snapshot.transactions.last().unwrap().clone();
        let kind = match observation.event {
            uob_application::TransactionEventKind::Started => "transaction.started",
            uob_application::TransactionEventKind::Updated => "transaction.updated",
            uob_application::TransactionEventKind::Ended => "transaction.ended",
        };
        let event_id = EventId::new(format!("issue87-201-{sequence}")).unwrap();
        let event: EventEnvelope<TransactionSnapshot> = serde_json::from_value(json!({
            "event_id": event_id,
            "schema_version":{"major":1,"revision":0},
            "runtime": identity().runtime,
            "resource": transaction.resource,
            "observed_at": Clock.now(),
            "event_type": kind,
            "origin":{"kind":"station"},
            "sequence": sequence,
            "payload": transaction
        }))
        .unwrap();
        write.required_deliveries.push(PendingDelivery {
            delivery_id: DeliveryId::new(format!("transaction/{}", event_id.as_str())).unwrap(),
            event_id,
            target_instance_id: TargetInstanceId::new("main").unwrap(),
            target_configuration_revision: 1,
            ordering_key: transaction.resource.clone(),
            deadline: time("2026-09-02T00:00:00Z"),
            durability: Durability::Critical,
            payload: transaction,
        });
        write.journal_events.push(event);
        store.write_atomic(write).await.unwrap();
    }
    let response = if observation.event == uob_application::TransactionEventKind::Started {
        json!({"idTokenInfo":{"status":"Accepted"}})
    } else {
        json!({})
    };
    incoming.responder.respond(&response).unwrap();
}
