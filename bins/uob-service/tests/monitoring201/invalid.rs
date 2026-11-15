use super::support::*;
use serde_json::{Value, json};

fn monitor(value: i32, kind: &str, severity: i32, id: Option<i32>) -> Value {
    let mut entry = json!({"value":value,"type":kind,"severity":severity,
        "component":{"name":"Connector","evse":{"id":1,"connectorId":1}},
        "variable":{"name":"Temperature"}});
    if let Some(id) = id {
        entry["id"] = json!(id);
    }
    entry
}

#[tokio::test]
async fn invalid_monitor_commands_are_refused_before_anything_is_transmitted() {
    let (fixture, _process) = launch(ALL_OPTIONS).await;
    let client = client();
    let mut socket = connected(&fixture, &client).await;
    learn_limits(&client, &fixture, &mut socket).await;

    let cases = [
        (
            "severity",
            "SetVariableMonitoring",
            json!({"setMonitoringData":[monitor(80, "UpperThreshold", 10, None)]}),
        ),
        (
            "negative-delta",
            "SetVariableMonitoring",
            json!({"setMonitoringData":[monitor(-5, "Delta", 3, None)]}),
        ),
        (
            "duplicate-monitor-ids",
            "SetVariableMonitoring",
            json!({"setMonitoringData":[monitor(80, "UpperThreshold", 3, Some(4010)),
                monitor(70, "LowerThreshold", 3, Some(4010))]}),
        ),
        (
            "duplicate-clear-ids",
            "ClearVariableMonitoring",
            json!({"id":[4010, 4010]}),
        ),
        ("empty-clear", "ClearVariableMonitoring", json!({"id":[]})),
        (
            "unknown-base",
            "SetMonitoringBase",
            json!({"monitoringBase":"Everything"}),
        ),
        (
            "level-out-of-range",
            "SetMonitoringLevel",
            json!({"severity":-1}),
        ),
        (
            "opaque-vendor-data",
            "SetMonitoringLevel",
            json!({"severity":3,"customData":{"vendorId":"SECRET-VENDOR"}}),
        ),
    ];
    for (id, action, payload) in cases {
        let (status, body) = refused(&client, &fixture, command(id, action, payload)).await;
        assert!((400..500).contains(&status), "{id}: {status}");
        assert!(!body.to_string().contains("SECRET-VENDOR"), "{id}");
    }
    // A scoped target is wrong: monitors belong to the station's own device model.
    let mut evse = command("evse-scope", "SetMonitoringLevel", json!({"severity":3}));
    evse["resource"]["resource"] = json!({"kind":"evse","evse_id":"one"});
    evse["resource"]["native_protocol_reference"] = json!({"protocol":"ocpp201","evse_id":1});
    let (status, _) = refused(&client, &fixture, evse).await;
    assert!((400..500).contains(&status));
    // The control grant may not send privileged native actions.
    let denied = client
        .post(fixture.url("/api/v1/commands"))
        .bearer_auth(CONTROL)
        .json(&command(
            "unprivileged",
            "SetMonitoringLevel",
            json!({"severity":3}),
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(denied.status(), 403);
    no_call(&mut socket).await;
}

#[tokio::test]
async fn more_monitors_than_the_station_accepts_are_not_transmitted() {
    let (fixture, _process) = launch(ALL_OPTIONS).await;
    let client = client();
    let mut socket = connected(&fixture, &client).await;
    // The station never reported its message limits, so only a single monitor may be sent.
    let two = command(
        "unlearned-limits",
        "SetVariableMonitoring",
        json!({"setMonitoringData":[monitor(80, "UpperThreshold", 3, None),
            monitor(10, "LowerThreshold", 3, None)]}),
    );
    let response = client
        .post(fixture.url("/api/v1/commands"))
        .bearer_auth(PRIVILEGED)
        .json(&two)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 400);
    let body: Value = response.json().await.unwrap();
    assert_eq!(body["lifecycle"]["stage"], "rejected");
    assert_eq!(body["lifecycle"]["error"]["code"], "policy_rejected");
    no_call(&mut socket).await;
}

#[tokio::test]
async fn a_reply_that_does_not_match_the_request_is_uncertain_not_success() {
    let (fixture, _process) = launch(ALL_OPTIONS).await;
    let client = client();
    let mut socket = connected(&fixture, &client).await;
    learn_limits(&client, &fixture, &mut socket).await;

    let (_, mismatched) = answered(
        &client,
        &fixture,
        &mut socket,
        command(
            "wrong-count",
            "SetVariableMonitoring",
            json!({"setMonitoringData":[monitor(80, "UpperThreshold", 3, None),
                monitor(10, "LowerThreshold", 3, None)]}),
        ),
        json!({"setMonitoringResult":[{"id":4010,"status":"Accepted","type":"UpperThreshold",
            "severity":3,"component":{"name":"Connector","evse":{"id":1,"connectorId":1}},
            "variable":{"name":"Temperature"}}]}),
    )
    .await;
    assert_eq!(mismatched["lifecycle"]["stage"], "transmission_uncertain");
    assert_ne!(mismatched["lifecycle"]["accepted"], true);
    assert!(mismatched["monitoring_201"].is_null());

    // A native CALLERROR is a protocol rejection, never an accepted monitor.
    let submission = begin(
        &client,
        &fixture,
        command("call-error", "SetMonitoringLevel", json!({"severity":3})),
    );
    let call = receive(&mut socket).await;
    send(&mut socket, json!([4, call[1], "NotImplemented", "", {}])).await;
    let rejected = completed(submission).await;
    assert_eq!(rejected["lifecycle"]["accepted"], false);
    assert_eq!(rejected["lifecycle"]["error"]["code"], "protocol_rejected");
}
