//! Demo-only certificate authority implementing the application PKI port.
//!
//! Construction refuses production and generates every key in memory. Roots, the issuing CA
//! and every leaf carry "TEST ONLY" in their subject and are reported as test-only material.
//! No private key leaves this module, and every failure is sanitized.

mod chain;
mod csr;
mod firmware;
mod hierarchy;

use std::{
    fmt,
    sync::{Arc, Mutex, PoisonError},
    time::Duration,
};

use uob_application::{
    RuntimeSecurityPolicy,
    artifact_provider::FirmwareSignature,
    certificate_provider::{
        CertificateProvider, CertificateProviderDescriptor, CertificateProviderError,
        CertificateProviderFuture, CertificateSigningRequest, ChainVerification, CommonName,
        CsrDecision, CsrRejection, TrustAnchorKind, TrustAnchors, TrustDecision,
    },
};

use crate::test_provider::TestProviderError;
use firmware::FirmwareSigner;
use hierarchy::Hierarchy;

const DESCRIPTOR: CertificateProviderDescriptor = CertificateProviderDescriptor {
    kind: "test.local-ca",
    test_only: true,
};

/// In-memory demo CA with CSO, manufacturer and V2G roots.
///
/// The CSO root issues an issuing CA that signs station certificates. The manufacturer root
/// certifies an RSA-PSS firmware signer, and the V2G root signs V2G certificates directly.
#[derive(Clone)]
pub struct TestCertificateAuthority {
    state: Arc<State>,
}

struct State {
    organization: String,
    hierarchy: Hierarchy,
    firmware: FirmwareSigner,
    faults: TestCertificateFaults,
}

impl TestCertificateAuthority {
    /// Generates fresh demo hierarchies for a CSO organization name of 1 to 64 printable
    /// ASCII characters.
    ///
    /// # Errors
    ///
    /// Returns [`TestProviderError::Policy`] in production, `InvalidConfiguration` for an
    /// invalid organization name and `Crypto` when key generation fails.
    pub fn generate(
        policy: RuntimeSecurityPolicy,
        organization: &str,
    ) -> Result<Self, TestProviderError> {
        policy.authorize_certificate_provider(DESCRIPTOR)?;
        CommonName::new(organization).map_err(|_| TestProviderError::InvalidConfiguration)?;
        let hierarchy = Hierarchy::generate(organization)?;
        let firmware = FirmwareSigner::generate(&hierarchy.manufacturer_root, organization)?;
        Ok(Self {
            state: Arc::new(State {
                organization: organization.to_owned(),
                hierarchy,
                firmware,
                faults: TestCertificateFaults::default(),
            }),
        })
    }

    /// Shared fault controls for denied and delayed scenarios.
    #[must_use]
    pub fn faults(&self) -> &TestCertificateFaults {
        &self.state.faults
    }

    /// Signs a complete firmware image with the demo manufacturer signer.
    pub(crate) fn sign_firmware(
        &self,
        firmware: &[u8],
    ) -> Result<FirmwareSignature, TestProviderError> {
        self.state.firmware.sign(firmware)
    }
}

impl fmt::Debug for TestCertificateAuthority {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TestCertificateAuthority")
            .field("organization", &self.state.organization)
            .finish_non_exhaustive()
    }
}

impl CertificateProvider for TestCertificateAuthority {
    fn descriptor(&self) -> CertificateProviderDescriptor {
        DESCRIPTOR
    }

    fn sign_csr<'a>(
        &'a self,
        request: &'a CertificateSigningRequest,
    ) -> CertificateProviderFuture<'a, CsrDecision> {
        Box::pin(async move {
            if self.state.faults.admit().await?.reject_csr {
                return Ok(CsrDecision::Rejected(CsrRejection::PolicyRejected));
            }
            csr::sign(&self.state, request)
        })
    }

    fn trust_anchors(&self, kind: TrustAnchorKind) -> CertificateProviderFuture<'_, TrustAnchors> {
        Box::pin(async move {
            self.state.faults.admit().await?;
            let hierarchy = &self.state.hierarchy;
            let root = match kind {
                TrustAnchorKind::CsmsRoot => Some(&hierarchy.cso_root),
                TrustAnchorKind::ManufacturerRoot => Some(&hierarchy.manufacturer_root),
                TrustAnchorKind::V2gRoot => Some(&hierarchy.v2g_root),
                TrustAnchorKind::MoRoot => None,
            };
            TrustAnchors::new(
                root.map(|root| root.pem.clone()).into_iter().collect(),
                true,
            )
        })
    }

    fn verify_chain<'a>(
        &'a self,
        request: &'a ChainVerification,
    ) -> CertificateProviderFuture<'a, TrustDecision> {
        Box::pin(async move {
            self.state.faults.admit().await?;
            Ok(chain::verify(&self.state, request))
        })
    }
}

/// Shared fault controls applied before each PKI port operation.
#[derive(Clone, Debug, Default)]
pub struct TestCertificateFaults {
    state: Arc<Mutex<CertificateFaults>>,
}

#[derive(Clone, Copy, Debug, Default)]
struct CertificateFaults {
    unavailable: bool,
    delay: Duration,
    reject_csr: bool,
}

impl TestCertificateFaults {
    /// Fails every operation as unavailable.
    pub fn set_unavailable(&self, unavailable: bool) {
        self.update(|faults| faults.unavailable = unavailable);
    }

    /// Delays every operation before it is admitted.
    pub fn set_delay(&self, delay: Duration) {
        self.update(|faults| faults.delay = delay);
    }

    /// Refuses every CSR as a policy rejection.
    pub fn set_reject_csr(&self, reject: bool) {
        self.update(|faults| faults.reject_csr = reject);
    }

    fn update(&self, change: impl FnOnce(&mut CertificateFaults)) {
        change(&mut self.state.lock().unwrap_or_else(PoisonError::into_inner));
    }

    async fn admit(&self) -> Result<CertificateFaults, CertificateProviderError> {
        let faults = *self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if !faults.delay.is_zero() {
            tokio::time::sleep(faults.delay).await;
        }
        if faults.unavailable {
            return Err(CertificateProviderError::Unavailable);
        }
        Ok(faults)
    }
}
