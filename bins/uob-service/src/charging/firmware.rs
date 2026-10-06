//! Demo-only OCPP 1.6 firmware composition: one local test artifact service and one generated
//! test PKI. Every published image and every signature is test-only; production is refused by
//! both provider constructors and by the charging environment gate.
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, OpenOptions},
    io::{self, Write},
    os::unix::fs::OpenOptionsExt,
    path::Path,
    sync::Arc,
    time::Duration,
};

use serde::Deserialize;
use tokio::net::TcpListener;
use uob_application::{
    Application, RuntimeSecurityPolicy,
    artifact_provider::ArtifactReference,
    certificate_provider::{CertificatePem, CertificateProvider, TrustAnchorKind},
};
use uob_contracts::{Operation, ProtocolEdition, StationId, StationSnapshot, SupportedOperation};
use uob_protocol_adapter::v16::remote_control::{FirmwareSettings16, ReservationGrant16};
use uob_provider_adapter::{
    artifacts::{ArtifactTransfers, TransferLimits},
    test_artifacts::{TestArtifactConfiguration, TestArtifactService},
    test_ca::TestCertificateAuthority,
};

use super::{StationSettings, files};
use crate::configuration::charging::{StationFirmware, ValidatedFirmwareArtifacts};

const CATALOG_BYTES: usize = 64 * 1024;
const MAX_CATALOG_ENTRIES: usize = 16;
const MAX_IMAGE_BYTES: usize = 32 * 1024 * 1024;

/// Shared providers referenced by every firmware-enabled station.
pub(super) struct Providers {
    artifacts: TestArtifactService,
    certificates: TestCertificateAuthority,
    policy: RuntimeSecurityPolicy,
}

/// Listener reserved at startup; serving starts with the charging runtime.
pub(super) struct ArtifactServer {
    pub(super) providers: Arc<Providers>,
    pub(super) listener: TcpListener,
}

impl ArtifactServer {
    pub(super) async fn serve(
        providers: Arc<Providers>,
        listener: TcpListener,
        mut shutdown: tokio::sync::watch::Receiver<bool>,
    ) -> io::Result<()> {
        providers
            .artifacts
            .serve(listener, async move {
                let _ = shutdown.wait_for(|stopped| *stopped).await;
            })
            .await
    }
}

/// Per-socket policy for one firmware-enabled station.
pub(super) fn session(
    station: StationFirmware,
    providers: &Providers,
    grant: Arc<ReservationGrant16>,
) -> Arc<FirmwareSettings16> {
    Arc::new(FirmwareSettings16 {
        signed: station.signed,
        job_timeout: station.job_timeout,
        artifacts: Arc::new(providers.artifacts.clone()),
        certificates: Some(Arc::new(providers.certificates.clone())),
        policy: providers.policy,
        grant,
    })
}

/// Offers exactly the configured native family on the station root.
pub(super) fn apply_capabilities(
    snapshot: &mut StationSnapshot,
    firmware: Option<StationFirmware>,
) {
    let Some(firmware) = firmware else {
        return;
    };
    snapshot.capabilities.operations.push(SupportedOperation {
        operation: Operation::ProtocolAction {
            protocol: ProtocolEdition::Ocpp16j,
            action: if firmware.signed {
                "SignedUpdateFirmware"
            } else {
                "UpdateFirmware"
            }
            .to_owned(),
        },
        parameters: vec![],
    });
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Catalog {
    artifacts: Vec<CatalogEntry>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CatalogEntry {
    reference: String,
    file: String,
    signed: bool,
}

/// Loads the protected catalog, publishes every image and binds the station-facing listener.
pub(super) async fn install(
    config: Option<ValidatedFirmwareArtifacts>,
    application: &Application,
    seen: &mut BTreeSet<(u64, u64)>,
    stations: &mut BTreeMap<StationId, StationSettings>,
) -> io::Result<Option<ArtifactServer>> {
    let Some(config) = config else {
        return Ok(None);
    };
    let fail = |detail: &'static str| io::Error::other(detail);
    let policy = application.security_policy();
    files::directory(&config.spool_directory).map_err(fail)?;
    let catalog = files::protected(&config.catalog_file, CATALOG_BYTES, seen).map_err(fail)?;
    let catalog: Catalog =
        serde_json::from_slice(&catalog.0).map_err(|_| fail("invalid firmware catalog"))?;
    let references = catalog
        .artifacts
        .iter()
        .map(|entry| entry.reference.as_str())
        .collect::<BTreeSet<_>>();
    if catalog.artifacts.is_empty()
        || catalog.artifacts.len() > MAX_CATALOG_ENTRIES
        || references.len() != catalog.artifacts.len()
    {
        return Err(fail("invalid firmware catalog"));
    }
    let transfers = ArtifactTransfers::new(
        application.health().resources().clone(),
        TransferLimits {
            buffer_bytes: 64 * 1024,
            maximum_artifact_bytes: MAX_IMAGE_BYTES as u64,
            timeout: Duration::from_secs(300),
        },
    )
    .map_err(|_| fail("firmware transfer limits unavailable"))?;
    let artifacts = TestArtifactService::new(
        policy,
        transfers,
        TestArtifactConfiguration {
            public_base: config.public_base.clone(),
            spool_directory: config.spool_directory.clone(),
            maximum_artifacts: MAX_CATALOG_ENTRIES,
            maximum_uploads: 1,
        },
    )
    .map_err(|_| fail("firmware artifact service unavailable"))?;
    let certificates = TestCertificateAuthority::generate(policy, &config.organization)
        .map_err(|_| fail("firmware test PKI unavailable"))?;
    for entry in catalog.artifacts {
        let reference = ArtifactReference::new(entry.reference)
            .map_err(|_| fail("invalid firmware catalog reference"))?;
        let mut image = files::protected(Path::new(&entry.file), MAX_IMAGE_BYTES, seen)
            .map_err(|_| fail("invalid firmware catalog image"))?;
        let image = std::mem::take(&mut image.0);
        let published = if entry.signed {
            artifacts.publish_signed_firmware(reference, image, &certificates)
        } else {
            artifacts.publish_firmware(reference, image)
        };
        published.map_err(|_| fail("firmware catalog image refused"))?;
    }
    if let Some(output) = &config.manufacturer_root_file {
        let roots = certificates
            .trust_anchors(TrustAnchorKind::ManufacturerRoot)
            .await
            .map_err(|_| fail("firmware test PKI unavailable"))?;
        let pem = roots
            .certificates()
            .iter()
            .map(CertificatePem::as_str)
            .collect::<String>();
        write_public(output, pem.as_bytes())?;
    }
    let listener = TcpListener::bind(config.listen_addr).await?;
    let providers = Arc::new(Providers {
        artifacts,
        certificates,
        policy,
    });
    for station in stations
        .values_mut()
        .filter(|station| station.firmware.is_some())
    {
        station.firmware_providers = Some(providers.clone());
    }
    Ok(Some(ArtifactServer {
        providers,
        listener,
    }))
}

/// Replaces the published root atomically; it is public material, never a key.
fn write_public(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let temporary = path.with_extension("uob-tmp");
    let _ = fs::remove_file(&temporary);
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(files::NOFOLLOW)
        .open(&temporary)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    fs::rename(&temporary, path)
}
