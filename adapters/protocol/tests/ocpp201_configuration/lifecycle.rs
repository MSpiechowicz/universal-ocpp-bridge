use crate::configuration_support::reference;
use crate::{configuration_support::*, support::*};
use futures::poll;
use serde_json::json;
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use uob_application::*;
use uob_contracts::*;
use uob_protocol_adapter::{OutboundCall, SessionCallOutcome};
struct AdvancingClock(AtomicBool);
impl CommandClock for AdvancingClock {
    fn now(&self) -> UtcTimestamp {
        if self.0.load(Ordering::SeqCst) {
            time("2026-09-01T04:00:00Z")
        } else {
            Clock.now()
        }
    }
}
async fn sentinel(running: &mut RunningSession, id: &str) {
    let pending = running
        .handle
        .try_call(OutboundCall {
            message_id: id.into(),
            action: ProtocolActionName::new("Heartbeat").unwrap(),
            payload: json!({}),
            correlation_id: CorrelationId::new(id).unwrap(),
        })
        .unwrap();
    assert_eq!(
        receive_json(&mut running.peer).await,
        json!([2, id, "Heartbeat", {}])
    );
    running
        .peer
        .send_text(json!([3,id,{"currentTime":"2026-09-01T04:00:00Z"}]).to_string())
        .await
        .unwrap();
    assert!(matches!(
        pending.receive().await,
        SessionCallOutcome::Result { .. }
    ));
}

#[tokio::test(flavor = "current_thread")]
async fn queued_revocation_expiry_and_generation_detach_send_no_secret_call() {
    let mut running = session("ocpp2.0.1", Duration::from_secs(2)).await;
    let database = Database::new();
    let store = database.open();
    let clock = Arc::new(AdvancingClock(AtomicBool::new(false)));
    let a = entry("Revoked", 1);
    let b = entry("Expired", 2);
    let (snapshot, provider, port, _) = Box::pin(configured(
        &store,
        &running,
        vec![
            (a.clone(), "REVOKED_SECRET".into()),
            (b.clone(), "EXPIRED_SECRET".into()),
        ],
        None,
        clock.clone(),
    ))
    .await;
    let mut revoked =
        Box::pin(port.dispatch(variables(&snapshot, "queued-revoked", &[a]).admit(clock.now())));
    let mut expired = Box::pin(port.dispatch(
        variables(&snapshot, "queued-expired", std::slice::from_ref(&b)).admit(clock.now()),
    ));
    assert!(poll!(revoked.as_mut()).is_pending());
    assert!(poll!(expired.as_mut()).is_pending());
    provider.revoke(&reference(1)).unwrap();
    clock.0.store(true, Ordering::SeqCst);
    for outcome in [revoked.await.unwrap(), expired.await.unwrap()] {
        assert!(matches!(
            outcome,
            CommandDispatchOutcome::NotTransmitted { .. }
        ));
    }
    sentinel(&mut running, "after-revoke-expire").await;
    clock.0.store(false, Ordering::SeqCst);
    let mut detached =
        Box::pin(port.dispatch(variables(&snapshot, "queued-generation", &[b]).admit(clock.now())));
    assert!(poll!(detached.as_mut()).is_pending());
    port.detach_device_model();
    port.update_committed(snapshot.clone()).unwrap();
    assert!(matches!(
        detached.await.unwrap(),
        CommandDispatchOutcome::NotTransmitted { .. }
    ));
    assert!(matches!(
        port.dispatch(
            variables(&snapshot, "stale-republish", &[entry("Expired", 2)]).admit(clock.now())
        )
        .await
        .unwrap(),
        CommandDispatchOutcome::NotTransmitted { .. }
    ));
    sentinel(&mut running, "after-generation").await;
    stop(running, store).await;
}

#[tokio::test]
async fn actual_escaped_native_frame_limits_are_inclusive_and_unknown_limits_are_single_item() {
    let mut running = session("ocpp2.0.1", Duration::from_secs(2)).await;
    let database = Database::new();
    let store = database.open();
    let a = entry("First", 1);
    let b = entry("Second", 2);
    let (snapshot, _, _, commands) = Box::pin(configured(
        &store,
        &running,
        vec![
            (a.clone(), "\"\\\n😀".repeat(100)),
            (b.clone(), String::new()),
        ],
        None,
        Arc::new(Clock),
    ))
    .await;
    let request = variables(&snapshot, "unknown-multiple", &[a.clone(), b.clone()]);
    assert!(matches!(
        commands.submit(request).await.unwrap().lifecycle,
        CommandLifecycle::Rejected { .. }
    ));
    sentinel(&mut running, "unknown-no-call").await;
    let native = json!([2,"exact-native","SetVariables",{"setVariableData":[
        {"component":{"name":"VendorCtrlr"},"variable":{"name":"First"},"attributeValue":"\"\\\n😀".repeat(100)},
        {"component":{"name":"VendorCtrlr"},"variable":{"name":"Second"},"attributeValue":""}
    ]}]);
    let bytes = native.to_string().len();
    learn(&mut running, &snapshot, &commands, 2, bytes, "exact").await;
    let request = variables(&snapshot, "exact-native", &[a.clone(), b.clone()]);
    let submit = {
        let commands = commands.clone();
        tokio::spawn(async move { commands.submit(request).await.unwrap() })
    };
    assert_eq!(receive_json(&mut running.peer).await, native);
    running.peer.send_text(json!([3,"exact-native",{"setVariableResult":[
        {"component":{"name":"VendorCtrlr"},"variable":{"name":"First"},"attributeStatus":"Accepted"},
        {"component":{"name":"VendorCtrlr"},"variable":{"name":"Second"},"attributeStatus":"RebootRequired"}
    ]}]).to_string()).await.unwrap();
    assert!(matches!(
        submit.await.unwrap().lifecycle,
        CommandLifecycle::ProtocolResponse { accepted: true, .. }
    ));
    learn(&mut running, &snapshot, &commands, 2, bytes - 1, "tight").await;
    // Same identifier length makes the independent byte oracle exact for the second command.
    let request = variables(&snapshot, "tight-native", &[a.clone(), b.clone()]);
    assert!(matches!(
        commands.submit(request).await.unwrap().lifecycle,
        CommandLifecycle::Rejected { .. }
    ));
    sentinel(&mut running, "tight-no-call").await;
    learn(&mut running, &snapshot, &commands, 1, 16384, "items").await;
    assert!(matches!(
        commands
            .submit(variables(&snapshot, "item-bound", &[a, b]))
            .await
            .unwrap()
            .lifecycle,
        CommandLifecycle::Rejected { .. }
    ));
    sentinel(&mut running, "items-no-call").await;
    stop(running, store).await;
}

#[tokio::test]
async fn admission_preflight_binds_exact_resource_unicode_identity_attribute_and_expiry() {
    let mut running = session("ocpp2.0.1", Duration::from_secs(2)).await;
    let database = Database::new();
    let store = database.open();
    let mut original = entry("Straße", 1);
    original.component.name = "Straße".into();
    let (snapshot, provider, _, _) = Box::pin(configured(
        &store,
        &running,
        vec![(original.clone(), "BOUND_SECRET".into())],
        None,
        Arc::new(Clock),
    ))
    .await;
    let mut normalized = original.clone();
    normalized.component.name = "STRASSE".into();
    normalized.variable.name = "STRASSE".into();
    normalized.attribute_type = Some(DeviceAttributeType201::Actual);
    let request = variables(&snapshot, "preflight", &[normalized]);
    let CommandOperation::Ocpp(operation) = request.request.operation else {
        panic!("operation");
    };
    assert!(
        provider
            .validate_operation(&snapshot.station, &operation, Clock.now())
            .is_ok()
    );
    let mut foreign = snapshot.station.clone();
    foreign.station_id = StationId::new("foreign").unwrap();
    assert!(
        provider
            .validate_operation(&foreign, &operation, Clock.now())
            .is_err()
    );
    let mut wrong_attribute = operation.clone();
    wrong_attribute.payload["setVariableData"][0]["attributeType"] = json!("Target");
    assert!(
        provider
            .validate_operation(&snapshot.station, &wrong_attribute, Clock.now())
            .is_err()
    );
    assert!(
        provider
            .validate_operation(&snapshot.station, &operation, time("2026-09-01T03:00:00Z"))
            .is_err()
    );
    provider.revoke(&original.value_reference).unwrap();
    assert!(
        provider
            .validate_operation(&snapshot.station, &operation, Clock.now())
            .is_err()
    );
    sentinel(&mut running, "preflight-no-call").await;
    stop(running, store).await;
}
