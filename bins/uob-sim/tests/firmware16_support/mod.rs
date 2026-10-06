//! Test-only manufacturer PKI and private state directories for the firmware station model.
#![allow(dead_code)]
use aws_lc_rs::{
    rand::SystemRandom,
    rsa::{KeyPair as RsaKeyPair, KeySize},
    signature::{KeyPair as _, RSA_PSS_SHA256},
};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use rcgen::{
    BasicConstraints, CertificateParams, CertifiedIssuer, DistinguishedName, DnType,
    ExtendedKeyUsagePurpose, IsCa, KeyPair, KeyUsagePurpose, PKCS_RSA_SHA256, PublicKeyData,
    SignatureAlgorithm,
};
use std::{fs, os::unix::fs::PermissionsExt, path::PathBuf};
use time::{Duration, OffsetDateTime};

pub mod model;

/// A manufacturer root and one code-signing firmware certificate issued directly by it.
pub struct TestPki {
    pub root_pem: String,
    pub certificate_pem: String,
    key: RsaKeyPair,
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

impl TestPki {
    #[must_use]
    pub fn generate() -> Self {
        let mut root = params("Sim Test Manufacturer Root (TEST ONLY)");
        root.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        root.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
        let root = CertifiedIssuer::self_signed(root, KeyPair::generate().unwrap()).unwrap();
        let key = RsaKeyPair::generate(KeySize::Rsa2048).unwrap();
        let mut leaf = params("Sim Test Firmware Signing (TEST ONLY)");
        leaf.is_ca = IsCa::ExplicitNoCa;
        leaf.key_usages = vec![KeyUsagePurpose::DigitalSignature];
        leaf.extended_key_usages = vec![ExtendedKeyUsagePurpose::CodeSigning];
        leaf.use_authority_key_identifier_extension = true;
        let certificate = leaf
            .signed_by(&RsaPublicKey(key.public_key().as_ref().to_vec()), &root)
            .unwrap();
        Self {
            root_pem: root.pem(),
            certificate_pem: certificate.pem(),
            key,
        }
    }

    /// RSA-PSS with SHA-256 over the complete image, base64 encoded.
    #[must_use]
    pub fn sign(&self, image: &[u8]) -> String {
        let mut signature = vec![0; self.key.public_modulus_len()];
        self.key
            .sign(&RSA_PSS_SHA256, &SystemRandom::new(), image, &mut signature)
            .unwrap();
        STANDARD.encode(signature)
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

/// An owner-only canonical directory, as the durable native models require.
#[must_use]
pub fn private_directory(label: &str) -> PathBuf {
    let directory = std::env::temp_dir().join(format!("uob-{label}-{}", uuid::Uuid::new_v4()));
    fs::create_dir(&directory).unwrap();
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).unwrap();
    fs::canonicalize(directory).unwrap()
}

#[must_use]
pub fn timestamp(value: OffsetDateTime) -> String {
    value
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap()
}
