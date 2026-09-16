use super::support::*;
use serde_json::{Value, json};
use std::future::Future;
use std::{sync::Arc, time::Duration};
use uob_application::data_transfer201::{
    Capability, Error, MAX_CAPABILITIES, MAX_DATA_BYTES, Observation, OpaqueData, Provider,
    Registry, Reply, Status,
};
use uob_contracts::{Connectivity, ProtocolEdition, TypedValue};
use uob_protocol_adapter::{
    OcppErrorCode,
    v201::{
        data_transfer::{data_transfer_call, observation},
        decode_call,
    },
};

#[tokio::test]
async fn native_json_statuses_commit_without_retaining_opaque_content() {
    let db = Database::new();
    let store = db.open();
    let mut state = registered(&store, None).await;
    let registry = registry(Arc::new(Probe));
    let accepted: Value = serde_json::from_slice(include_bytes!(
        "../../../../tests/ocpp-fixtures/corpus/wire/2.0.1/data-transfer-accepted.json"
    ))
    .unwrap();
    let rejected: Value = serde_json::from_slice(include_bytes!(
        "../../../../tests/ocpp-fixtures/corpus/wire/2.0.1/data-transfer-rejected.json"
    ))
    .unwrap();
    for (index, (payload, expected)) in [
        (
            serde_json::from_slice::<Value>(TRANSFER).unwrap()[3].clone(),
            accepted[2].clone(),
        ),
        (
            json!({"vendorId":VENDOR,"messageId":"Probe","data":false}),
            rejected[2].clone(),
        ),
        (
            json!({"vendorId":"secret://unknown-vendor","messageId":"Probe","data":null}),
            json!({"status":"UnknownVendorId"}),
        ),
        (
            json!({"vendorId":VENDOR,"messageId":"secret://unknown-message","data":[1,null]}),
            json!({"status":"UnknownMessageId"}),
        ),
    ]
    .into_iter()
    .enumerate()
    {
        let bytes = request(payload);
        let decoded = decode_call(&bytes).unwrap();
        assert!(!format!("{decoded:?}").contains("secret://"));
        let response = data_transfer_call(&bytes, &store, &mut state, &registry, now(1))
            .await
            .unwrap();
        assert_eq!(response[2], expected);
        assert_eq!(persisted(&store).await, state);
        assert_eq!(
            value(&state, "status"),
            Some(&TypedValue::Text(
                response[2]["status"].as_str().unwrap().to_owned()
            ))
        );
        assert_eq!(
            value(&state, "received_count"),
            Some(&TypedValue::UnsignedInteger(index as u64 + 1))
        );
        let durable = serde_json::to_string(&state).unwrap();
        assert!(!durable.contains("secret://"));
        assert!(!durable.contains(VENDOR));
    }
    shutdown(&store).await;
    drop(store);
    let store = db.open();
    let mut recovered = persisted(&store).await;
    recovered.connectivity = Connectivity::Connected {
        protocol: ProtocolEdition::Ocpp201,
        connected_at: now(10),
        last_message_at: None,
    };
    assert_eq!(
        data_transfer_call(TRANSFER, &store, &mut recovered, &registry, now(11))
            .await
            .unwrap()[2]["status"],
        "Accepted"
    );
    assert_eq!(
        value(&persisted(&store).await, "received_count"),
        Some(&TypedValue::UnsignedInteger(5))
    );
    shutdown(&store).await;
}

#[tokio::test]
async fn invalid_denied_and_failed_storage_never_change_committed_state() {
    let db = Database::new();
    let store = db.open();
    let registry = registry(Arc::new(Probe));
    let mut state = snapshot();
    assert_eq!(
        data_transfer_call(TRANSFER, &store, &mut state, &registry, now(1))
            .await
            .unwrap_err()
            .code,
        OcppErrorCode::ProtocolError
    );
    state = registered(&store, None).await;
    let before = state.clone();
    for payload in [
        json!({}),
        json!({"vendorId":null}),
        json!({"vendorId":1}),
        json!({"vendorId":"x","messageId":null}),
        json!({"vendorId":"x","unexpected":true}),
        json!({"vendorId":"x".repeat(256)}),
        json!({"vendorId":"x","messageId":"x".repeat(51)}),
        json!({"vendorId":"x","data":"é".repeat(MAX_DATA_BYTES)}),
        json!({"vendorId":"x","customData":null}),
        json!({"vendorId":"x","customData":{}}),
        json!({"vendorId":"x","customData":{"vendorId":null}}),
        json!({"vendorId":"x","customData":{"vendorId":"x".repeat(256)}}),
    ] {
        let response =
            data_transfer_call(&request(payload), &store, &mut state, &registry, now(2)).await;
        assert!(response.is_err());
        assert_eq!(state, before);
        assert_eq!(persisted(&store).await, before);
    }
    shutdown(&store).await;
    assert_eq!(
        data_transfer_call(TRANSFER, &store, &mut state, &registry, now(3))
            .await
            .unwrap_err()
            .code,
        OcppErrorCode::InternalError
    );
    assert_eq!(state, before);
}

#[tokio::test]
async fn omitted_null_and_unicode_bounds_are_exact_capabilities() {
    let db = Database::new();
    let store = db.open();
    let mut state = registered(&store, None).await;
    let provider: Arc<dyn Provider> = Arc::new(Probe);
    let registry = Registry::new(vec![(
        (Capability {
            vendor_id: "é".repeat(255),
            message_id: None,
        }),
        provider,
    )])
    .unwrap();
    let omitted = json!({"vendorId":"é".repeat(255)});
    assert_eq!(
        data_transfer_call(
            &request(omitted.clone()),
            &store,
            &mut state,
            &registry,
            now(1)
        )
        .await
        .unwrap()[2]["status"],
        "Rejected"
    );
    let null = json!({"vendorId":"é".repeat(255),"data":null,"customData":{"vendorId":"extension","opaque":[false,null]}});
    let decoded = observation(null.clone()).unwrap();
    assert_eq!(
        decoded.data.as_ref().map(OpaqueData::expose),
        Some(&Value::Null)
    );
    assert_eq!(
        data_transfer_call(&request(null), &store, &mut state, &registry, now(2))
            .await
            .unwrap()[2]["status"],
        "Accepted"
    );
    let mut empty = omitted;
    empty["messageId"] = json!("");
    assert_eq!(
        data_transfer_call(&request(empty), &store, &mut state, &registry, now(3))
            .await
            .unwrap()[2],
        json!({"status":"UnknownMessageId"})
    );
    let observation = Observation {
        vendor_id: String::new(),
        message_id: Some(String::new()),
        data: Some(OpaqueData::new(json!(["é".repeat(MAX_DATA_BYTES / 4)])).unwrap()),
        custom_data: Some(OpaqueData::new(json!({"vendorId":""})).unwrap()),
    };
    assert!(observation.validate().is_ok());
    shutdown(&store).await;
}

#[tokio::test]
async fn serialized_json_byte_limit_includes_escaping_and_preserves_state_on_overflow() {
    let db = Database::new();
    let store = db.open();
    let mut state = registered(&store, None).await;
    let registry = Registry::new(vec![]).unwrap();
    // Each quote needs two JSON bytes; the enclosing quotes consume two more.
    let data = "\"".repeat((MAX_DATA_BYTES - 2) / 2);
    let payload = json!({"vendorId":"unknown","data":data});
    let response = data_transfer_call(&request(payload), &store, &mut state, &registry, now(1))
        .await
        .unwrap();
    assert_eq!(response[2], json!({"status":"UnknownVendorId"}));
    assert_eq!(
        value(&state, "request_bytes"),
        Some(&TypedValue::UnsignedInteger(MAX_DATA_BYTES as u64 + 7)),
    );
    let before = state.clone();
    let oversized = json!({"vendorId":"unknown","data":format!("{data}x")});
    assert!(
        data_transfer_call(&request(oversized), &store, &mut state, &registry, now(2),)
            .await
            .is_err()
    );
    assert_eq!(state, before);
    assert_eq!(persisted(&store).await, before);
    shutdown(&store).await;
}

#[tokio::test(start_paused = true)]
async fn stalled_provider_times_out_without_a_success_or_commit() {
    let db = Database::new();
    let store = db.open();
    let mut state = registered(&store, None).await;
    let before = state.clone();
    let registry = registry(Arc::new(DelayedProbe(tokio::sync::Notify::new())));
    let result = data_transfer_call(TRANSFER, &store, &mut state, &registry, now(1));
    tokio::pin!(result);
    tokio::select! {
        biased;
        value = &mut result => panic!("premature provider outcome: {value:?}"),
        () = tokio::time::sleep(Duration::from_secs(1)) => {},
    }
    assert_eq!(result.await.unwrap_err().code, OcppErrorCode::InternalError);
    assert_eq!(persisted(&store).await, before);
    shutdown(&store).await;
}

#[tokio::test]
async fn ocpp16_registration_does_not_authorize_ocpp201_data_transfer() {
    let db = Database::new();
    let store = db.open();
    let mut state: uob_contracts::StationSnapshot = serde_json::from_slice(include_bytes!(
        "../../../../crates/contracts/tests/fixtures/station-snapshot-ocpp16-v1.json"
    ))
    .unwrap();
    uob_protocol_adapter::v16::registration_call(
        include_bytes!("../../../../tests/ocpp-fixtures/corpus/wire/1.6/boot-notification.json"),
        &store,
        &mut state,
        uob_application::registration::RegistrationDecision::Accepted,
        60,
        now(0),
    )
    .await
    .unwrap();
    let before = state.clone();
    assert_eq!(
        data_transfer_call(
            TRANSFER,
            &store,
            &mut state,
            &registry(Arc::new(Probe)),
            now(1)
        )
        .await
        .unwrap_err()
        .code,
        OcppErrorCode::ProtocolError
    );
    assert_eq!(state, before);
    assert_eq!(persisted(&store).await, before);
    shutdown(&store).await;
}

#[tokio::test]
async fn registry_rejects_ambiguous_routes_and_provider_spoofed_statuses() {
    struct InvalidProvider;
    impl Provider for InvalidProvider {
        fn handle<'a>(
            &'a self,
            _: &'a uob_contracts::StationSnapshot,
            _: &'a Observation,
        ) -> std::pin::Pin<Box<dyn Future<Output = Result<Reply, Error>> + Send + 'a>> {
            Box::pin(async { Ok(fixture_reply(Status::UnknownVendorId)) })
        }
    }
    let capability = Capability {
        vendor_id: VENDOR.to_owned(),
        message_id: Some("Probe".to_owned()),
    };
    let provider: Arc<dyn Provider> = Arc::new(InvalidProvider);
    assert!(matches!(
        Registry::new(vec![
            (capability.clone(), provider.clone()),
            (capability.clone(), provider.clone())
        ]),
        Err(Error::InvalidRegistry)
    ));
    let entries = (0..=MAX_CAPABILITIES)
        .map(|index| {
            (
                Capability {
                    vendor_id: format!("vendor-{index}"),
                    message_id: None,
                },
                provider.clone(),
            )
        })
        .collect();
    assert!(matches!(
        Registry::new(entries),
        Err(Error::InvalidRegistry)
    ));
    let db = Database::new();
    let store = db.open();
    let mut state = registered(&store, None).await;
    let before = state.clone();
    let registry = Registry::new(vec![(capability, provider)]).unwrap();
    assert!(
        data_transfer_call(TRANSFER, &store, &mut state, &registry, now(1))
            .await
            .is_err()
    );
    assert_eq!(persisted(&store).await, before);
    shutdown(&store).await;
}
