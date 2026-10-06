use super::*;
use serde_json::json;
use uob_contracts::{BridgeId, StationId};

fn station() -> ResourceRef {
    ResourceRef {
        bridge_id: BridgeId::new("bridge").unwrap(),
        station_id: StationId::new("station").unwrap(),
        resource: None,
        native_protocol_reference: None,
    }
}
fn reference(byte: char) -> String {
    format!("reserve201:{}", byte.to_string().repeat(64))
}
fn native() -> Value {
    json!({"id":i32::MIN,"expiryDateTime":"2099-01-01T00:00:00Z","evseId":2,"connectorType":"cType2","idToken":{"idToken":"Driver","type":"ISO14443"},"groupIdToken":{"idToken":"Fleet","type":"Central"}})
}
fn file(request: &Value, identities: &Value) -> Vec<u8> {
    json!({"reservations":[{"reference":reference('a'),"request":request,"expires_at":"2099-01-01T00:00:00Z","revoked":false}],"identities":identities}).to_string().into_bytes()
}
fn wrapper(request: &Value) -> ReserveNowReference201 {
    ReserveNowReference201 {
        id: i32::try_from(request["id"].as_i64().unwrap()).unwrap(),
        expiry_date_time: serde_json::from_value(request["expiryDateTime"].clone()).unwrap(),
        evse_id: request["evseId"]
            .as_u64()
            .map(|id| u32::try_from(id).unwrap()),
        connector_type: request
            .get("connectorType")
            .map(|kind| serde_json::from_value(kind.clone()).unwrap()),
        reservation_reference: reference('a'),
    }
}
fn now() -> UtcTimestamp {
    serde_json::from_value(json!("2026-09-01T00:00:00Z")).unwrap()
}

#[test]
fn exact_native_request_yields_only_one_way_keys_and_the_public_wrapper() {
    let values = ReservationValues201::from_json_bytes(
        station(),
        file(&native(), &json!([{"idToken":{"idToken":"SIBLING","type":"ISO14443"},"groupIdToken":{"idToken":"FLEET","type":"Central"},"authorize":true}])),
    )
    .unwrap();
    let request = wrapper(&native());
    let candidate = values.candidate(&station(), &request, now()).unwrap();
    assert_eq!(candidate.evse_id, Some(2));
    assert_eq!(
        candidate.connector_type,
        Some(ReservationConnectorType201::CType2)
    );
    assert_eq!(
        candidate.token_key,
        reservation_key_201("ISO14443", "driver").unwrap()
    );
    assert_ne!(
        candidate.token_key,
        reservation_key_201("Central", "Driver").unwrap(),
        "the token type is part of the identity"
    );
    assert_eq!(candidate.group_key, reservation_key_201("Central", "fleet"));
    assert_eq!(
        values.group_key(&reservation_key_201("ISO14443", "Sibling").unwrap()),
        reservation_key_201("Central", "Fleet"),
        "owner identities provide group membership for attribution"
    );
    let entries = values.policy_entries().collect::<Vec<_>>();
    assert_eq!(entries.len(), 1);
    assert!(entries[0].1);
    let mut changed = request.clone();
    changed.evse_id = Some(3);
    assert!(values.candidate(&station(), &changed, now()).is_none());
    assert!(values.revoke(&reference('a')));
    assert!(values.candidate(&station(), &request, now()).is_none());
}

#[test]
fn non_native_vendor_duplicate_and_unbounded_material_is_rejected() {
    let mut cases = Vec::new();
    for (field, value) in [
        ("customData", json!({"vendorId":"v"})),
        ("evseId", json!(0)),
        ("evseId", json!(-3)),
        ("connectorType", json!("cType9")),
        ("extra", json!(1)),
    ] {
        let mut request = native();
        request[field] = value;
        cases.push(file(&request, &json!([])));
    }
    for token in [
        json!({"idToken":"","type":"ISO14443"}),
        json!({"idToken":"x".repeat(37),"type":"ISO14443"}),
        json!({"idToken":"Driver","type":"NoAuthorization"}),
        json!({"idToken":"Driver","type":"ISO14443","customData":{"vendorId":"v"}}),
        json!({"idToken":"Drïver","type":"ISO14443"}),
    ] {
        let mut request = native();
        request["idToken"] = token;
        cases.push(file(&request, &json!([])));
    }
    let mut missing = native();
    missing.as_object_mut().unwrap().remove("idToken");
    cases.push(file(&missing, &json!([])));
    cases.push(file(
        &native(),
        &json!([{"idToken":{"idToken":"A","type":"KeyCode"}},{"idToken":{"idToken":"a","type":"KeyCode"}}]),
    ));
    cases.push(file(
        &native(),
        &json!([{"idToken":{"idToken":"A","type":"eMAID"},"authorize":true}]),
    ));
    cases.push(
        json!({"reservations":[{"reference":reference('a'),"request":native(),"expires_at":"2099-01-01T00:00:00Z","revoked":false},{"reference":reference('a'),"request":native(),"expires_at":"2099-01-01T00:00:00Z","revoked":false}],"identities":[]})
            .to_string()
            .into_bytes(),
    );
    cases.push(
        json!({"reservations":[{"reference":format!("reserve16:{}", "a".repeat(64)),"request":native(),"expires_at":"2099-01-01T00:00:00Z","revoked":false}],"identities":[]})
            .to_string()
            .into_bytes(),
    );
    cases.push(vec![b' '; 65_537]);
    for bytes in cases {
        assert!(ReservationValues201::from_json_bytes(station(), bytes).is_err());
    }
    let mut evse = station();
    evse.native_protocol_reference = Some(uob_contracts::NativeProtocolReference::Ocpp201 {
        evse_id: 1,
        connector_id: None,
    });
    assert!(
        ReservationValues201::from_json_bytes(evse, file(&native(), &json!([]))).is_err(),
        "the provider is owned by the station, never by one EVSE"
    );
}

#[test]
fn an_owner_record_never_outlives_its_own_or_the_request_expiry() {
    let mut request = native();
    request["expiryDateTime"] = json!("2026-09-01T00:00:10Z");
    let values =
        ReservationValues201::from_json_bytes(station(), file(&request, &json!([]))).unwrap();
    let wrapper = wrapper(&request);
    let before: UtcTimestamp = serde_json::from_value(json!("2026-09-01T00:00:09Z")).unwrap();
    let at: UtcTimestamp = serde_json::from_value(json!("2026-09-01T00:00:10Z")).unwrap();
    assert!(values.candidate(&station(), &wrapper, before).is_some());
    assert!(values.candidate(&station(), &wrapper, at).is_none());
    let mut other = station();
    other.station_id = StationId::new("other").unwrap();
    assert!(values.candidate(&other, &wrapper, before).is_none());
}
