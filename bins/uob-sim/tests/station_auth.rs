#![allow(clippy::result_large_err)] // Tungstenite's handshake callback fixes the error type.

use std::fs;
use std::path::PathBuf;
use std::time::Duration;

use tokio::net::TcpListener;
use tokio::time::{sleep, timeout};
use tokio_tungstenite::tungstenite::handshake::server::{Request, Response};
use uob_sim::scenario::parse_configuration;
use uob_sim::{
    OcppVersion, ProtocolClient, SimulatorClientConfig, SimulatorProtocolClient, TraceKind,
};

const BOUND: Duration = Duration::from_secs(5);
const SECRET: &str = "demo-station-secret-12345";

struct SecretFile(PathBuf);

impl SecretFile {
    fn new(contents: &[u8]) -> Self {
        let path = std::env::temp_dir().join(format!("uob-sim-auth-{}", uuid::Uuid::new_v4()));
        fs::write(&path, contents).unwrap();
        Self(path)
    }

    fn path(&self) -> &str {
        self.0.to_str().unwrap()
    }
}

impl Drop for SecretFile {
    fn drop(&mut self) {
        fs::remove_file(&self.0).unwrap();
    }
}

fn station_config(
    address: std::net::SocketAddr,
    id: &str,
    version: OcppVersion,
    secret: &str,
    reconnect: bool,
) -> SimulatorClientConfig {
    let edition = match version {
        OcppVersion::V1_6 => "1.6",
        OcppVersion::V2_0_1 => "2.0.1",
    };
    let source = format!(
        "schema_version = 1\n[[stations]]\nid = '{id}'\nendpoint = 'ws://{address}/ocpp/{id}'\nocpp_version = '{edition}'\ncredentials_file = '{secret}'\nreconnect = {reconnect}\n"
    );
    parse_configuration(&source).unwrap().stations[0].client_config()
}

async fn accept_authenticated(
    listener: &TcpListener,
    id: &'static str,
    protocol: &'static str,
    expected_authorization: &'static str,
    allowed: bool,
) -> Option<tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>> {
    let (stream, _) = timeout(BOUND, listener.accept()).await.unwrap().unwrap();
    let response = tokio_tungstenite::accept_hdr_async(
        stream,
        move |request: &Request, mut response: Response| {
            assert_eq!(request.uri().path(), format!("/ocpp/{id}"));
            assert_eq!(
                request.headers()["Sec-WebSocket-Protocol"]
                    .to_str()
                    .unwrap(),
                protocol
            );
            let authorization = request
                .headers()
                .get("Authorization")
                .and_then(|value| value.to_str().ok());
            if authorization != Some(expected_authorization) {
                return Err(Response::builder()
                    .status(401)
                    .body(Some("Unauthorized".to_owned()))
                    .unwrap());
            }
            response
                .headers_mut()
                .insert("Sec-WebSocket-Protocol", protocol.parse().unwrap());
            Ok(response)
        },
    )
    .await;
    if allowed {
        Some(response.unwrap())
    } else {
        assert!(response.is_err());
        None
    }
}

#[tokio::test]
async fn both_editions_authenticate_initial_and_reconnect_handshakes() {
    for (version, id, expected) in [
        (
            OcppVersion::V1_6,
            "station-a",
            "Basic c3RhdGlvbi1hOmRlbW8tc3RhdGlvbi1zZWNyZXQtMTIzNDU=",
        ),
        (
            OcppVersion::V2_0_1,
            "station-b",
            "Basic c3RhdGlvbi1iOmRlbW8tc3RhdGlvbi1zZWNyZXQtMTIzNDUK",
        ),
    ] {
        let bytes: &[u8] = if version == OcppVersion::V2_0_1 {
            b"demo-station-secret-12345\n"
        } else {
            SECRET.as_bytes()
        };
        let secret = SecretFile::new(bytes);
        let (done, hold) = tokio::sync::oneshot::channel::<()>();
        let (start, ready) = tokio::sync::oneshot::channel::<()>();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let config = station_config(
            listener.local_addr().unwrap(),
            id,
            version,
            secret.path(),
            true,
        );
        let server = tokio::spawn(async move {
            let mut first =
                accept_authenticated(&listener, id, version.websocket_protocol(), expected, true)
                    .await
                    .unwrap();
            let _ = ready.await;
            first.close(None).await.unwrap();
            let second =
                accept_authenticated(&listener, id, version.websocket_protocol(), expected, true)
                    .await
                    .unwrap();
            let _ = hold.await;
            drop(second);
        });
        let client = timeout(BOUND, SimulatorProtocolClient::connect(config))
            .await
            .unwrap()
            .unwrap();
        let _ = start.send(());
        timeout(BOUND, async {
            loop {
                if client
                    .traces()
                    .iter()
                    .any(|event| event.kind == TraceKind::Reconnected)
                {
                    break;
                }
                sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        client.shutdown().await.unwrap();
        let _ = done.send(());
        server.await.unwrap();
    }
}

#[tokio::test]
async fn invalid_and_oversized_secret_files_fail_without_disclosure() {
    let missing = std::env::temp_dir().join(format!("uob-missing-{}", uuid::Uuid::new_v4()));
    let oversized = SecretFile::new(&vec![b'x'; 4097]);
    let short = SecretFile::new(b"too-short");
    let binary = SecretFile::new(b"demo-station-secret-1234\xff");
    let wrong = SecretFile::new(b"wrong-station-secret-12345");
    let target = SecretFile::new(SECRET.as_bytes());
    let link = SecretFile(
        std::env::temp_dir().join(format!("uob-sim-auth-link-{}", uuid::Uuid::new_v4())),
    );
    std::os::unix::fs::symlink(target.path(), &link.0).unwrap();

    for (version, id, expected) in [
        (
            OcppVersion::V1_6,
            "station-a",
            "Basic c3RhdGlvbi1hOmRlbW8tc3RhdGlvbi1zZWNyZXQtMTIzNDU=",
        ),
        (
            OcppVersion::V2_0_1,
            "station-b",
            "Basic c3RhdGlvbi1iOmRlbW8tc3RhdGlvbi1zZWNyZXQtMTIzNDU=",
        ),
    ] {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        for path in [
            missing.to_str().unwrap(),
            oversized.path(),
            short.path(),
            binary.path(),
            link.path(),
        ] {
            let config = station_config(address, id, version, path, false);
            let message = SimulatorProtocolClient::connect(config)
                .await
                .err()
                .unwrap()
                .to_string();
            assert!(message.contains("station credential unavailable"));
            assert!(!message.contains(path));
            assert!(!message.contains(SECRET));
        }
        assert!(
            timeout(Duration::from_millis(100), listener.accept())
                .await
                .is_err()
        );

        let server = tokio::spawn(async move {
            accept_authenticated(&listener, id, version.websocket_protocol(), expected, false)
                .await;
        });
        let config = station_config(address, id, version, wrong.path(), false);
        let message = SimulatorProtocolClient::connect(config)
            .await
            .err()
            .unwrap()
            .to_string();
        assert!(message.contains("station handshake failed"));
        assert!(!message.contains(wrong.path()));
        assert!(!message.contains("wrong-station-secret"));
        server.await.unwrap();
    }
}
