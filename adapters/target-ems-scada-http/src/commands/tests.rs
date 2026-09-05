use std::{
    sync::{Arc, atomic::Ordering},
    time::Duration,
};

use axum::{body::Body, http::StatusCode};
use serde_json::{Value, json};
use uob_application::{
    CommandAdmissionError, CommandAdmissionErrorCode, CommandAdmissionFuture, CommandAdmissionPort,
    OperationalStore,
};
use uob_contracts::{CommandResult, ExternalCommand, RequestId};

mod parity;
mod support;
use support::{Harness, Store, payload, post, send};

#[tokio::test]
async fn admission_is_durable_deduplicated_and_reopens_with_protocol_response_only() {
    let harness = Harness::new();
    let request = payload("request /?#%é", "station-a");
    let (status, accepted) = post(harness.router(), "operator", request.clone()).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{accepted}");
    assert_eq!(
        accepted["result"]["lifecycle"]["stage"],
        "protocol_response"
    );
    assert!(
        serde_json::from_value::<CommandResult>(accepted["result"].clone())
            .unwrap()
            .observed_effects
            .is_empty()
    );
    let id = RequestId::new("request /?#%é").unwrap();
    let persisted = harness
        .store
        .command_by_request_id(id.clone())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        serde_json::to_value(&persisted.origin).unwrap(),
        json!({
            "kind": "target", "target_instance_id": "main", "principal_id": "operator"
        })
    );
    let (status, duplicate) = post(harness.router(), "operator", request).await;
    assert_eq!(status, StatusCode::ACCEPTED);
    assert_eq!(duplicate, accepted);
    assert_eq!(harness.stations.0.load(Ordering::SeqCst), 1);

    let reopened = Arc::new(Store::open(harness.path.join("store.sqlite"), 16).unwrap());
    assert!(reopened.command_by_request_id(id).await.unwrap().is_some());
    let router = support::router(
        reopened,
        harness.coordinator.clone(),
        Duration::from_secs(2),
    );
    let (status, result) = send(
        router,
        "GET",
        accepted["status_url"].as_str().unwrap(),
        "operator",
        Body::empty(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{result}");
    assert_eq!(result, accepted["result"]);

    let mut conflict = payload("request /?#%é", "station-a");
    conflict["operation"]["parameters"]["authorization_reference"] = json!("different");
    let (status, result) = post(harness.router(), "operator", conflict).await;
    assert_eq!(status, StatusCode::CONFLICT, "{result}");
    assert_eq!(result["error"], "ems_scada_http.request_conflict");
    assert_eq!(harness.stations.0.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn readers_and_out_of_scope_operators_cannot_submit_or_enumerate_status() {
    let harness = Harness::new();
    for token in ["reader", "bad-token"] {
        let expected = if token == "reader" {
            StatusCode::FORBIDDEN
        } else {
            StatusCode::UNAUTHORIZED
        };
        assert_eq!(
            post(harness.router(), token, payload("hidden", "station-a"))
                .await
                .0,
            expected
        );
        for id in ["exists", "missing"] {
            assert_eq!(
                send(
                    harness.router(),
                    "GET",
                    &format!("/bridge/v1/commands/{id}"),
                    token,
                    Body::empty()
                )
                .await
                .0,
                expected
            );
        }
    }
    assert_eq!(
        post(
            harness.router(),
            "station-operator",
            payload("bad-scope", "station-b")
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(harness.stations.0.load(Ordering::SeqCst), 0);
    assert_eq!(
        post(
            harness.router(),
            "operator",
            payload("station-b", "station-b")
        )
        .await
        .0,
        StatusCode::ACCEPTED
    );
    let (status, body) = send(
        harness.router(),
        "GET",
        "/bridge/v1/commands/station-b",
        "station-operator",
        Body::empty(),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body, json!({"error": "ems_scada_http.permission_denied"}));
    assert_eq!(
        post(
            harness.router(),
            "station-operator",
            payload("station-a", "station-a")
        )
        .await
        .0,
        StatusCode::ACCEPTED
    );
    assert_eq!(
        send(
            harness.router(),
            "GET",
            "/bridge/v1/commands/station-a",
            "station-operator",
            Body::empty()
        )
        .await
        .0,
        StatusCode::OK
    );
}

#[tokio::test]
async fn untrusted_origin_invalid_schema_and_privileged_actions_never_reach_admission() {
    let harness = Harness::new();
    for field in ["origin", "target_instance_id", "principal_id", "unexpected"] {
        let mut request = payload("spoof", "station-a");
        request[field] = json!("attacker");
        assert_eq!(
            post(harness.router(), "operator", request).await.0,
            StatusCode::BAD_REQUEST
        );
    }
    let mut privileged = payload("privileged", "station-a");
    privileged["operation"] = json!({"kind": "ocpp", "parameters": {
        "protocol": "ocpp16j", "action": "Reset", "payload_schema": "reset", "payload": {}
    }});
    let status = post(harness.router(), "operator", privileged).await.0;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(harness.stations.0.load(Ordering::SeqCst), 0);
    for id in [".", "..", &"a".repeat(257)] {
        assert_eq!(
            post(harness.router(), "operator", payload(id, "station-a"))
                .await
                .0,
            StatusCode::BAD_REQUEST
        );
    }
    let (status, _) = send(
        harness.router(),
        "POST",
        "/bridge/v1/commands",
        "operator",
        Body::from("x".repeat(65537)),
    )
    .await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
}

#[tokio::test]
async fn shared_application_rejections_never_return_accepted() {
    let harness = Harness::new();
    let mut expired = payload("expired", "station-a");
    expired["expires_at"] = json!("2000-01-01T00:00:00Z");
    assert_eq!(
        post(harness.router(), "operator", expired).await.0,
        StatusCode::GONE
    );
    assert_eq!(
        post(harness.router(), "operator", payload("offline", "offline"))
            .await
            .0,
        StatusCode::CONFLICT
    );
    let mut unsupported = payload("unsupported", "station-a");
    unsupported["operation"] = json!({"kind": "stop", "parameters": {"transaction_id": "tx-1"}});
    assert_eq!(
        post(harness.router(), "operator", unsupported).await.0,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    assert_eq!(harness.stations.0.load(Ordering::SeqCst), 0);
}

struct Failure(Option<CommandAdmissionErrorCode>);
impl CommandAdmissionPort<Value> for Failure {
    fn submit(&self, _: ExternalCommand<Value>) -> CommandAdmissionFuture<'_, CommandResult> {
        Box::pin(async move {
            match self.0 {
                Some(code) => Err(CommandAdmissionError::new(code, "private host detail")),
                None => std::future::pending().await,
            }
        })
    }
}

#[tokio::test]
async fn storage_failures_and_stalled_admission_are_bounded_and_sanitized() {
    let harness = Harness::new();
    for code in [
        CommandAdmissionErrorCode::StorageCapacityExhausted,
        CommandAdmissionErrorCode::Unavailable,
    ] {
        let (status, body) = post(
            harness.router_with(Arc::new(Failure(Some(code))), Duration::from_secs(1)),
            "operator",
            payload("fail", "station-a"),
        )
        .await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(body, json!({"error": "ems_scada_http.source_unavailable"}));
    }
    let router = harness.router_with(Arc::new(Failure(None)), Duration::from_millis(30));
    let first = post(router.clone(), "operator", payload("stall", "station-a"));
    let second = async {
        tokio::task::yield_now().await;
        post(router.clone(), "operator", payload("busy", "station-a")).await
    };
    let (first, second) = tokio::join!(first, second);
    assert_eq!(first.0, StatusCode::GATEWAY_TIMEOUT);
    assert_eq!(second.0, StatusCode::TOO_MANY_REQUESTS);
    // The timeout releases the command permit; the next request reaches its own deadline.
    assert_eq!(
        post(router, "operator", payload("retry", "station-a"))
            .await
            .0,
        StatusCode::GATEWAY_TIMEOUT
    );
}
