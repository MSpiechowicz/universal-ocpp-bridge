//! Station-side transfer bounds, exercised against a local stub server.
use std::{
    net::SocketAddr,
    sync::{Arc, Mutex},
    time::Duration,
};

use axum::{
    Router,
    body::{Body, Bytes},
    extract::{Path, State},
    http::{HeaderMap, Method, StatusCode, header},
    routing::{any, get},
};
use sha2::{Digest, Sha256};
use uob_sim::artifact_transfer::{TransferFailure, TransferPolicy, download, upload};

type Received = Arc<Mutex<Vec<(Method, String, Option<String>, Vec<u8>)>>>;

const FIRMWARE: &[u8] = b"independent firmware image";

async fn stub() -> (SocketAddr, Received) {
    let received = Received::default();
    let router = Router::new()
        .route("/firmware", get(|| async { FIRMWARE }))
        .route("/missing", get(|| async { StatusCode::NOT_FOUND }))
        .route("/large", get(|| async { vec![7_u8; 4096] }))
        .route(
            "/stream",
            get(|| async {
                let chunks = (0..8).map(|_| Ok::<_, std::io::Error>(Bytes::from(vec![1_u8; 512])));
                Body::from_stream(futures::stream::iter(chunks))
            }),
        )
        .route(
            "/slow",
            get(|| async {
                tokio::time::sleep(Duration::from_secs(5)).await;
                FIRMWARE
            }),
        )
        .route("/logs/{file}", any(record))
        .with_state(received.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    (address, received)
}

async fn record(
    State(received): State<Received>,
    method: Method,
    Path(file): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> StatusCode {
    let content_type = headers
        .get(header::CONTENT_TYPE)
        .map(|value| value.to_str().unwrap().to_owned());
    received
        .lock()
        .unwrap()
        .push((method, file, content_type, body.to_vec()));
    StatusCode::CREATED
}

fn policy(maximum_bytes: u64) -> TransferPolicy {
    TransferPolicy {
        maximum_bytes,
        timeout: Duration::from_secs(2),
    }
}

#[tokio::test]
async fn download_returns_exact_bytes_and_digest_within_the_cap() {
    let (address, _) = stub().await;
    let artifact = download(&format!("http://{address}/firmware"), policy(1024))
        .await
        .unwrap();
    assert_eq!(artifact.bytes, FIRMWARE);
    assert_eq!(artifact.sha256, <[u8; 32]>::from(Sha256::digest(FIRMWARE)));
    assert_eq!(
        download(&format!("http://{address}/missing"), policy(1024)).await,
        Err(TransferFailure::Status(404))
    );
}

#[tokio::test]
async fn download_refuses_declared_and_streamed_bodies_over_the_cap() {
    let (address, _) = stub().await;
    for path in ["large", "stream"] {
        assert_eq!(
            download(&format!("http://{address}/{path}"), policy(1024)).await,
            Err(TransferFailure::TooLarge),
            "{path}"
        );
    }
    assert_eq!(
        download(&format!("http://{address}/stream"), policy(4096))
            .await
            .unwrap()
            .bytes
            .len(),
        4096
    );
}

#[tokio::test]
async fn download_is_bounded_by_the_deadline_and_reports_unreachable_peers() {
    let (address, _) = stub().await;
    let short = TransferPolicy {
        maximum_bytes: 1024,
        timeout: Duration::from_millis(200),
    };
    assert_eq!(
        download(&format!("http://{address}/slow"), short).await,
        Err(TransferFailure::TimedOut)
    );
    let closed = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let closed_address = closed.local_addr().unwrap();
    drop(closed);
    assert_eq!(
        download(&format!("http://{closed_address}/firmware"), policy(1024)).await,
        Err(TransferFailure::Unreachable)
    );
}

#[tokio::test]
async fn upload_puts_raw_bytes_and_appends_the_file_name_to_directory_locations() {
    let (address, received) = stub().await;
    upload(
        &format!("http://{address}/logs/"),
        "diagnostics-1.log",
        b"log line".to_vec(),
        policy(1024),
    )
    .await
    .unwrap();
    upload(
        &format!("http://{address}/logs/exact.log"),
        "ignored.log",
        b"exact".to_vec(),
        policy(1024),
    )
    .await
    .unwrap();
    let received = received.lock().unwrap().clone();
    assert_eq!(
        received,
        [
            (
                Method::PUT,
                "diagnostics-1.log".to_owned(),
                Some("application/octet-stream".to_owned()),
                b"log line".to_vec()
            ),
            (
                Method::PUT,
                "exact.log".to_owned(),
                Some("application/octet-stream".to_owned()),
                b"exact".to_vec()
            ),
        ]
    );
}

#[tokio::test]
async fn transfers_refuse_unsafe_locations_names_and_oversized_uploads_before_connecting() {
    let (address, received) = stub().await;
    for location in [
        "ftp://example.test/firmware",
        "file:///var/lib/firmware",
        "not a url",
        "http://user:secret@127.0.0.1/firmware",
    ] {
        assert_eq!(
            download(location, policy(1024)).await,
            Err(TransferFailure::InvalidLocation),
            "{location}"
        );
    }
    let directory = format!("http://{address}/logs/");
    for name in ["", ".hidden", "../escape", "a/b"] {
        assert_eq!(
            upload(&directory, name, b"x".to_vec(), policy(1024)).await,
            Err(TransferFailure::InvalidLocation),
            "{name:?}"
        );
    }
    assert_eq!(
        upload(&directory, "big.log", vec![0; 2048], policy(1024)).await,
        Err(TransferFailure::TooLarge)
    );
    assert!(received.lock().unwrap().is_empty());
}
