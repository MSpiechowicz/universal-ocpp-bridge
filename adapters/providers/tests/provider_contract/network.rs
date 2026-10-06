//! The independent simulator transfers artifacts over real sockets; it imports no service code.
use std::{net::SocketAddr, time::Duration};

use sha2::{Digest, Sha256};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
};
use uob_application::{
    artifact_provider::{
        ArtifactKind, ArtifactProvider, ArtifactProviderError, ArtifactReference,
        UploadDestination, UploadRefusal, UploadRequest, UploadStatus,
    },
    certificate_provider::{CertificateProvider, TrustAnchorKind},
};
use uob_sim::artifact_transfer::{TransferFailure, TransferPolicy, download, upload};

use super::{Served, authority, contract::station_accepts_firmware, firmware, serve};

const STATION: TransferPolicy = TransferPolicy {
    maximum_bytes: 1024 * 1024,
    timeout: Duration::from_secs(10),
};

async fn destination(served: &Served, maximum_bytes: u64) -> UploadDestination {
    let request = UploadRequest {
        kind: ArtifactKind::DiagnosticsLog,
        maximum_bytes,
    };
    served.service.open_upload(request).await.unwrap()
}

/// Sends a raw request head and possibly incomplete body, keeps the socket open, and returns
/// the response status line.
async fn raw_status(address: SocketAddr, head: &str, body: &[u8]) -> String {
    let mut stream = TcpStream::connect(address).await.unwrap();
    stream.write_all(head.as_bytes()).await.unwrap();
    stream.write_all(body).await.unwrap();
    let mut response = Vec::new();
    let mut buffer = [0; 1024];
    while !response.windows(4).any(|window| window == b"\r\n\r\n") {
        let count = stream.read(&mut buffer).await.unwrap();
        assert_ne!(count, 0, "connection closed before a response");
        response.extend_from_slice(&buffer[..count]);
    }
    let response = String::from_utf8(response).unwrap();
    assert!(
        response.to_ascii_lowercase().contains("content-length: 0"),
        "{response}"
    );
    response.lines().next().unwrap().to_owned()
}

#[tokio::test]
async fn simulator_downloads_signed_firmware_and_uploads_diagnostics_over_the_network() {
    let served = serve(256 * 1024, Duration::from_secs(10)).await;
    let pki = authority();
    let image = firmware(200 * 1024);
    let reference = ArtifactReference::new("station-firmware").unwrap();
    served
        .service
        .publish_signed_firmware(reference.clone(), image.clone(), &pki)
        .unwrap();
    let descriptor = served.service.resolve(&reference).await.unwrap();
    let downloaded = download(descriptor.location().as_str(), STATION)
        .await
        .unwrap();
    assert_eq!(downloaded.bytes, image);
    assert_eq!(&downloaded.sha256, descriptor.integrity().sha256.as_bytes());
    let roots = pki
        .trust_anchors(TrustAnchorKind::ManufacturerRoot)
        .await
        .unwrap();
    let signature = descriptor.integrity().signature.as_ref().unwrap();
    assert!(station_accepts_firmware(
        &roots,
        signature,
        &downloaded.bytes
    ));

    let log = firmware(48 * 1024);
    let opened = destination(&served, 64 * 1024).await;
    upload(
        opened.location.as_str(),
        "diagnostics.log",
        log.clone(),
        STATION,
    )
    .await
    .unwrap();
    assert_eq!(
        served.service.upload_status(&opened.upload_id).await,
        Ok(UploadStatus::Received {
            size_bytes: log.len() as u64,
            sha256: uob_application::artifact_provider::ArtifactSha256::from_bytes(
                Sha256::digest(&log).into()
            ),
        })
    );
    assert!(served.spool.is_empty(), "received uploads are unlinked");
    assert_eq!(
        upload(opened.location.as_str(), "again.log", log, STATION).await,
        Err(TransferFailure::Status(409))
    );
}

#[tokio::test]
async fn corrupt_and_unavailable_faults_reach_the_station() {
    let served = serve(64 * 1024, Duration::from_secs(10)).await;
    let reference = ArtifactReference::new("fw").unwrap();
    let descriptor = served
        .service
        .publish_firmware(reference.clone(), firmware(4096))
        .unwrap();
    served.service.faults().set_corrupt_downloads(true);
    let corrupted = download(descriptor.location().as_str(), STATION)
        .await
        .unwrap();
    assert_eq!(corrupted.bytes.len(), 4096);
    assert_ne!(&corrupted.sha256, descriptor.integrity().sha256.as_bytes());
    served.service.faults().set_corrupt_downloads(false);

    let opened = destination(&served, 1024).await;
    served.service.faults().set_unavailable(true);
    assert_eq!(
        download(descriptor.location().as_str(), STATION).await,
        Err(TransferFailure::Status(503))
    );
    assert_eq!(
        upload(opened.location.as_str(), "x.log", vec![1; 10], STATION).await,
        Err(TransferFailure::Status(503))
    );
    assert_eq!(
        served.service.resolve(&reference).await,
        Err(ArtifactProviderError::Unavailable)
    );
    served.service.faults().set_unavailable(false);
    assert_eq!(
        served.service.upload_status(&opened.upload_id).await,
        Ok(UploadStatus::Refused(UploadRefusal::Unavailable))
    );
    let missing = format!("http://{}/artifacts/missing", served.address);
    assert_eq!(
        download(&missing, STATION).await,
        Err(TransferFailure::Status(404))
    );
}

#[tokio::test]
async fn uploads_over_the_cap_are_refused_by_header_and_while_streaming() {
    let served = serve(64 * 1024, Duration::from_secs(10)).await;
    let opened = destination(&served, 4096).await;
    served.service.faults().set_upload_cap(Some(1024));
    assert_eq!(
        upload(opened.location.as_str(), "big.log", vec![1; 2048], STATION).await,
        Err(TransferFailure::Status(413))
    );
    assert_eq!(
        served.service.upload_status(&opened.upload_id).await,
        Ok(UploadStatus::Refused(UploadRefusal::TooLarge))
    );
    served.service.faults().set_upload_cap(Some(0));
    assert_eq!(
        upload(opened.location.as_str(), "no-cap.log", vec![1], STATION).await,
        Err(TransferFailure::Status(413))
    );
    served.service.faults().set_upload_cap(None);
    upload(
        opened.location.as_str(),
        "retry.log",
        vec![1; 2048],
        STATION,
    )
    .await
    .unwrap();
    assert!(matches!(
        served.service.upload_status(&opened.upload_id).await,
        Ok(UploadStatus::Received {
            size_bytes: 2048,
            ..
        })
    ));

    let chunked = destination(&served, 4096).await;
    let path = chunked
        .location
        .as_str()
        .split_once(&served.address.to_string())
        .unwrap()
        .1;
    let chunk = format!("{:x}\r\n{}\r\n", 1024, "a".repeat(1024));
    let head = format!(
        "POST {path}chunked.log HTTP/1.1\r\nHost: {}\r\nTransfer-Encoding: chunked\r\n\r\n",
        served.address
    );
    let status = raw_status(served.address, &head, chunk.repeat(5).as_bytes()).await;
    assert!(status.starts_with("HTTP/1.1 413"), "{status}");
    assert_eq!(
        served.service.upload_status(&chunked.upload_id).await,
        Ok(UploadStatus::Refused(UploadRefusal::TooLarge))
    );
}

#[tokio::test]
async fn stalled_uploads_time_out_and_unknown_destinations_are_not_found() {
    let served = serve(64 * 1024, Duration::from_millis(300)).await;
    let opened = destination(&served, 4096).await;
    let path = opened
        .location
        .as_str()
        .split_once(&served.address.to_string())
        .unwrap()
        .1;
    let head = format!(
        "PUT {path}stalled.log HTTP/1.1\r\nHost: {}\r\nContent-Length: 100\r\n\r\n",
        served.address
    );
    let status = raw_status(served.address, &head, &[1; 10]).await;
    assert!(status.starts_with("HTTP/1.1 408"), "{status}");
    assert_eq!(
        served.service.upload_status(&opened.upload_id).await,
        Ok(UploadStatus::Refused(UploadRefusal::TimedOut))
    );
    let head = format!(
        "PUT /uploads/unknown/x.log HTTP/1.1\r\nHost: {}\r\nContent-Length: 1\r\n\r\n",
        served.address
    );
    let status = raw_status(served.address, &head, b"x").await;
    assert!(status.starts_with("HTTP/1.1 404"), "{status}");
}
