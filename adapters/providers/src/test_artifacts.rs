//! Demo-only artifact service implementing the application artifact port over HTTP.
//!
//! Firmware is published in memory and streamed to stations through [`ArtifactTransfers`].
//! Station log uploads stream through the same bounded transfers into unlinked spool files.
//! Construction refuses production, and every descriptor and destination is test-only.

mod http;
mod uploads;

use std::{
    collections::BTreeMap,
    fmt,
    future::Future,
    path::PathBuf,
    sync::{Arc, Mutex, MutexGuard, PoisonError},
    time::Duration,
};

use sha2::{Digest, Sha256};
use tokio::net::TcpListener;
use uob_application::{
    RuntimeSecurityPolicy,
    artifact_provider::{
        ArtifactDescriptor, ArtifactIntegrity, ArtifactKind, ArtifactLocation, ArtifactProvider,
        ArtifactProviderDescriptor, ArtifactProviderError, ArtifactProviderFuture,
        ArtifactReference, ArtifactSha256, FirmwareSignature, UploadDestination, UploadId,
        UploadRefusal, UploadRequest, UploadStatus,
    },
};

use crate::{TestProviderError, artifacts::ArtifactTransfers, test_ca::TestCertificateAuthority};
use uploads::{BeginError, Completion, UploadSlots};

const DESCRIPTOR: ArtifactProviderDescriptor = ArtifactProviderDescriptor {
    kind: "test.local-artifacts",
    test_only: true,
};

/// Where the service is reachable and how much it retains.
#[derive(Clone, Debug)]
pub struct TestArtifactConfiguration {
    /// Station-reachable `http` or `https` base, such as `http://artifacts:8080`, without a
    /// trailing slash, query or credentials.
    pub public_base: String,
    /// Trusted, disk-budgeted spool directory for received uploads. It is never disclosed.
    pub spool_directory: PathBuf,
    /// Most published firmware artifacts.
    pub maximum_artifacts: usize,
    /// Most retained upload destinations; the oldest idle one is evicted when full.
    pub maximum_uploads: usize,
}

/// In-memory firmware catalog and bounded upload destinations served over HTTP.
#[derive(Clone)]
pub struct TestArtifactService {
    state: Arc<State>,
}

struct State {
    base: String,
    spool: PathBuf,
    transfers: ArtifactTransfers,
    maximum_artifacts: usize,
    catalog: Mutex<BTreeMap<ArtifactReference, Published>>,
    uploads: Mutex<UploadSlots>,
    faults: TestArtifactFaults,
}

struct Published {
    bytes: Arc<[u8]>,
    descriptor: ArtifactDescriptor,
}

impl TestArtifactService {
    /// Creates the service. Each download and upload uses `transfers`, whose byte cap bounds
    /// every published artifact and upload destination.
    ///
    /// # Errors
    ///
    /// Returns [`TestProviderError::Policy`] in production and `InvalidConfiguration` for an
    /// invalid base location or zero bound.
    pub fn new(
        policy: RuntimeSecurityPolicy,
        transfers: ArtifactTransfers,
        configuration: TestArtifactConfiguration,
    ) -> Result<Self, TestProviderError> {
        policy.authorize_artifact_provider(DESCRIPTOR)?;
        let base = configuration.public_base;
        let lowercase = base.to_ascii_lowercase();
        let longest = format!("{base}/uploads/{}/", "0".repeat(32));
        if !(lowercase.starts_with("http://") || lowercase.starts_with("https://"))
            || base.ends_with('/')
            || base.contains(['?', '#'])
            || ArtifactLocation::new(longest).is_err()
            || configuration.maximum_artifacts == 0
            || configuration.maximum_uploads == 0
        {
            return Err(TestProviderError::InvalidConfiguration);
        }
        Ok(Self {
            state: Arc::new(State {
                base,
                spool: configuration.spool_directory,
                transfers,
                maximum_artifacts: configuration.maximum_artifacts,
                catalog: Mutex::new(BTreeMap::new()),
                uploads: Mutex::new(UploadSlots::new(configuration.maximum_uploads)),
                faults: TestArtifactFaults::default(),
            }),
        })
    }

    /// Shared fault controls for denied, delayed and corrupt scenarios.
    #[must_use]
    pub fn faults(&self) -> &TestArtifactFaults {
        &self.state.faults
    }

    /// Publishes, or replaces, unsigned test firmware.
    ///
    /// # Errors
    ///
    /// Returns `InvalidConfiguration` for an empty or over-cap image and `Capacity` when the
    /// catalog is full.
    pub fn publish_firmware(
        &self,
        reference: ArtifactReference,
        firmware: Vec<u8>,
    ) -> Result<ArtifactDescriptor, TestProviderError> {
        self.publish(reference, firmware, None)
    }

    /// Publishes, or replaces, test firmware signed by the demo manufacturer signer.
    ///
    /// # Errors
    ///
    /// As [`Self::publish_firmware`], plus `Crypto` when signing fails.
    pub fn publish_signed_firmware(
        &self,
        reference: ArtifactReference,
        firmware: Vec<u8>,
        signer: &TestCertificateAuthority,
    ) -> Result<ArtifactDescriptor, TestProviderError> {
        self.check_size(&firmware)?;
        let signature = signer.sign_firmware(&firmware)?;
        self.publish(reference, firmware, Some(signature))
    }

    /// Serves downloads and uploads on `listener` until `shutdown` completes.
    ///
    /// # Errors
    ///
    /// Returns the listener's I/O error.
    pub async fn serve(
        &self,
        listener: TcpListener,
        shutdown: impl Future<Output = ()> + Send + 'static,
    ) -> std::io::Result<()> {
        axum::serve(listener, http::router(self.clone()))
            .with_graceful_shutdown(shutdown)
            .await
    }

    fn check_size(&self, firmware: &[u8]) -> Result<(), TestProviderError> {
        let maximum = self.state.transfers.limits().maximum_artifact_bytes;
        if firmware.is_empty() || u64::try_from(firmware.len()).is_ok_and(|size| size > maximum) {
            return Err(TestProviderError::InvalidConfiguration);
        }
        Ok(())
    }

    fn publish(
        &self,
        reference: ArtifactReference,
        firmware: Vec<u8>,
        signature: Option<FirmwareSignature>,
    ) -> Result<ArtifactDescriptor, TestProviderError> {
        self.check_size(&firmware)?;
        let invalid = |_| TestProviderError::InvalidConfiguration;
        let location = ArtifactLocation::new(format!(
            "{}/artifacts/{}",
            self.state.base,
            reference.as_str()
        ))
        .map_err(invalid)?;
        let kind = if signature.is_some() {
            ArtifactKind::SignedFirmware
        } else {
            ArtifactKind::Firmware
        };
        let integrity = ArtifactIntegrity {
            size_bytes: u64::try_from(firmware.len())
                .map_err(|_| TestProviderError::InvalidConfiguration)?,
            sha256: ArtifactSha256::from_bytes(Sha256::digest(&firmware).into()),
            signature,
        };
        let descriptor =
            ArtifactDescriptor::new(reference.clone(), kind, location, integrity, true)
                .map_err(invalid)?;
        let mut catalog = lock(&self.state.catalog);
        if !catalog.contains_key(&reference) && catalog.len() >= self.state.maximum_artifacts {
            return Err(TestProviderError::Capacity);
        }
        catalog.insert(
            reference,
            Published {
                bytes: firmware.into(),
                descriptor: descriptor.clone(),
            },
        );
        Ok(descriptor)
    }

    async fn admit_transfer(&self) -> Option<ArtifactFaults> {
        self.state.faults.admit().await.ok()
    }

    fn served_bytes(&self, reference: &ArtifactReference, corrupt: bool) -> Option<Arc<[u8]>> {
        let bytes = Arc::clone(&lock(&self.state.catalog).get(reference)?.bytes);
        if !corrupt {
            return Some(bytes);
        }
        let mut corrupted = bytes.to_vec();
        if let Some(last) = corrupted.last_mut() {
            *last ^= 0xff;
        }
        Some(corrupted.into())
    }

    fn begin_upload(&self, upload_id: &UploadId) -> Result<u64, BeginError> {
        lock(&self.state.uploads).begin(upload_id)
    }

    fn finish_upload(&self, upload_id: &UploadId, completion: Completion) {
        lock(&self.state.uploads).finish(upload_id, completion);
    }

    fn refuse_idle(&self, upload_id: &UploadId, refusal: UploadRefusal) {
        lock(&self.state.uploads).refuse_idle(upload_id, refusal);
    }
}

impl fmt::Debug for TestArtifactService {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TestArtifactService")
            .field("public_base", &self.state.base)
            .finish_non_exhaustive()
    }
}

impl ArtifactProvider for TestArtifactService {
    fn descriptor(&self) -> ArtifactProviderDescriptor {
        DESCRIPTOR
    }

    fn resolve<'a>(
        &'a self,
        reference: &'a ArtifactReference,
    ) -> ArtifactProviderFuture<'a, ArtifactDescriptor> {
        Box::pin(async move {
            self.state.faults.admit().await?;
            lock(&self.state.catalog)
                .get(reference)
                .map(|published| published.descriptor.clone())
                .ok_or(ArtifactProviderError::UnknownArtifact)
        })
    }

    fn open_upload(&self, request: UploadRequest) -> ArtifactProviderFuture<'_, UploadDestination> {
        Box::pin(async move {
            self.state.faults.admit().await?;
            if !request.kind.is_log() || request.maximum_bytes == 0 {
                return Err(ArtifactProviderError::InvalidRequest);
            }
            let maximum_bytes = request
                .maximum_bytes
                .min(self.state.transfers.limits().maximum_artifact_bytes);
            let upload_id = UploadId::new(uuid::Uuid::new_v4().simple().to_string())?;
            let location = ArtifactLocation::new(format!(
                "{}/uploads/{}/",
                self.state.base,
                upload_id.as_str()
            ))?;
            lock(&self.state.uploads).open(upload_id.clone(), maximum_bytes)?;
            Ok(UploadDestination {
                upload_id,
                kind: request.kind,
                location,
                maximum_bytes,
                test_only: true,
            })
        })
    }

    fn upload_status<'a>(
        &'a self,
        upload: &'a UploadId,
    ) -> ArtifactProviderFuture<'a, UploadStatus> {
        Box::pin(async move {
            self.state.faults.admit().await?;
            lock(&self.state.uploads)
                .status(upload)
                .ok_or(ArtifactProviderError::UnknownUpload)
        })
    }
}

/// Shared fault controls applied to port operations and HTTP transfers.
#[derive(Clone, Debug, Default)]
pub struct TestArtifactFaults {
    state: Arc<Mutex<ArtifactFaults>>,
}

#[derive(Clone, Copy, Debug, Default)]
struct ArtifactFaults {
    unavailable: bool,
    delay: Duration,
    corrupt_downloads: bool,
    upload_cap: Option<u64>,
}

impl TestArtifactFaults {
    /// Fails port operations as unavailable and HTTP transfers with 503.
    pub fn set_unavailable(&self, unavailable: bool) {
        self.update(|faults| faults.unavailable = unavailable);
    }

    /// Delays every port operation and HTTP transfer before it is admitted.
    pub fn set_delay(&self, delay: Duration) {
        self.update(|faults| faults.delay = delay);
    }

    /// Serves firmware whose bytes no longer match the advertised SHA-256 and signature.
    pub fn set_corrupt_downloads(&self, corrupt: bool) {
        self.update(|faults| faults.corrupt_downloads = corrupt);
    }

    /// Refuses uploads larger than `cap` bytes as too large, below any destination's own cap.
    pub fn set_upload_cap(&self, cap: Option<u64>) {
        self.update(|faults| faults.upload_cap = cap);
    }

    fn update(&self, change: impl FnOnce(&mut ArtifactFaults)) {
        change(&mut lock(&self.state));
    }

    async fn admit(&self) -> Result<ArtifactFaults, ArtifactProviderError> {
        let faults = *lock(&self.state);
        if !faults.delay.is_zero() {
            tokio::time::sleep(faults.delay).await;
        }
        if faults.unavailable {
            return Err(ArtifactProviderError::Unavailable);
        }
        Ok(faults)
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}
