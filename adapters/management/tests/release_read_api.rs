use std::{
    fs,
    io::{BufRead, BufReader, Write},
    os::unix::net::UnixListener,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    thread,
};

use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use tower::ServiceExt;
use uob_management_adapter::{
    ManagementReleaseReadAuthenticator, ManagementReleaseReadConfiguration, release_read_router,
};

static NEXT_PATH: AtomicU64 = AtomicU64::new(1);

struct Directory(PathBuf);
impl Drop for Directory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

struct Token;
impl ManagementReleaseReadAuthenticator for Token {
    fn authenticate(&self, token: &str) -> bool {
        token == "release-read-token"
    }
}

fn fixture(response: Vec<u8>) -> (axum::Router, thread::JoinHandle<String>, Directory) {
    let directory = Directory(std::env::temp_dir().join(format!(
        "uob-management-release-read-{}-{}",
        std::process::id(),
        NEXT_PATH.fetch_add(1, Ordering::Relaxed)
    )));
    fs::create_dir(&directory.0).unwrap();
    let socket = directory.0.join("control.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let peer = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut request = String::new();
        BufReader::new(&mut stream).read_line(&mut request).unwrap();
        let _ = stream.write_all(&response);
        request
    });
    let router = release_read_router(ManagementReleaseReadConfiguration {
        supervisor_socket: socket,
        authenticator: Arc::new(Token),
    });
    (router, peer, directory)
}

fn no_peer_router() -> (axum::Router, Directory) {
    let directory = Directory(std::env::temp_dir().join(format!(
        "uob-management-release-read-{}-{}",
        std::process::id(),
        NEXT_PATH.fetch_add(1, Ordering::Relaxed)
    )));
    fs::create_dir(&directory.0).unwrap();
    let router = release_read_router(ManagementReleaseReadConfiguration {
        supervisor_socket: directory.0.join("control.sock"),
        authenticator: Arc::new(Token),
    });
    (router, directory)
}

fn authorized(uri: &str) -> Request<Body> {
    Request::builder()
        .uri(uri)
        .header("authorization", "Bearer release-read-token")
        .body(Body::empty())
        .unwrap()
}

#[tokio::test]
async fn missing_or_wrong_credentials_cannot_open_the_supervisor_socket() {
    let (router, _directory) = no_peer_router();
    let missing = router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/v1/release/status")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let wrong = router
        .oneshot(
            Request::builder()
                .uri("/api/v1/release/status")
                .header("authorization", "Bearer wrong")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(missing.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(wrong.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn malformed_or_oversized_supervisor_frames_are_sanitized() {
    for response in [
        br#"{"protocol":99,"manager_version":"peer-secret","code":"ok"}
"#
        .to_vec(),
        {
            let mut bytes = b"{\"protocol\":1,".to_vec();
            bytes.resize(80 * 1024 + 1, b'x');
            bytes.push(b'\n');
            bytes
        },
    ] {
        let (router, peer, _directory) = fixture(response);
        let response = router
            .oneshot(authorized("/api/v1/release/status"))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
        let body = to_bytes(response.into_body(), 4096).await.unwrap();
        assert!(!String::from_utf8_lossy(&body).contains("peer-secret"));
        let _ = peer.join();
    }
}

#[tokio::test]
async fn release_read_routes_have_no_mutation_methods() {
    let (router, _directory) = no_peer_router();
    let response = router
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/release/status")
                .header("authorization", "Bearer release-read-token")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
}

fn activation() -> serde_json::Value {
    serde_json::json!({
        "sequence": 42,
        "production": {
            "digest": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            "phase": "previous-good"
        },
        "previous_good": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
        "candidate": {
            "digest": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "phase": "quarantined"
        }
    })
}

fn status_with_activation(activation: &serde_json::Value) -> Vec<u8> {
    let mut response = serde_json::to_vec(&serde_json::json!({
        "protocol": 1,
        "manager_version": "0.28.0",
        "code": "ok",
        "status": {
            "rollback": null,
            "probation": null,
            "promotion": null,
            "failures": null,
            "sequence": 0,
            "failed_operations": 0,
            "staged_verified_digest": null,
            "last_operation": null,
            "qualification": null
        },
        "activation": activation
    }))
    .unwrap();
    response.push(b'\n');
    response
}

#[tokio::test]
async fn status_exposes_valid_live_activation_pointers() {
    let (router, peer, _directory) = fixture(status_with_activation(&activation()));
    let response = router
        .oneshot(authorized("/api/v1/release/status"))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let response: serde_json::Value =
        serde_json::from_slice(&to_bytes(response.into_body(), 4096).await.unwrap()).unwrap();
    assert_eq!(
        response["activation"]["production"]["phase"],
        serde_json::Value::String("previous-good".into())
    );
    assert_eq!(
        response["activation"]["candidate"]["phase"],
        serde_json::Value::String("quarantined".into())
    );
    assert_eq!(peer.join().unwrap(), "{\"operation\":\"status\"}\n");
}

#[tokio::test]
async fn status_without_activation_remains_compatible() {
    let mut legacy: serde_json::Value =
        serde_json::from_slice(&status_with_activation(&activation())).unwrap();
    legacy.as_object_mut().unwrap().remove("activation");
    let mut legacy = serde_json::to_vec(&legacy).unwrap();
    legacy.push(b'\n');
    let (router, peer, _directory) = fixture(legacy);
    let response = router
        .oneshot(authorized("/api/v1/release/status"))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let response: serde_json::Value =
        serde_json::from_slice(&to_bytes(response.into_body(), 4096).await.unwrap()).unwrap();
    assert!(response.get("activation").is_none());
    let _ = peer.join();
}

#[tokio::test]
async fn activation_is_rejected_for_invalid_pointers_events_and_errors() {
    let mut invalid_digest = activation();
    invalid_digest["candidate"]["digest"] = serde_json::json!("A");
    let mut invalid_phase = activation();
    invalid_phase["production"]["phase"] = serde_json::json!("current");
    let mut events = serde_json::to_vec(&serde_json::json!({
        "protocol": 1,
        "manager_version": "0.28.0",
        "code": "ok",
        "events": {
            "records": [],
            "oldest_sequence": 0,
            "latest_sequence": 0,
            "truncated": false
        },
        "activation": activation()
    }))
    .unwrap();
    events.push(b'\n');
    let mut forbidden = serde_json::to_vec(&serde_json::json!({
        "protocol": 1,
        "manager_version": "0.28.0",
        "code": "forbidden",
        "activation": activation()
    }))
    .unwrap();
    forbidden.push(b'\n');
    for (uri, response) in [
        (
            "/api/v1/release/status",
            status_with_activation(&invalid_digest),
        ),
        (
            "/api/v1/release/status",
            status_with_activation(&invalid_phase),
        ),
        ("/api/v1/release/events", events),
        ("/api/v1/release/status", forbidden),
    ] {
        let (router, peer, _directory) = fixture(response);
        let response = router.oneshot(authorized(uri)).await.unwrap();
        assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
        let _ = peer.join();
    }
}
