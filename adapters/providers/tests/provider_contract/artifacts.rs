use std::time::Duration;

use uob_application::artifact_provider::{
    ArtifactKind, ArtifactProvider, ArtifactProviderError, ArtifactReference, UploadRequest,
    UploadStatus,
};
use uob_provider_adapter::{TestProviderError, test_artifacts::TestArtifactService};

use super::{
    Spool, authority, configuration,
    contract::{
        opens_bounded_upload_destinations, resolves_published_firmware,
        signed_firmware_verifies_against_the_manufacturer_root,
    },
    demo, firmware, transfers,
};

fn service(spool: &Spool, maximum_artifact_bytes: u64) -> TestArtifactService {
    TestArtifactService::new(
        demo(),
        transfers(maximum_artifact_bytes, Duration::from_secs(5)),
        configuration("http://artifacts.test:8080", spool),
    )
    .unwrap()
}

fn reference(value: &str) -> ArtifactReference {
    ArtifactReference::new(value).unwrap()
}

#[tokio::test]
async fn test_artifact_service_passes_the_artifact_contract() {
    let spool = Spool::new();
    let artifacts = service(&spool, 64 * 1024);
    let pki = authority();
    let image = firmware(4096);
    artifacts
        .publish_firmware(reference("plain"), image.clone())
        .unwrap();
    artifacts
        .publish_signed_firmware(reference("signed"), image.clone(), &pki)
        .unwrap();
    let plain = resolves_published_firmware(&artifacts, &reference("plain"), &image).await;
    assert_eq!(plain.kind(), ArtifactKind::Firmware);
    assert_eq!(
        plain.location().as_str(),
        "http://artifacts.test:8080/artifacts/plain"
    );
    resolves_published_firmware(&artifacts, &reference("signed"), &image).await;
    signed_firmware_verifies_against_the_manufacturer_root(
        &artifacts,
        &pki,
        &reference("signed"),
        &image,
    )
    .await;
    let destination = opens_bounded_upload_destinations(&artifacts, 1 << 40).await;
    assert_eq!(
        destination.maximum_bytes,
        64 * 1024,
        "lowered to the transfer cap"
    );
    assert!(
        destination
            .location
            .as_str()
            .starts_with("http://artifacts.test:8080/uploads/")
    );
}

#[test]
fn publishing_is_bounded_by_the_transfer_cap_and_catalog_size() {
    let spool = Spool::new();
    let artifacts = service(&spool, 1024);
    assert_eq!(
        artifacts.publish_firmware(reference("empty"), Vec::new()),
        Err(TestProviderError::InvalidConfiguration)
    );
    assert_eq!(
        artifacts.publish_firmware(reference("large"), firmware(1025)),
        Err(TestProviderError::InvalidConfiguration)
    );
    for index in 0..4 {
        artifacts
            .publish_firmware(reference(&format!("fw-{index}")), firmware(16))
            .unwrap();
    }
    assert_eq!(
        artifacts.publish_firmware(reference("fw-4"), firmware(16)),
        Err(TestProviderError::Capacity)
    );
    let replaced = artifacts
        .publish_firmware(reference("fw-0"), firmware(32))
        .unwrap();
    assert_eq!(replaced.integrity().size_bytes, 32);
}

#[test]
fn invalid_service_configuration_is_refused() {
    let spool = Spool::new();
    for base in [
        "ftp://artifacts.test",
        "http://artifacts.test/",
        "http://user:secret@artifacts.test",
        "http://artifacts.test?token=1",
        "/var/lib/uob/artifacts",
    ] {
        assert_eq!(
            TestArtifactService::new(
                demo(),
                transfers(1024, Duration::from_secs(1)),
                configuration(base, &spool),
            )
            .err(),
            Some(TestProviderError::InvalidConfiguration),
            "{base}"
        );
    }
    let mut zero = configuration("http://artifacts.test", &spool);
    zero.maximum_uploads = 0;
    assert!(
        TestArtifactService::new(demo(), transfers(1024, Duration::from_secs(1)), zero).is_err()
    );
}

#[tokio::test]
async fn full_upload_destinations_evict_the_oldest_idle_one() {
    let spool = Spool::new();
    let artifacts = service(&spool, 1024);
    let request = UploadRequest {
        kind: ArtifactKind::DiagnosticsLog,
        maximum_bytes: 512,
    };
    let mut opened = Vec::new();
    for _ in 0..5 {
        opened.push(artifacts.open_upload(request).await.unwrap().upload_id);
    }
    assert_eq!(
        artifacts.upload_status(&opened[0]).await,
        Err(ArtifactProviderError::UnknownUpload)
    );
    for upload in &opened[1..] {
        assert_eq!(
            artifacts.upload_status(upload).await,
            Ok(UploadStatus::Pending)
        );
    }
}

#[tokio::test(start_paused = true)]
async fn artifact_faults_deny_and_delay_port_operations() {
    let spool = Spool::new();
    let artifacts = service(&spool, 1024);
    artifacts
        .publish_firmware(reference("fw"), firmware(16))
        .unwrap();
    let request = UploadRequest {
        kind: ArtifactKind::SecurityLog,
        maximum_bytes: 512,
    };
    let destination = artifacts.open_upload(request).await.unwrap();
    artifacts.faults().set_unavailable(true);
    assert_eq!(
        artifacts.resolve(&reference("fw")).await,
        Err(ArtifactProviderError::Unavailable)
    );
    assert_eq!(
        artifacts.open_upload(request).await,
        Err(ArtifactProviderError::Unavailable)
    );
    assert_eq!(
        artifacts.upload_status(&destination.upload_id).await,
        Err(ArtifactProviderError::Unavailable)
    );
    artifacts.faults().set_unavailable(false);
    artifacts.faults().set_delay(Duration::from_secs(20));
    let started = tokio::time::Instant::now();
    assert!(artifacts.resolve(&reference("fw")).await.is_ok());
    assert!(started.elapsed() >= Duration::from_secs(20));
    assert!(
        tokio::time::timeout(Duration::from_secs(19), artifacts.open_upload(request))
            .await
            .is_err()
    );
}
