use super::support::*;
use crate::endpoint_support::{self, SECRET, TEST_BOUND};
use futures::{SinkExt, StreamExt};
use serde_json::{Value, json};
use std::{sync::Arc, time::Duration};
use tokio::time::timeout;
use tokio_tungstenite::tungstenite::Message;
use uob_application::{
    CommandClock, FlowDiagnostics,
    capture::{CaptureFilter, CaptureGrant, CaptureLevel, CaptureManager, CapturePermission},
};
use uob_contracts::{Connectivity, Environment, ProtocolEdition, UtcTimestamp};
use uob_protocol_adapter::{
    CallSessionConfiguration, spawn_call_session, v201::data_transfer::complete_data_transfer,
};

type Peer =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;
struct Clock;
impl CommandClock for Clock {
    fn now(&self) -> UtcTimestamp {
        now(1)
    }
}

#[tokio::test]
async fn delayed_wire_reply_reconnect_and_diagnostics_preserve_boundaries() {
    let application = endpoint_support::application(Environment::Demo, None);
    let capture = CaptureManager::with_resources(true, application.health().resources().clone());
    let grant = CaptureGrant::new(
        application.identity().bridge_id.clone(),
        vec![CapturePermission::Capture, CapturePermission::Read],
        None,
        None,
    )
    .unwrap();
    let session = capture
        .start(
            &grant,
            CaptureFilter {
                bridge: application.identity().bridge_id.clone(),
                station: None,
                target: None,
            },
            CaptureLevel::Metadata,
            None,
        )
        .unwrap();
    let traces = capture.lease(&grant, session.id, false).unwrap();
    let flow = FlowDiagnostics::retained(
        application.runtime_identity().process_instance_id.clone(),
        application.identity().bridge_id.clone(),
        capture.clone(),
        Arc::new(Clock),
    );
    let application = application.with_diagnostics(flow);
    let (address, mut connections, _server) = endpoint_support::plaintext_endpoint(None).await;
    let db = Database::new();
    let store = db.open();
    registered(&store, None).await;
    let delayed = Arc::new(DelayedProbe(tokio::sync::Notify::new()));
    let registry = registry(delayed.clone());
    for epoch in 0..2 {
        let (mut peer, _) = endpoint_support::connect_plain(address, "alpha", SECRET, "ocpp2.0.1")
            .await
            .unwrap();
        let connection = timeout(TEST_BOUND, connections.receive())
            .await
            .unwrap()
            .unwrap();
        let (_, mut outputs, task) = spawn_call_session(
            connection,
            &application,
            CallSessionConfiguration {
                pending_call_capacity: 4,
                incoming_call_capacity: 4,
                diagnostic_capacity: 8,
                response_timeout: TEST_BOUND,
            },
        )
        .unwrap();
        exchange(&mut peer, &mut outputs, &store, &registry, &delayed, epoch).await;
        task.shutdown(TEST_BOUND).await.unwrap();
    }
    let mut after = None;
    let mut stages = std::collections::BTreeSet::new();
    while let Some(record) = traces.read_after(after).unwrap().record {
        after = Some(record.sequence);
        let encoded = std::str::from_utf8(record.diagnostic.encoded_json()).unwrap();
        assert!(!encoded.contains("secret://fixture"));
        assert!(!encoded.contains(VENDOR));
        let trace: Value = serde_json::from_str(encoded).unwrap();
        stages.insert(trace["stage"].as_str().unwrap().to_owned());
    }
    assert!(stages.contains("ocpp.receive"));
    assert!(stages.contains("ocpp.send"));
    shutdown(&store).await;
}

async fn exchange(
    peer: &mut Peer,
    outputs: &mut uob_protocol_adapter::CallSessionOutputs,
    store: &Store,
    registry: &uob_application::data_transfer201::Registry,
    delayed: &DelayedProbe,
    epoch: u8,
) {
    let mut state = persisted(store).await;
    state.connectivity = Connectivity::Connected {
        protocol: ProtocolEdition::Ocpp201,
        connected_at: now(10 + epoch),
        last_message_at: None,
    };
    // The same OCPP message ID on a new socket is an invocation, not a replay request.
    peer.send(Message::Text(
        String::from_utf8(TRANSFER.to_vec()).unwrap().into(),
    ))
    .await
    .unwrap();
    let incoming = timeout(TEST_BOUND, outputs.incoming.receive())
        .await
        .unwrap()
        .unwrap();
    let response = {
        let handling =
            complete_data_transfer(incoming.call, store, &mut state, registry, now(20 + epoch));
        tokio::pin!(handling);
        tokio::select! {
            biased;
            result = &mut handling => panic!("provider was not released: {result:?}"),
            () = tokio::time::sleep(Duration::from_millis(20)) => {},
        }
        assert!(
            timeout(Duration::from_millis(20), peer.next())
                .await
                .is_err()
        );
        delayed.0.notify_one();
        handling.await.unwrap()
    };
    assert_eq!(
        value(&persisted(store).await, "received_count"),
        Some(&uob_contracts::TypedValue::UnsignedInteger(
            u64::from(epoch) * 2 + 1
        ))
    );
    incoming.responder.respond(&response[2]).unwrap();
    let frame = timeout(TEST_BOUND, peer.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap()
        .into_text()
        .unwrap();
    let expected: Value = serde_json::from_slice(include_bytes!(
        "../../../../tests/ocpp-fixtures/corpus/wire/2.0.1/data-transfer-accepted.json"
    ))
    .unwrap();
    assert_eq!(serde_json::from_str::<Value>(&frame).unwrap(), expected);
    peer.send(Message::Text(json!([2,"unknown","DataTransfer",{"vendorId":"unregistered","data":"secret://fixture/201-unknown"}]).to_string().into())).await.unwrap();
    let incoming = timeout(TEST_BOUND, outputs.incoming.receive())
        .await
        .unwrap()
        .unwrap();
    let response =
        complete_data_transfer(incoming.call, store, &mut state, registry, now(30 + epoch))
            .await
            .unwrap();
    incoming.responder.respond(&response[2]).unwrap();
    let frame = timeout(TEST_BOUND, peer.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap()
        .into_text()
        .unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&frame).unwrap()[2],
        json!({"status":"UnknownVendorId"})
    );
}
