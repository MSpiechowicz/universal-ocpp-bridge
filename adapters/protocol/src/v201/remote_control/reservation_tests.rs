use super::*;
use serde_json::json;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use uob_contracts::{
    CanonicalEvseId, CanonicalResource, NativeProtocolReference, Operation, PayloadSchemaId,
    PrivilegedOcppOperation, ProtocolActionName, ProtocolEdition, RESERVE_NOW_REFERENCE_SCHEMA_201,
    ReserveNowReference201, SupportedOperation,
};

fn at(text: &str) -> UtcTimestamp {
    serde_json::from_value(json!(text)).unwrap()
}
struct Clock(parking_lot::Mutex<UtcTimestamp>);
impl CommandClock for Clock {
    fn now(&self) -> UtcTimestamp {
        *self.0.lock()
    }
}
fn snapshot() -> StationSnapshot {
    let mut snapshot: StationSnapshot = serde_json::from_slice(include_bytes!(
        "../../../../../crates/contracts/tests/fixtures/station-snapshot-ocpp201-v1.json"
    ))
    .unwrap();
    let mut accepted: Value = serde_json::from_slice(include_bytes!(
        "../../../../../crates/contracts/tests/fixtures/data-point-value-v1.json"
    ))
    .unwrap();
    accepted["point_id"] = json!("ocpp201/registration/status");
    accepted["value"] = json!({"type":"text","value":"Accepted"});
    snapshot
        .current_values
        .push(serde_json::from_value(accepted).unwrap());
    let mut evse = snapshot.resources[0].clone();
    evse.resource.resource = Some(CanonicalResource::Evse {
        evse_id: CanonicalEvseId::new("evse-1").unwrap(),
        connector_id: None,
    });
    evse.resource.native_protocol_reference = Some(NativeProtocolReference::Ocpp201 {
        evse_id: 1,
        connector_id: None,
    });
    evse.capabilities.operations = vec![SupportedOperation {
        operation: Operation::ProtocolAction {
            protocol: ProtocolEdition::Ocpp201,
            action: "ReserveNow".to_owned(),
        },
        parameters: vec![],
    }];
    snapshot.resources.insert(0, evse);
    snapshot
}
fn deferred() -> (DeferredReservationCall201, Arc<Clock>, Arc<AtomicBool>) {
    let clock = Arc::new(Clock(parking_lot::Mutex::new(at("2026-09-01T15:00:00Z"))));
    let snapshot = snapshot();
    let reference = format!("reserve201:{}", "a".repeat(64));
    let native = json!({"id":-1,"expiryDateTime":"2026-09-02T00:00:00Z","evseId":1,"idToken":{"idToken":"Driver-A1","type":"ISO14443"},"groupIdToken":{"idToken":"Fleet","type":"Central"}});
    let provider = Arc::new(ReservationValues201::from_json_bytes(snapshot.station.clone(), json!({"reservations":[{"reference":reference,"request":native,"expires_at":"2026-09-02T00:00:00Z","revoked":false}],"identities":[{"idToken":{"idToken":"sibling","type":"ISO14443"},"groupIdToken":{"idToken":"FLEET","type":"Central"}}]}).to_string().into_bytes()).unwrap());
    let mut command: Command<Value> = serde_json::from_slice(include_bytes!(
        "../../../../../crates/contracts/tests/fixtures/command-start-v1.json"
    ))
    .unwrap();
    command.resource = snapshot.resources[0].resource.clone();
    command.admitted_at = clock.now();
    command.expires_at = at("2026-09-03T00:00:00Z");
    let request = ReserveNowReference201 {
        id: -1,
        expiry_date_time: at("2026-09-02T00:00:00Z"),
        evse_id: Some(1),
        connector_type: None,
        reservation_reference: reference,
    };
    command.operation = CommandOperation::Ocpp(PrivilegedOcppOperation {
        protocol: ProtocolEdition::Ocpp201,
        action: ProtocolActionName::new("ReserveNow").unwrap(),
        payload_schema: PayloadSchemaId::new(RESERVE_NOW_REFERENCE_SCHEMA_201).unwrap(),
        payload: serde_json::to_value(&request).unwrap(),
    });
    let permission = Arc::new(AtomicBool::new(true));
    let grant_permission = permission.clone();
    let grant: Arc<ReservationGrant201> =
        Arc::new(move |_, _| grant_permission.load(Ordering::SeqCst));
    (
        DeferredReservationCall201 {
            command,
            request: Request::Reserve(request),
            snapshot: Arc::new(RwLock::new(snapshot)),
            provider: Some(provider),
            grant: Some(grant),
            active: Arc::new(Mutex::new(true)),
            non_evse_specific: false,
            clock: clock.clone(),
        },
        clock,
        permission,
    )
}
#[tokio::test]
async fn bounded_wire_count_matches_the_exact_native_frame_without_the_public_reference() {
    let (deferred, _, _) = deferred();
    let id = "message\"\\";
    let counted = deferred.wire_size(id).unwrap();
    let sent = deferred
        .send_with(id, |message| {
            let axum::extract::ws::Message::Text(text) = message else {
                panic!("text wire");
            };
            assert_eq!(text.len(), counted);
            let frame: Value = serde_json::from_str(&text).unwrap();
            assert_eq!(frame[2], "ReserveNow");
            assert_eq!(frame[3]["idToken"]["idToken"], "Driver-A1");
            assert_eq!(frame[3]["groupIdToken"]["type"], "Central");
            assert_eq!(frame[3]["evseId"], 1);
            assert!(frame[3]["reservationReference"].is_null());
            assert!(crate::command_registry::reservation201::valid_native(
                0, &frame[3]
            ));
            std::future::ready(Ok(()))
        })
        .await;
    assert!(sent.unwrap().is_ok());
    let metadata = deferred
        .metadata_size(&json!({}), id, "correlation")
        .unwrap();
    assert!(metadata >= serde_json::to_string(&deferred.command).unwrap().len());
}
fn mutate_snapshot(
    deferred: &DeferredReservationCall201,
    change: impl FnOnce(&mut StationSnapshot),
) {
    let Ok(mut snapshot) = deferred.snapshot.write() else {
        panic!("unexpectedly poisoned production snapshot lock");
    };
    change(&mut snapshot);
}
#[tokio::test]
async fn expiry_owner_revocation_scope_capability_grant_registration_and_generation_recheck_before_first_poll()
 {
    for change in 0..7 {
        let (deferred, clock, permission) = deferred();
        assert!(deferred.wire_size("admitted").is_some());
        let called = Arc::new(AtomicUsize::new(0));
        let observed = called.clone();
        let send = deferred.send_with("admitted", move |_| {
            observed.fetch_add(1, Ordering::SeqCst);
            std::future::ready(Ok(()))
        });
        match change {
            0 => *clock.0.lock() = at("2026-09-02T00:00:00Z"),
            1 => {
                let Request::Reserve(request) = &deferred.request else {
                    unreachable!()
                };
                assert!(
                    deferred
                        .provider
                        .as_ref()
                        .unwrap()
                        .revoke(&request.reservation_reference)
                );
            }
            2 => {
                let Ok(mut active) = deferred.active.lock() else {
                    panic!("unexpectedly poisoned production generation lock");
                };
                *active = false;
            }
            3 => permission.store(false, Ordering::SeqCst),
            4 => mutate_snapshot(&deferred, |s| {
                s.resources[0].capabilities.operations.clear();
            }),
            5 => mutate_snapshot(&deferred, |s| {
                s.current_values
                    .retain(|v| v.point_id.as_str() != "ocpp201/registration/status");
            }),
            _ => mutate_snapshot(&deferred, |s| {
                s.resources[0].resource.native_protocol_reference =
                    Some(NativeProtocolReference::Ocpp201 {
                        evse_id: 2,
                        connector_id: None,
                    });
            }),
        }
        assert!(send.await.is_none());
        assert_eq!(called.load(Ordering::SeqCst), 0);
    }
}
#[test]
fn unspecified_evse_reservations_need_explicit_non_evse_specific_support() {
    let (mut deferred, _, _) = deferred();
    let snapshot = deferred.snapshot.read().unwrap().clone();
    let station = snapshot.station.clone();
    let mut command = deferred.command.clone();
    command.resource = station;
    let request = ReserveNowReference201 {
        id: 1,
        expiry_date_time: at("2026-09-02T00:00:00Z"),
        evse_id: None,
        connector_type: None,
        reservation_reference: format!("reserve201:{}", "b".repeat(64)),
    };
    let mut with_station_capability = snapshot;
    with_station_capability
        .capabilities
        .operations
        .push(SupportedOperation {
            operation: Operation::ProtocolAction {
                protocol: ProtocolEdition::Ocpp201,
                action: "ReserveNow".to_owned(),
            },
            parameters: vec![],
        });
    let now = at("2026-09-01T15:00:00Z");
    let grant = deferred.grant.take();
    let reserve = Request::Reserve(request);
    assert_eq!(
        check(
            &command,
            &with_station_capability,
            &reserve,
            false,
            grant.as_ref(),
            now
        ),
        Err(CommandErrorCode::UnsupportedOperation)
    );
    assert_eq!(
        check(
            &command,
            &with_station_capability,
            &reserve,
            true,
            grant.as_ref(),
            now
        ),
        Ok(())
    );
    assert_eq!(
        check(
            &command,
            &with_station_capability,
            &reserve,
            true,
            None,
            now
        ),
        Err(CommandErrorCode::Unauthorized)
    );
}
#[test]
fn native_acknowledgements_are_exact_and_drop_status_info() {
    let (deferred, _, _) = deferred();
    let now = at("2026-09-01T15:00:01Z");
    for status in ["Accepted", "Faulted", "Occupied", "Rejected", "Unavailable"] {
        let payload = json!({"status":status,"statusInfo":{"reasonCode":"PRIVATE","additionalInfo":"vendor text"}});
        let CommandDispatchOutcome::ReservationResponse201(evidence) =
            response("ReserveNow", &payload, &deferred.command, now)
        else {
            panic!("valid native {status}");
        };
        let encoded = serde_json::to_string(&evidence).unwrap();
        assert!(!encoded.contains("PRIVATE") && !encoded.contains("vendor"));
        assert!(matches!(
            evidence,
            ReservationResult201::ReserveNow {
                reservation_id: -1,
                evse_id: Some(1),
                status: Some(_),
                ..
            }
        ));
    }
    for invalid in [
        json!({}),
        json!({"status":"Expired"}),
        json!({"status":"Accepted","extra":1}),
        json!({"status":"Accepted","statusInfo":{"reasonCode":"x".repeat(21)}}),
    ] {
        assert!(matches!(
            response("ReserveNow", &invalid, &deferred.command, now),
            CommandDispatchOutcome::TransmissionUncertain { .. }
        ));
    }
    assert_eq!(
        native_status("CancelReservation", &json!({"status":"Occupied"})),
        None,
        "cancel acknowledgements have only Accepted and Rejected"
    );
}
