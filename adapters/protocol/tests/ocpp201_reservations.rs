use serde_json::{Value, json};
use uob_application::{ChargerObservation, ReservationUpdateStatus201};
use uob_contracts::{
    CommandErrorCode, PayloadSchemaId, PrivilegedOcppOperation, ProtocolActionName,
    ProtocolEdition, ResourceRef, StationSnapshot,
};
use uob_protocol_adapter::{
    command_registry::{command_schemas, validate_privileged_operation},
    v201,
    v201::remote_control::reservation_key_201,
};

fn corpus(name: &str) -> Vec<u8> {
    std::fs::read(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/ocpp-fixtures/corpus/wire/2.0.1")
            .join(format!("{name}.json")),
    )
    .unwrap()
}

#[test]
fn independent_native_updates_decode_to_exact_terminal_facts() {
    for (name, status) in [
        (
            "reservation-status-expired",
            ReservationUpdateStatus201::Expired,
        ),
        (
            "reservation-status-removed",
            ReservationUpdateStatus201::Removed,
        ),
    ] {
        let call = v201::decode_call(&corpus(name)).unwrap();
        assert_eq!(call.action.as_str(), "ReservationStatusUpdate");
        assert_eq!(
            call.observation,
            ChargerObservation::ReservationStatusUpdate201 {
                reservation_id: -114,
                status
            }
        );
    }
    for payload in [
        json!({"reservationId":1,"reservationUpdateStatus":"Removed","customData":{"vendorId":"v"}}),
        json!({"reservationId":1,"reservationUpdateStatus":"Faulted"}),
        json!({"reservationId":2_147_483_648_u64,"reservationUpdateStatus":"Expired"}),
        json!({"reservationUpdateStatus":"Expired"}),
    ] {
        let frame = json!([2, "invalid", "ReservationStatusUpdate", payload]).to_string();
        assert!(v201::decode_call(frame.as_bytes()).is_err(), "{payload}");
    }
}

#[test]
fn transaction_reservation_id_keeps_only_a_one_way_type_scoped_identity_key() {
    let call = v201::decode_call(&corpus("reservation-transaction")).unwrap();
    let ChargerObservation::TransactionEvent(observation) = call.observation else {
        panic!("transaction event");
    };
    assert_eq!(observation.reservation_id, Some(-114));
    assert_eq!(
        observation.reservation_token_key,
        reservation_key_201("ISO14443", "TOKEN-MARKER-114")
    );
    assert_ne!(
        observation.reservation_token_key,
        reservation_key_201("Local", "token-marker-114")
    );
    assert!(!format!("{:?}", observation.reservation_token_key).contains("token-marker"));
    let call = v201::decode_call(&corpus("reservation-transaction-tokenless")).unwrap();
    let ChargerObservation::TransactionEvent(observation) = call.observation else {
        panic!("transaction event");
    };
    assert_eq!(observation.reservation_id, Some(-114));
    assert!(observation.reservation_token_key.is_none());
}

fn snapshot() -> StationSnapshot {
    serde_json::from_slice(include_bytes!(
        "../../../crates/contracts/tests/fixtures/station-snapshot-ocpp201-v1.json"
    ))
    .unwrap()
}
fn operation(
    protocol: ProtocolEdition,
    action: &str,
    schema: &str,
    payload: Value,
) -> PrivilegedOcppOperation<Value> {
    PrivilegedOcppOperation {
        protocol,
        action: ProtocolActionName::new(action).unwrap(),
        payload_schema: PayloadSchemaId::new(schema).unwrap(),
        payload,
    }
}

#[test]
fn privileged_validation_routes_each_edition_to_its_own_reservation_contract() {
    let station: ResourceRef = snapshot().station;
    let reference = format!("reserve201:{}", "a".repeat(64));
    let unspecified =
        json!({"id":-1,"expiryDateTime":"2099-01-01T00:00:00Z","reservationReference":reference});
    assert_eq!(
        validate_privileged_operation(
            &station,
            &operation(
                ProtocolEdition::Ocpp201,
                "ReserveNow",
                "urn:uob:ocpp201:ReserveNowReference:1",
                unspecified.clone()
            )
        ),
        Ok(())
    );
    let mut evse_payload = unspecified.clone();
    evse_payload["evseId"] = json!(1);
    assert_eq!(
        validate_privileged_operation(
            &station,
            &operation(
                ProtocolEdition::Ocpp201,
                "ReserveNow",
                "urn:uob:ocpp201:ReserveNowReference:1",
                evse_payload
            )
        ),
        Err(CommandErrorCode::InvalidParameters),
        "an EVSE reservation is never admitted at station scope"
    );
    assert_eq!(
        validate_privileged_operation(
            &station,
            &operation(
                ProtocolEdition::Ocpp201,
                "ReserveNow",
                "urn:uob:ocpp16:ReserveNowReference:1",
                unspecified
            )
        ),
        Err(CommandErrorCode::InvalidParameters)
    );
    assert_eq!(
        validate_privileged_operation(
            &station,
            &operation(
                ProtocolEdition::Ocpp201,
                "CancelReservation",
                "urn:OCPP:Cp:2:2020:3:CancelReservationRequest",
                json!({"reservationId":i32::MIN})
            )
        ),
        Ok(())
    );
    assert_eq!(
        validate_privileged_operation(
            &station,
            &operation(
                ProtocolEdition::Ocpp201,
                "CancelReservation",
                "urn:OCPP:Cp:2:2020:3:CancelReservationRequest",
                json!({"reservationId":1,"customData":{"vendorId":"v"}})
            )
        ),
        Err(CommandErrorCode::InvalidParameters)
    );
    assert!(
        command_schemas(&snapshot())
            .iter()
            .all(|descriptor| descriptor.action != "ReserveNow"),
        "nothing is discoverable without explicit station capabilities"
    );
}
