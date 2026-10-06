use rcgen::{
    BasicConstraints, CertificateParams, CertifiedIssuer, DistinguishedName, DnType,
    ExtendedKeyUsagePurpose, IsCa, KeyPair, KeyUsagePurpose, SerialNumber,
};
use time::{Duration, OffsetDateTime};
use uob_application::certificate_provider::CertificatePem;

use crate::test_provider::TestProviderError;

const CA_LIFETIME_DAYS: i64 = 3_650;
const LEAF_LIFETIME_DAYS: i64 = 365;

/// One demo CA with its in-memory signing key and public certificate.
pub(super) struct Ca {
    pub(super) issuer: CertifiedIssuer<'static, KeyPair>,
    pub(super) pem: CertificatePem,
}

/// Demo CSO, manufacturer and V2G hierarchies, generated fresh at construction.
pub(super) struct Hierarchy {
    pub(super) cso_root: Ca,
    pub(super) cso_issuing: Ca,
    pub(super) manufacturer_root: Ca,
    pub(super) v2g_root: Ca,
}

impl Hierarchy {
    pub(super) fn generate(organization: &str) -> Result<Self, TestProviderError> {
        let cso_root = root("UOB Test CSO Root", organization)?;
        let cso_issuing = Ca::new(CertifiedIssuer::signed_by(
            ca_params("UOB Test CSO Issuing CA", organization, Some(0)),
            KeyPair::generate().map_err(crypto)?,
            &cso_root.issuer,
        ))?;
        Ok(Self {
            cso_root,
            cso_issuing,
            manufacturer_root: root("UOB Test Manufacturer Root", organization)?,
            v2g_root: root("UOB Test V2G Root", organization)?,
        })
    }
}

impl Ca {
    fn new(
        issuer: Result<CertifiedIssuer<'static, KeyPair>, rcgen::Error>,
    ) -> Result<Self, TestProviderError> {
        let issuer = issuer.map_err(crypto)?;
        let pem = certificate_pem(issuer.pem())?;
        Ok(Self { issuer, pem })
    }
}

fn root(label: &str, organization: &str) -> Result<Ca, TestProviderError> {
    Ca::new(CertifiedIssuer::self_signed(
        ca_params(label, organization, None),
        KeyPair::generate().map_err(crypto)?,
    ))
}

fn ca_params(label: &str, organization: &str, path_length: Option<u8>) -> CertificateParams {
    let mut params = base_params(
        &format!("{label} (TEST ONLY)"),
        organization,
        CA_LIFETIME_DAYS,
    );
    params.is_ca = IsCa::Ca(path_length.map_or(
        BasicConstraints::Unconstrained,
        BasicConstraints::Constrained,
    ));
    params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
    params
}

/// End-entity parameters chosen by the CA; nothing is copied from an untrusted request.
pub(super) fn leaf_params(
    common_name: &str,
    organization: &str,
    key_usages: Vec<KeyUsagePurpose>,
    extended_key_usages: Vec<ExtendedKeyUsagePurpose>,
) -> CertificateParams {
    let mut params = base_params(common_name, organization, LEAF_LIFETIME_DAYS);
    params.is_ca = IsCa::ExplicitNoCa;
    params.key_usages = key_usages;
    params.extended_key_usages = extended_key_usages;
    params.use_authority_key_identifier_extension = true;
    params
}

fn base_params(common_name: &str, organization: &str, lifetime_days: i64) -> CertificateParams {
    let now = OffsetDateTime::now_utc();
    let mut name = DistinguishedName::new();
    name.push(DnType::CommonName, common_name);
    name.push(DnType::OrganizationName, organization);
    let mut serial = *uuid::Uuid::new_v4().as_bytes();
    serial[0] = (serial[0] & 0x7f) | 0x40;
    let mut params = CertificateParams::default();
    params.distinguished_name = name;
    params.serial_number = Some(SerialNumber::from_slice(&serial));
    params.not_before = now - Duration::hours(1);
    params.not_after = now + Duration::days(lifetime_days);
    params
}

pub(super) fn certificate_pem(pem: String) -> Result<CertificatePem, TestProviderError> {
    CertificatePem::new(pem).map_err(crypto)
}

pub(super) fn crypto<E>(_: E) -> TestProviderError {
    TestProviderError::Crypto
}
