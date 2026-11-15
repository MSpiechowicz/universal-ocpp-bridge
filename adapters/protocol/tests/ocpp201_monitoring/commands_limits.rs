use crate::harness::{Harness, corpus, evidence, exchange, learn_set, not_sent};
use serde_json::json;
use uob_contracts::*;

#[tokio::test]
async fn refused_requests_transmit_nothing() {
    let mut h = Harness::new().await;
    let evse = json!({"name": "EVSE", "evse": {"id": 1}});
    let monitor = |kind: &str, severity: i64| json!({"value": 5, "type": kind, "severity": severity, "component": evse, "variable": {"name": "Power"}});
    let refused = [
        (
            "SetVariableMonitoring",
            json!({"setMonitoringData": [monitor("Delta", 10)]}),
        ),
        (
            "SetVariableMonitoring",
            json!({"setMonitoringData": [{"value": -1, "type": "Delta", "severity": 5, "component": evse, "variable": {"name": "Power"}}]}),
        ),
        (
            "SetVariableMonitoring",
            json!({"setMonitoringData": [monitor("Delta", 5), monitor("Delta", 5)]}),
        ),
        (
            "SetVariableMonitoring",
            json!({"setMonitoringData": [{"value": 1, "type": "Delta", "severity": 5, "component": evse, "variable": {"name": "Power"}, "customData": {"vendorId": "v"}}]}),
        ),
        ("ClearVariableMonitoring", json!({"id": [1, 1]})),
        ("SetMonitoringLevel", json!({"severity": 10})),
        ("SetMonitoringBase", json!({"monitoringBase": "None"})),
        (
            "GetMonitoringReport",
            json!({"requestId": 2_147_483_648_i64}),
        ),
    ];
    for (action, payload) in refused {
        let id = h.id();
        let outcome = h.dispatch(&id, action, payload.clone()).await.unwrap();
        assert!(
            not_sent(&outcome, CommandErrorCode::InvalidParameters),
            "{action} {payload}: {outcome:?}"
        );
    }
    h.no_extra_call().await;
    h.close().await;
}

#[tokio::test]
async fn station_message_limits_gate_every_multi_item_request() {
    let mut h = Harness::new().await;
    let id = h.id();
    // Nothing learned yet: one item is allowed, more are not (N04.FR.09, N06.FR.04).
    let outcome = h
        .dispatch(&id, "SetVariableMonitoring", corpus("set-all-types"))
        .await
        .unwrap();
    assert!(
        not_sent(&outcome, CommandErrorCode::PolicyRejected),
        "{outcome:?}"
    );
    let id = h.id();
    let outcome = h
        .dispatch(&id, "ClearVariableMonitoring", corpus("clear-multi"))
        .await
        .unwrap();
    assert!(
        not_sent(&outcome, CommandErrorCode::PolicyRejected),
        "{outcome:?}"
    );
    // GetMonitoringReport selectors share the learned GetReport limit (N02.FR.07).
    let id = h.id();
    let outcome = h
        .dispatch(
            &id,
            "GetMonitoringReport",
            corpus("get-report-component-variable"),
        )
        .await
        .unwrap();
    assert!(
        not_sent(&outcome, CommandErrorCode::PolicyRejected),
        "{outcome:?}"
    );
    h.no_extra_call().await;

    // A single item needs no learned limit.
    let single = evidence(
        exchange(
            &mut h,
            "SetVariableMonitoring",
            corpus("set-minimal"),
            corpus("set-reply-minimal"),
        )
        .await,
    );
    assert!(single.accepted());

    // Learned limits are exact: item count and encoded bytes.
    learn_set(&mut h, "4", "100000").await;
    let id = h.id();
    let outcome = h
        .dispatch(&id, "SetVariableMonitoring", corpus("set-all-types"))
        .await
        .unwrap();
    assert!(
        not_sent(&outcome, CommandErrorCode::PolicyRejected),
        "{outcome:?}"
    );
    h.learn("SetVariableMonitoring", "ItemsPerMessage", "5")
        .await;
    let result = evidence(
        exchange(
            &mut h,
            "SetVariableMonitoring",
            corpus("set-all-types"),
            corpus("set-reply-accepted"),
        )
        .await,
    );
    assert!(result.accepted());
    h.learn("SetVariableMonitoring", "BytesPerMessage", "300")
        .await;
    let id = h.id();
    let outcome = h
        .dispatch(&id, "SetVariableMonitoring", corpus("set-all-types"))
        .await
        .unwrap();
    assert!(
        not_sent(&outcome, CommandErrorCode::PolicyRejected),
        "{outcome:?}"
    );
    // One monitor still fits 300 bytes, but not 100.
    let fits = evidence(
        exchange(
            &mut h,
            "SetVariableMonitoring",
            corpus("set-minimal"),
            corpus("set-reply-minimal"),
        )
        .await,
    );
    assert!(fits.accepted());
    h.learn("SetVariableMonitoring", "BytesPerMessage", "100")
        .await;
    let id = h.id();
    let outcome = h
        .dispatch(&id, "SetVariableMonitoring", corpus("set-minimal"))
        .await
        .unwrap();
    assert!(
        not_sent(&outcome, CommandErrorCode::PolicyRejected),
        "{outcome:?}"
    );
    h.no_extra_call().await;
    h.close().await;
}
