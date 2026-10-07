//! Test-only manufacturer PKI (RSA-PSS and ECDSA) and drivers for the 2.0.1 firmware model.
#![allow(dead_code)]
#[path = "../firmware16_support/mod.rs"]
mod shared;
#[allow(unused_imports)] // Each test target uses a different subset.
pub use shared::{TestPki, private_directory, timestamp};
pub mod model;

use aws_lc_rs::{
    rand::SystemRandom,
    signature::{ECDSA_P256_SHA256_ASN1_SIGNING, EcdsaKeyPair, KeyPair as _},
};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use rcgen::{
    BasicConstraints, CertificateParams, CertifiedIssuer, DistinguishedName, DnType,
    ExtendedKeyUsagePurpose, IsCa, KeyPair, KeyUsagePurpose, PKCS_ECDSA_P256_SHA256, PublicKeyData,
    SignatureAlgorithm,
};
use time::{Duration, OffsetDateTime};

/// A manufacturer root with an ECDSA P-256 code-signing certificate (errata L01.FR.04).
pub struct EcdsaPki {
    pub root_pem: String,
    pub certificate_pem: String,
    key: EcdsaKeyPair,
}

struct EcPublicKey(Vec<u8>);

impl PublicKeyData for EcPublicKey {
    fn der_bytes(&self) -> &[u8] {
        &self.0
    }

    fn algorithm(&self) -> &'static SignatureAlgorithm {
        &PKCS_ECDSA_P256_SHA256
    }
}

fn params(common_name: &str) -> CertificateParams {
    let now = OffsetDateTime::now_utc();
    let mut name = DistinguishedName::new();
    name.push(DnType::CommonName, common_name);
    let mut params = CertificateParams::default();
    params.distinguished_name = name;
    params.not_before = now - Duration::hours(1);
    params.not_after = now + Duration::days(30);
    params
}

impl EcdsaPki {
    #[must_use]
    pub fn generate() -> Self {
        let mut root = params("Sim Test ECDSA Manufacturer Root (TEST ONLY)");
        root.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        root.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
        let root = CertifiedIssuer::self_signed(root, KeyPair::generate().unwrap()).unwrap();
        let rng = SystemRandom::new();
        let document = EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_ASN1_SIGNING, &rng).unwrap();
        let key =
            EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_ASN1_SIGNING, document.as_ref()).unwrap();
        let mut leaf = params("Sim Test ECDSA Firmware Signing (TEST ONLY)");
        leaf.is_ca = IsCa::ExplicitNoCa;
        leaf.key_usages = vec![KeyUsagePurpose::DigitalSignature];
        leaf.extended_key_usages = vec![ExtendedKeyUsagePurpose::CodeSigning];
        leaf.use_authority_key_identifier_extension = true;
        // rcgen wraps the raw uncompressed point in the SubjectPublicKeyInfo itself.
        let public = EcPublicKey(key.public_key().as_ref().to_vec());
        let certificate = leaf.signed_by(&public, &root).unwrap();
        Self {
            root_pem: root.pem(),
            certificate_pem: certificate.pem(),
            key,
        }
    }

    /// ECDSA P-256 with SHA-256 over the complete image, ASN.1 DER, base64 encoded.
    #[must_use]
    pub fn sign(&self, image: &[u8]) -> String {
        let signature = self.key.sign(&SystemRandom::new(), image).unwrap();
        STANDARD.encode(signature.as_ref())
    }
}
