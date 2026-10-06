use super::*;
use serde_json::json;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use uob_application::AuthorizationProvider;
use uob_contracts::{
    Operation, PayloadSchemaId, PrivilegedOcppOperation, ProtocolActionName, ProtocolEdition,
    RESERVE_NOW_REFERENCE_SCHEMA_16, SupportedOperation,
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
fn deferred() -> (DeferredReservationCall16, Arc<Clock>, Arc<AtomicBool>) {
    let clock = Arc::new(Clock(parking_lot::Mutex::new(at("2026-09-01T15:00:00Z"))));
    let mut snapshot: StationSnapshot = serde_json::from_slice(include_bytes!(
        "../../../../../crates/contracts/tests/fixtures/station-snapshot-ocpp16-v1.json"
    ))
    .unwrap();
    let mut accepted: Value = serde_json::from_slice(include_bytes!(
        "../../../../../crates/contracts/tests/fixtures/data-point-value-v1.json"
    ))
    .unwrap();
    accepted["point_id"] = json!("ocpp16/registration/status");
    accepted["value"] = json!({"type":"text","value":"Accepted"});
    snapshot
        .current_values
        .push(serde_json::from_value(accepted).unwrap());
    snapshot.resources[0]
        .capabilities
        .operations
        .push(SupportedOperation {
            operation: Operation::ProtocolAction {
                protocol: ProtocolEdition::Ocpp16j,
                action: "ReserveNow".to_owned(),
            },
            parameters: vec![],
        });
    let reference = format!("reserve16:{}", "a".repeat(64));
    let native = json!({"connectorId":1,"expiryDate":"2026-09-02T00:00:00Z","reservationId":-1,"idTag":"Straße","parentIdTag":"Real-Parent"});
    let provider = Arc::new(ReservationValues16::from_json_bytes(snapshot.station.clone(), json!({"reservations":[{"reference":reference,"request":native,"expires_at":"2026-09-02T00:00:00Z","revoked":false}],"identities":[{"idTag":"Sibling","parentIdTag":"real-parent","authorize":false}]}).to_string().into_bytes()).unwrap());
    let mut command: Command<Value> = serde_json::from_slice(include_bytes!(
        "../../../../../crates/contracts/tests/fixtures/command-start-v1.json"
    ))
    .unwrap();
    command.resource = snapshot.resources[0].resource.clone();
    command.admitted_at = clock.now();
    command.expires_at = at("2026-09-03T00:00:00Z");
    let request = uob_contracts::ReserveNowReference16 {
        connector_id: 1,
        expiry_date: at("2026-09-02T00:00:00Z"),
        reservation_id: -1,
        reservation_reference: reference,
    };
    command.operation = CommandOperation::Ocpp(PrivilegedOcppOperation {
        protocol: ProtocolEdition::Ocpp16j,
        action: ProtocolActionName::new("ReserveNow").unwrap(),
        payload_schema: PayloadSchemaId::new(RESERVE_NOW_REFERENCE_SCHEMA_16).unwrap(),
        payload: serde_json::to_value(&request).unwrap(),
    });
    let permission = Arc::new(AtomicBool::new(true));
    let grant_permission = permission.clone();
    let grant: Arc<ReservationGrant16> =
        Arc::new(move |_, _| grant_permission.load(Ordering::SeqCst));
    (
        DeferredReservationCall16 {
            command,
            request: Request::Reserve(request),
            snapshot: Arc::new(RwLock::new(snapshot)),
            provider: Some(provider),
            grant: Some(grant),
            active: Arc::new(std::sync::Mutex::new(true)),
            zero: false,
            clock: clock.clone(),
        },
        clock,
        permission,
    )
}
#[tokio::test]
async fn bounded_wire_count_matches_actual_unicode_and_escaped_frame_without_identity_metadata() {
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
            assert_eq!(frame[3]["idTag"], "Straße");
            assert_eq!(frame[3]["parentIdTag"], "Real-Parent");
            assert!(frame[3]["reservationReference"].is_null());
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
    deferred: &DeferredReservationCall16,
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
                    .retain(|v| v.point_id.as_str() != "ocpp16/registration/status");
            }),
            _ => mutate_snapshot(&deferred, |s| {
                s.resources[0].resource.native_protocol_reference =
                    Some(uob_contracts::NativeProtocolReference::Ocpp16 { connector_id: 2 });
            }),
        }
        assert!(send.await.is_none());
        assert_eq!(called.load(Ordering::SeqCst), 0);
    }
}
#[tokio::test]
async fn native_group_keys_are_casefolded_but_existing_policy_keys_are_not() {
    let (deferred, _, _) = deferred();
    let provider = deferred.provider.as_ref().unwrap();
    assert_eq!(
        super::super::reservation_key_16("Straße".as_bytes()),
        super::super::reservation_key_16(b"STRASSE")
    );
    assert_eq!(
        provider.group_key(b"SIBLING"),
        super::super::reservation_key_16(b"REAL-PARENT")
    );
    assert!(provider.group_key(b"REAL-PARENT").is_none());
    assert_eq!(provider.policy_entries().count(), 0);
    let policy = uob_provider_adapter::LocalAuthorizationProvider;
    let mixed = uob_application::SensitiveAuthorizationToken::new("Straße").unwrap();
    let folded = uob_application::SensitiveAuthorizationToken::new("STRASSE").unwrap();
    assert_ne!(
        policy.resolve(&mixed).await.unwrap(),
        policy.resolve(&folded).await.unwrap()
    );
}
