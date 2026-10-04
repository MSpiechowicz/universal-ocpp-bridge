#![cfg(unix)]
#[path = "local_authorization201/admission.rs"]
mod admission;
#[allow(dead_code)]
#[path = "composite_schedule/support.rs"]
mod host;
#[path = "local_authorization201/limits.rs"]
mod limits;
#[path = "local_authorization201/mixed.rs"]
mod mixed;
#[path = "local_authorization201/recovery.rs"]
mod recovery;
#[allow(dead_code)]
#[path = "local_authorization201/support.rs"]
mod support;
use serde_json::{Value, json};
use support::*;

#[tokio::test]
async fn actual_daemon_preserves_native_list_cache_and_query_evidence_and_secret_free_history() {
    let fixture = fixture();
    let mut process = fixture.start();
    fixture.ready(&mut process).await;
    let client = client();
    let mut socket = fixture.station_protocol("station-a", "ocpp2.0.1").await;
    boot(&mut socket).await;
    fixture.connected(&client, "station-a").await;
    for case in native_cases() {
        exercise_native_case(&client, &fixture, &mut socket, case).await;
    }
    let history = client
        .get(fixture.url("/api/v1/commands?station_id=station-a"))
        .bearer_auth(READ)
        .send()
        .await
        .unwrap();
    assert_eq!(history.status(), 200);
    assert_private(&fixture, &history.json::<Value>().await.unwrap());
}

type NativeCase = (&'static str, &'static str, usize, Value, bool);

fn native_cases() -> [NativeCase; 10] {
    [
        (
            "list-full",
            "SendLocalList",
            1,
            json!({"status":"Accepted"}),
            true,
        ),
        (
            "list-delete",
            "SendLocalList",
            2,
            json!({"status":"Accepted"}),
            true,
        ),
        (
            "list-failed",
            "SendLocalList",
            2,
            json!({"status":"Failed"}),
            false,
        ),
        (
            "list-version-mismatch",
            "SendLocalList",
            2,
            json!({"status":"VersionMismatch"}),
            false,
        ),
        (
            "list-full-omitted",
            "SendLocalList",
            3,
            json!({"status":"Accepted"}),
            true,
        ),
        (
            "list-empty-diff",
            "SendLocalList",
            4,
            json!({"status":"Accepted"}),
            true,
        ),
        (
            "query-empty",
            "GetLocalListVersion",
            0,
            json!({"versionNumber":0}),
            true,
        ),
        (
            "query-installed",
            "GetLocalListVersion",
            0,
            json!({"versionNumber":2}),
            true,
        ),
        (
            "cache-clear",
            "ClearCache",
            0,
            json!({"status":"Accepted"}),
            true,
        ),
        (
            "cache-rejected",
            "ClearCache",
            0,
            json!({"status":"Rejected"}),
            false,
        ),
    ]
}

async fn exercise_native_case(
    client: &reqwest::Client,
    fixture: &host::Fixture,
    socket: &mut host::Socket,
    case: NativeCase,
) {
    let (id, action, index, reply, accepted) = case;
    let body = command(id, action, index);
    let submission = begin(client, fixture, body.clone());
    let frame = receive(socket).await;
    assert_eq!(frame[1], id);
    assert_eq!(frame[2], action);
    assert_eq!(
        frame[3],
        if action == "SendLocalList" {
            native(index)
        } else {
            json!({})
        }
    );
    send(socket, json!([3, id, reply])).await;
    let response = submission.await.unwrap();
    assert_eq!(response.status(), 202);
    let result = response.json::<Value>().await.unwrap()["result"].clone();
    assert_eq!(result["schema_version"], json!({"major":1,"revision":10}));
    assert_eq!(result["lifecycle"]["accepted"], accepted);
    assert_eq!(result["local_authorization_201"]["action"], action);
    if action == "GetLocalListVersion" {
        assert_eq!(
            result["local_authorization_201"]["version_number"],
            reply["versionNumber"]
        );
    } else {
        assert_eq!(result["local_authorization_201"]["status"], reply["status"]);
    }
    assert_private(fixture, &result);
    assert_eq!(support::result(client, fixture, id).await, result);
    let duplicate = client
        .post(fixture.url("/api/v1/commands"))
        .bearer_auth(PRIVILEGED)
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(duplicate.status(), 202);
    no_call(socket).await;
    let mut conflicting = body;
    conflicting["expires_at"] = json!("2098-01-01T00:00:00Z");
    assert!(
        client
            .post(fixture.url("/api/v1/commands"))
            .bearer_auth(PRIVILEGED)
            .json(&conflicting)
            .send()
            .await
            .unwrap()
            .status()
            .is_client_error()
    );
    no_call(socket).await;
}
