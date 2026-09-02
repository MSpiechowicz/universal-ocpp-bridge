mod endpoint_support;

use std::time::Duration;

use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde_json::{Value, json};
use tokio::{net::TcpListener, task::JoinHandle, time::timeout};
use tokio_tungstenite::tungstenite::Message;
use uob_contracts::{CorrelationId, Environment, ProtocolActionName};
use uob_hostile_websocket_peer::{Peer, PeerConfig};
use uob_protocol_adapter::{
    CallSessionConfiguration, CallSessionDiagnostic, CallSessionHandle, CallSessionOutputs,
    CallSessionTask, OcppEndpoint, OutboundCall, SessionCallOutcome, TransmissionUncertainReason,
    spawn_call_session,
};

use endpoint_support::{SECRET, TEST_BOUND};

struct RunningSession {
    peer: Peer,
    handle: CallSessionHandle,
    outputs: CallSessionOutputs,
    task: CallSessionTask,
    server: JoinHandle<()>,
}

async fn session(protocol: &str, response_timeout: Duration) -> RunningSession {
    let application = endpoint_support::application(Environment::Demo, None);
    let (endpoint, mut accepted) = OcppEndpoint::new(
        endpoint_support::authenticator(
            uob_protocol_adapter::StationAuthenticationMode::Credential,
            None,
        ),
        &application,
        4,
    )
    .expect("endpoint");
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("listener");
    let address = listener.local_addr().expect("address");
    let server = tokio::spawn(async move {
        endpoint
            .serve_plaintext(listener)
            .await
            .expect("endpoint server");
    });
    let authorization = STANDARD.encode([b"alpha:".as_slice(), SECRET].concat());
    let peer = Peer::connect(PeerConfig {
        endpoint: format!("ws://{address}/ocpp/alpha"),
        subprotocol: protocol.to_owned(),
        max_outbound_bytes: 512 * 1024,
        max_inbound_bytes: 512 * 1024,
        observation_capacity: 64,
        authorization: Some(format!("Basic {authorization}")),
    })
    .await
    .expect("raw authenticated peer");
    let connection = timeout(TEST_BOUND, accepted.receive())
        .await
        .expect("endpoint handoff bound")
        .expect("endpoint handoff");
    let (handle, outputs, task) = spawn_call_session(
        connection,
        &application,
        CallSessionConfiguration {
            pending_call_capacity: 8,
            incoming_call_capacity: 8,
            diagnostic_capacity: 16,
            response_timeout,
        },
    )
    .expect("call session");
    RunningSession {
        peer,
        handle,
        outputs,
        task,
        server,
    }
}

async fn receive_json(peer: &mut Peer) -> Value {
    let message = timeout(TEST_BOUND, peer.receive())
        .await
        .expect("peer receive bound")
        .expect("peer frame");
    let text = message.into_text().expect("text frame");
    serde_json::from_str(&text).expect("JSON frame")
}

fn outbound(id: &str, correlation: &str) -> OutboundCall {
    OutboundCall {
        message_id: id.to_owned(),
        action: ProtocolActionName::new("Reset").expect("action"),
        payload: json!({ "type": "Immediate" }),
        correlation_id: CorrelationId::new(correlation).expect("correlation"),
    }
}

async fn assert_bounded_application_response(running: &mut RunningSession) {
    running
        .peer
        .send_text(r#"[2,"bounded-response","Heartbeat",{}]"#.to_owned())
        .await
        .expect("bounded response call");
    let bounded = timeout(TEST_BOUND, running.outputs.incoming.receive())
        .await
        .expect("bounded response delivery")
        .expect("bounded response call");
    assert!(
        bounded
            .responder
            .respond(&json!({ "data": "x".repeat(256 * 1024) }))
            .is_err()
    );
    let error = receive_json(&mut running.peer).await;
    assert_eq!(error[1], "bounded-response");
    assert_eq!(error[2], "InternalError");
}

#[tokio::test]
async fn hostile_calls_are_validated_and_reads_continue_for_both_editions() {
    for protocol in ["ocpp1.6", "ocpp2.0.1"] {
        let mut running = session(protocol, Duration::from_secs(1)).await;

        running
            .peer
            .send_text(r#"[2,"broken""#.to_owned())
            .await
            .expect("invalid JSON");
        assert!(matches!(
            timeout(TEST_BOUND, running.outputs.diagnostics.recv())
                .await
                .expect("malformed diagnostic bound")
                .expect("malformed diagnostic"),
            CallSessionDiagnostic::MalformedFrame {
                message_id: None,
                field_path: "/",
            }
        ));

        running
            .peer
            .send_text(r#"[2,"bad-shape","Heartbeat",[]]"#.to_owned())
            .await
            .expect("malformed call");
        let error = receive_json(&mut running.peer).await;
        assert_eq!(error[0], 4);
        assert_eq!(error[1], "bad-shape");
        assert_eq!(error[2], "ProtocolError");
        assert_eq!(error[4]["path"], "/3");

        running
            .peer
            .send_text(r#"[2,"unknown","NoSuchAction",{}]"#.to_owned())
            .await
            .expect("unknown action");
        let error = receive_json(&mut running.peer).await;
        assert_eq!(error[1], "unknown");
        assert_eq!(error[2], "NotImplemented");

        for id in ["first", "second"] {
            running
                .peer
                .send_text(format!(r#"[2,"{id}","Heartbeat",{{}}]"#))
                .await
                .expect("heartbeat call");
        }
        let first = timeout(TEST_BOUND, running.outputs.incoming.receive())
            .await
            .expect("first application delivery")
            .expect("first call");
        let second = timeout(TEST_BOUND, running.outputs.incoming.receive())
            .await
            .expect("socket reader remained active")
            .expect("second call");
        assert_eq!(first.call.message_id, "first");
        assert_eq!(second.call.message_id, "second");

        second
            .responder
            .respond(&json!({ "order": 2 }))
            .expect("second response admitted");
        first
            .responder
            .respond(&json!({ "order": 1 }))
            .expect("first response admitted");
        assert_eq!(receive_json(&mut running.peer).await[1], "second");
        assert_eq!(receive_json(&mut running.peer).await[1], "first");

        running
            .peer
            .send_text(r#"[2,"duplicate","Heartbeat",{}]"#.to_owned())
            .await
            .expect("first duplicate ID");
        let original = timeout(TEST_BOUND, running.outputs.incoming.receive())
            .await
            .expect("original delivery")
            .expect("original call");
        running
            .peer
            .send_text(r#"[2,"duplicate","Heartbeat",{}]"#.to_owned())
            .await
            .expect("duplicate ID");
        let error = receive_json(&mut running.peer).await;
        assert_eq!(error[1], "duplicate");
        assert_eq!(error[2], "OccurrenceConstraintViolation");
        original
            .responder
            .respond(&json!({}))
            .expect("original response admitted");
        assert_eq!(receive_json(&mut running.peer).await[0], 3);
        running
            .peer
            .send_text(r#"[2,"duplicate","Heartbeat",{}]"#.to_owned())
            .await
            .expect("completed duplicate ID");
        assert_eq!(
            receive_json(&mut running.peer).await[2],
            "OccurrenceConstraintViolation"
        );

        assert_bounded_application_response(&mut running).await;

        running.task.shutdown(TEST_BOUND).await.expect("shutdown");
        running.server.abort();
    }
}

#[tokio::test]
async fn oversized_hostile_messages_close_both_editions_before_application_delivery() {
    for protocol in ["ocpp1.6", "ocpp2.0.1"] {
        let RunningSession {
            mut peer,
            mut outputs,
            task,
            server,
            ..
        } = session(protocol, Duration::from_secs(1)).await;
        peer.send_text("x".repeat(256 * 1024 + 1))
            .await
            .expect("hostile peer permits above-bridge-bound payload");
        task.wait().await.expect("session stops at transport bound");
        let terminal = timeout(TEST_BOUND, peer.receive())
            .await
            .expect("terminal peer outcome bound");
        assert!(terminal.is_err() || matches!(terminal, Ok(Message::Close(_))));
        assert!(outputs.incoming.receive().await.is_none());
        server.abort();
    }
}

#[tokio::test]
async fn replies_correlate_out_of_order_and_timeouts_remain_uncertain() {
    for protocol in ["ocpp1.6", "ocpp2.0.1"] {
        let mut running = session(protocol, Duration::from_millis(200)).await;
        let first = running
            .handle
            .try_call(outbound("one", "trace-one"))
            .expect("first call admitted");
        let second = running
            .handle
            .try_call(outbound("two", "trace-two"))
            .expect("second call admitted");
        let first_frame = receive_json(&mut running.peer).await;
        let second_frame = receive_json(&mut running.peer).await;
        assert_eq!(
            (&first_frame[1], &second_frame[1]),
            (&json!("one"), &json!("two"))
        );

        running
            .peer
            .send_text(r#"[3,"two",{"status":"Accepted"}]"#.to_owned())
            .await
            .expect("second response");
        running
            .peer
            .send_text(
                r#"[4,"one","ProtocolError","private peer detail",{"path":"/type","secret":"discarded"}]"#
                    .to_owned(),
            )
            .await
            .expect("first error");
        assert!(matches!(
            second.receive().await,
            SessionCallOutcome::Result { correlation_id, .. }
                if correlation_id.as_str() == "trace-two"
        ));
        assert!(matches!(
            first.receive().await,
            SessionCallOutcome::Error { error, correlation_id }
                if correlation_id.as_str() == "trace-one"
                    && error.code == "ProtocolError"
                    && error.field_path.as_deref() == Some("/type")
        ));
        assert!(matches!(
            running
                .handle
                .try_call(outbound("two", "trace-reused"))
                .expect("duplicate reaches lifecycle classification")
                .receive()
                .await,
            SessionCallOutcome::NotTransmitted { correlation_id, .. }
                if correlation_id.as_str() == "trace-reused"
        ));

        let delayed = running
            .handle
            .try_call(outbound("delayed", "trace-delayed"))
            .expect("delayed call admitted");
        assert_eq!(receive_json(&mut running.peer).await[1], "delayed");
        assert!(matches!(
            delayed.receive().await,
            SessionCallOutcome::TimedOut { correlation_id }
                if correlation_id.as_str() == "trace-delayed"
        ));
        running
            .peer
            .send_text(r#"[3,"delayed",{"status":"late"}]"#.to_owned())
            .await
            .expect("late response");
        assert!(matches!(
            timeout(TEST_BOUND, running.outputs.diagnostics.recv())
                .await
                .expect("late diagnostic bound")
                .expect("late diagnostic"),
            CallSessionDiagnostic::LateResponse { message_id, correlation_id }
                if message_id == "delayed" && correlation_id.as_str() == "trace-delayed"
        ));

        let disconnected = running
            .handle
            .try_call(outbound("disconnect", "trace-disconnect"))
            .expect("disconnect call admitted");
        assert_eq!(receive_json(&mut running.peer).await[1], "disconnect");
        running.peer.disconnect().await.expect("peer disconnect");
        assert!(matches!(
            disconnected.receive().await,
            SessionCallOutcome::TransmissionUncertain {
                reason: TransmissionUncertainReason::Disconnected,
                correlation_id,
            } if correlation_id.as_str() == "trace-disconnect"
        ));
        running.task.wait().await.expect("session stopped");
        running.server.abort();
    }
}
