use aws_lc_rs::{
    rand::SystemRandom,
    rsa::{KeyPair, KeySize},
    signature::{KeyPair as _, RSA_PSS_SHA256},
};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use rcgen::{
    ExtendedKeyUsagePurpose, KeyUsagePurpose, PKCS_RSA_SHA256, PublicKeyData, SignatureAlgorithm,
};
use uob_application::{
    artifact_provider::{FirmwareSignature, FirmwareSignatureValue},
    certificate_provider::CertificatePem,
};

use super::hierarchy::{Ca, certificate_pem, crypto, leaf_params};
use crate::test_provider::TestProviderError;

/// Demo firmware signer under the manufacturer root.
///
/// OCPP 2.0.1 L01.FR.04 validates firmware with RSA-PSS and SHA-256 over the entire file, so
/// the signer uses a 3072-bit RSA key generated in memory.
pub(super) struct FirmwareSigner {
    key: KeyPair,
    certificate: CertificatePem,
}

struct RsaPublicKey(Vec<u8>);

impl PublicKeyData for RsaPublicKey {
    fn der_bytes(&self) -> &[u8] {
        &self.0
    }

    fn algorithm(&self) -> &'static SignatureAlgorithm {
        &PKCS_RSA_SHA256
    }
}

impl FirmwareSigner {
    pub(super) fn generate(
        manufacturer: &Ca,
        organization: &str,
    ) -> Result<Self, TestProviderError> {
        let key = KeyPair::generate(KeySize::Rsa3072).map_err(crypto)?;
        let public = RsaPublicKey(key.public_key().as_ref().to_vec());
        let certificate = leaf_params(
            "UOB Test Firmware Signing (TEST ONLY)",
            organization,
            vec![KeyUsagePurpose::DigitalSignature],
            vec![ExtendedKeyUsagePurpose::CodeSigning],
        )
        .signed_by(&public, &manufacturer.issuer)
        .map_err(crypto)?;
        Ok(Self {
            key,
            certificate: certificate_pem(certificate.pem())?,
        })
    }

    /// Signs the complete image; the caller bounds its size.
    pub(super) fn sign(&self, firmware: &[u8]) -> Result<FirmwareSignature, TestProviderError> {
        let mut signature = vec![0; self.key.public_modulus_len()];
        self.key
            .sign(
                &RSA_PSS_SHA256,
                &SystemRandom::new(),
                firmware,
                &mut signature,
            )
            .map_err(crypto)?;
        Ok(FirmwareSignature {
            signing_certificate: self.certificate.clone(),
            signature: FirmwareSignatureValue::new(STANDARD.encode(signature)).map_err(crypto)?,
        })
    }
}
