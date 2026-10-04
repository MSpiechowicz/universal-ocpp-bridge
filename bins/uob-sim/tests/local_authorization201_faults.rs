use serde_json::{Value, json};
use std::fs;
use std::time::Duration;
use tokio::net::TcpListener;
use tokio::time::timeout;
#[path = "local_authorization201/process.rs"]
mod common;
use common::*;

#[tokio::test]
async fn binary_lost_transaction_ack_then_kill_recovers_uncertain_without_retry() {
    let directory = PrivateDirectory::new();
    let state = directory.0.join("state.json");
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let config = directory.write("config.toml", &format!("schema_version=1\n[[stations]]\nid='alpha'\nendpoint='ws://{address}'\nocpp_version='2.0.1'\nrequest_timeout_ms=3000\nreconnect=false\n[stations.local_authorization]\nprivate_state_file='{}'\n",state.display()));
    let first = prefix()
        + &step("connect", "connect", "")
        + &native_boot("boot", "Accepted")
        + &step(
            "installed",
            "await_local_authorization",
            "expect_response={listVersion=7,listEntries=1}",
        )
        + &step("offline", "csms_offline", "")
        + &facts()
        + &step("reconnect", "csms_reconnect", "");
    let first = directory.write("first.toml", &first);
    let second = directory.write(
        "second.toml",
        &(prefix() + &step("connect", "connect", "") + &native_boot("boot", "Accepted")),
    );
    let (observed, observation) = tokio::sync::oneshot::channel();
    let (killed, termination) = tokio::sync::oneshot::channel();
    let peer = tokio::spawn(async move {
        let mut socket = accept(&listener).await;
        boot(&mut socket).await;
        let full: Value = serde_json::from_str(include_str!(
            "../../../tests/ocpp-fixtures/corpus/wire/2.0.1/local-list-full.json"
        ))
        .unwrap();
        send(&mut socket, full).await;
        assert_eq!(receive(&mut socket).await[2]["status"], "Accepted");
        closed(&mut socket).await;
        let mut socket = accept(&listener).await;
        boot(&mut socket).await;
        let fact = receive(&mut socket).await;
        assert_eq!(fact[2], "TransactionEvent");
        assert_eq!(fact[3], start_fact());
        observed.send(()).unwrap();
        termination.await.unwrap();
        drop(socket);
        let mut socket = accept(&listener).await;
        boot(&mut socket).await;
        // Accepted Boot does not turn a potentially delivered fact into Pending.
        closed(&mut socket).await;
    });
    let mut first = Process::start(&config, &first);
    timeout(BOUND, observation).await.unwrap().unwrap();
    let before: Value = serde_json::from_slice(&fs::read(&state).unwrap()).unwrap();
    assert_eq!(before["offline"][0]["delivery"], "Sending");
    assert_private_output(&first.kill());
    killed.send(()).unwrap();
    let mut second = Process::start(&config, &second);
    let output = second.finish().await;
    assert!(!output.status.success());
    assert_private_output(&output);
    let recovered: Value = serde_json::from_slice(&fs::read(&state).unwrap()).unwrap();
    assert_eq!(recovered["offline"].as_array().unwrap().len(), 2);
    assert_eq!(recovered["offline"][0]["delivery"], "Uncertain");
    assert_eq!(recovered["offline"][0]["payload"], start_fact());
    assert_eq!(recovered["offline"][1]["delivery"], "Pending");
    assert_eq!(recovered["offline"][1]["payload"], end_fact());
    timeout(BOUND, peer).await.unwrap().unwrap();
}

#[tokio::test]
async fn binary_delayed_ack_and_duplicate_native_identity_commit_once() {
    let directory = PrivateDirectory::new();
    let state = directory.0.join("state.json");
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let config = directory.write("config.toml",&format!("schema_version=1\n[[stations]]\nid='alpha'\nendpoint='ws://{address}'\nocpp_version='2.0.1'\nreconnect=false\n[stations.local_authorization]\nprivate_state_file='{}'\n",state.display()));
    let scenario = prefix()
        + &step("connect", "connect", "")
        + &native_boot("boot", "Accepted")
        + &step("delay", "delay_local_reply", "duration_ms=200")
        + &step(
            "committed",
            "await_local_authorization",
            "expect_response={listVersion=8,listEntries=1}",
        )
        + &step("window", "wait", "duration_ms=500")
        + &step("disconnect", "disconnect", "");
    let scenario = directory.write("scenario.toml", &scenario);
    let peer = tokio::spawn(async move {
        let mut socket = accept(&listener).await;
        boot(&mut socket).await;
        tokio::time::sleep(Duration::from_millis(50)).await;
        let full: Value = serde_json::from_str(include_str!(
            "../../../tests/ocpp-fixtures/corpus/wire/2.0.1/local-list-full.json"
        ))
        .unwrap();
        send(&mut socket, full).await;
        assert!(
            timeout(Duration::from_millis(75), receive(&mut socket))
                .await
                .is_err()
        );
        assert_eq!(receive(&mut socket).await[2]["status"], "Accepted");
        let diff = json!([2,"unique-diff-8","SendLocalList",{"versionNumber":8,"updateType":"Differential"}]);
        send(&mut socket, diff.clone()).await;
        assert_eq!(receive(&mut socket).await[2]["status"], "Accepted");
        send(&mut socket, diff).await;
        assert_eq!(receive(&mut socket).await[2]["status"], "Accepted");
        send(&mut socket,json!([2,"other-diff-8","SendLocalList",{"versionNumber":8,"updateType":"Differential"}])).await;
        assert_eq!(receive(&mut socket).await[2]["status"], "VersionMismatch");
        send(&mut socket,json!([2,"full-missing-info","SendLocalList",{"versionNumber":20,"updateType":"Full","localAuthorizationList":[{"idToken":{"idToken":"native-other-112","type":"Local"}}]}])).await;
        assert_eq!(receive(&mut socket).await[2]["status"], "Failed");
        send(&mut socket, json!([2, "clear-cache", "ClearCache", {}])).await;
        assert_eq!(receive(&mut socket).await[2]["status"], "Accepted");
        send(
            &mut socket,
            json!([2, "query-version", "GetLocalListVersion", {}]),
        )
        .await;
        assert_eq!(receive(&mut socket).await[2]["versionNumber"], 8);
        closed(&mut socket).await;
    });
    let mut process = Process::start(&config, &scenario);
    let output = process.finish().await;
    assert_private_output(&output);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    timeout(BOUND, peer).await.unwrap().unwrap();
}
