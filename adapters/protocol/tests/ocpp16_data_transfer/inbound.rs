use super::support::*;
use serde_json::{Value, json};
use std::{sync::Arc, time::Duration};
use uob_application::data_transfer::{
    Capability, MAX_DATA_BYTES, Observation, OpaqueData, Provider, Registry,
};
use uob_contracts::{Connectivity, ProtocolEdition, TypedValue};
use uob_protocol_adapter::{
    OcppErrorCode,
    v16::{data_transfer::data_transfer_call, decode_call},
};

#[tokio::test]
async fn native_statuses_commit_without_retaining_vendor_content() {
    let db = Database::new();
    let store = db.open();
    let mut state = registered(&store).await;
    let registry = registry(Arc::new(Probe));
    let cases = [
        (
            json!({"vendorId":VENDOR,"messageId":"Probe","data":"ping"}),
            include_bytes!(
                "../../../../tests/ocpp-fixtures/corpus/wire/1.6/data-transfer-accepted.json"
            )
            .as_slice(),
        ),
        (
            json!({"vendorId":VENDOR,"messageId":"Probe","data":"secret://fixture/not-a-probe"}),
            include_bytes!(
                "../../../../tests/ocpp-fixtures/corpus/wire/1.6/data-transfer-rejected.json"
            )
            .as_slice(),
        ),
        (
            json!({"vendorId":"secret://unknown-vendor","messageId":"Probe","data":"secret://fixture/payload"}),
            include_bytes!(
                "../../../../tests/ocpp-fixtures/corpus/wire/1.6/data-transfer-unknown-vendor.json"
            )
            .as_slice(),
        ),
        (
            json!({"vendorId":VENDOR,"messageId":"secret://unknown-message","data":"secret://fixture/payload"}),
            include_bytes!(
                "../../../../tests/ocpp-fixtures/corpus/wire/1.6/data-transfer-unknown-message.json"
            )
            .as_slice(),
        ),
    ];
    for (index, (payload, expected)) in cases.into_iter().enumerate() {
        let bytes = request(payload);
        let decoded = decode_call(&bytes).unwrap();
        assert!(!format!("{decoded:?}").contains("secret://fixture"));
        let response = data_transfer_call(&bytes, &store, &mut state, &registry, now(1))
            .await
            .unwrap();
        assert_eq!(response, serde_json::from_slice::<Value>(expected).unwrap());
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
        protocol: ProtocolEdition::Ocpp16j,
        connected_at: now(10),
        last_message_at: None,
    };
    assert_eq!(
        data_transfer_call(TRANSFER, &store, &mut recovered, &registry, now(11))
            .await
            .unwrap()[2],
        json!({"status":"Accepted","data":"pong"})
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
    state = registered(&store).await;
    let before = state.clone();
    for payload in [
        json!({}),
        json!({"vendorId":null}),
        json!({"vendorId":1}),
        json!({"vendorId":"x","messageId":null}),
        json!({"vendorId":"x","data":{}}),
        json!({"vendorId":"x","unexpected":true}),
        json!({"vendorId":"x".repeat(256)}),
        json!({"vendorId":"x","messageId":"x".repeat(51)}),
        json!({"vendorId":"x","data":"é".repeat(MAX_DATA_BYTES / 2 + 1)}),
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
async fn omitted_message_and_unicode_bounds_are_exact_capabilities() {
    let db = Database::new();
    let store = db.open();
    let mut state = registered(&store).await;
    let provider: Arc<dyn Provider> = Arc::new(Probe);
    let registry = Registry::new(vec![(
        Capability {
            vendor_id: "é".repeat(255),
            message_id: None,
        },
        provider,
    )])
    .unwrap();
    let payload = json!({"vendorId":"é".repeat(255),"data":"ping"});
    assert_eq!(
        data_transfer_call(
            &request(payload.clone()),
            &store,
            &mut state,
            &registry,
            now(1)
        )
        .await
        .unwrap()[2]["status"],
        "Accepted"
    );
    let mut with_empty_message = payload;
    with_empty_message["messageId"] = json!("");
    assert_eq!(
        data_transfer_call(
            &request(with_empty_message),
            &store,
            &mut state,
            &registry,
            now(2)
        )
        .await
        .unwrap()[2],
        json!({"status":"UnknownMessageId"})
    );
    let observation = Observation {
        vendor_id: String::new(),
        message_id: Some(String::new()),
        data: Some(OpaqueData::new("é".repeat(MAX_DATA_BYTES / 2)).unwrap()),
    };
    assert!(observation.validate().is_ok());
    shutdown(&store).await;
}

#[tokio::test(start_paused = true)]
async fn stalled_provider_times_out_without_a_success_or_commit() {
    let db = Database::new();
    let store = db.open();
    let mut state = registered(&store).await;
    let before = state.clone();
    let delayed = Arc::new(DelayedProbe(tokio::sync::Notify::new()));
    let registry = registry(delayed);
    let result = data_transfer_call(TRANSFER, &store, &mut state, &registry, now(1));
    tokio::pin!(result);
    tokio::select! {
        biased;
        value = &mut result => panic!("premature provider outcome: {value:?}"),
        () = tokio::time::sleep(Duration::from_secs(1)) => {},
    }
    let error = result.await.unwrap_err();
    assert_eq!(error.code, OcppErrorCode::InternalError);
    assert_eq!(persisted(&store).await, before);
    shutdown(&store).await;
}

#[tokio::test]
async fn registry_rejects_ambiguous_routes_and_provider_spoofed_unknown_status() {
    use std::{future::Future, pin::Pin};
    use uob_application::data_transfer::{Error, MAX_CAPABILITIES, Reply, Status};
    use uob_contracts::StationSnapshot;
    struct InvalidProvider;
    impl Provider for InvalidProvider {
        fn handle<'a>(
            &'a self,
            _: &'a StationSnapshot,
            _: &'a Observation,
        ) -> Pin<Box<dyn Future<Output = Result<Reply, Error>> + Send + 'a>> {
            Box::pin(async {
                Ok(Reply {
                    status: Status::UnknownVendorId,
                    data: None,
                })
            })
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
        .map(|i| {
            (
                Capability {
                    vendor_id: format!("vendor-{i}"),
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
    let mut state = registered(&store).await;
    let before = state.clone();
    let registry = Registry::new(vec![(capability, provider)]).unwrap();
    assert!(
        data_transfer_call(TRANSFER, &store, &mut state, &registry, now(1))
            .await
            .is_err()
    );
    assert_eq!(persisted(&store).await, before);
    assert_eq!(state, before);
    shutdown(&store).await;
}
