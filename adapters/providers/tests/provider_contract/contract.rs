//! Provider-independent contracts; every implementation of the ports must pass them.
use base64::{Engine as _, engine::general_purpose::STANDARD};
use rcgen::{CertificateParams, DistinguishedName, DnType, KeyPair, PublicKeyData};
use rustls_pki_types::{CertificateDer, UnixTime, pem::PemObject};
use sha2::{Digest, Sha256};
use uob_application::{
    artifact_provider::{
        ArtifactDescriptor, ArtifactKind, ArtifactProvider, ArtifactProviderError,
        ArtifactReference, FirmwareSignature, UploadDestination, UploadId, UploadRequest,
        UploadStatus,
    },
    certificate_provider::{
        CertificateChainPem, CertificateProvider, CertificateSigningRequest, CertificateUse,
        ChainPurpose, ChainVerification, CommonName, CsrDecision, CsrPem, CsrRejection,
        MAX_TRUST_ANCHORS, SignedCertificate, TrustAnchorKind, TrustAnchors, TrustDecision,
        TrustFailure,
    },
};
use webpki::{EndEntityCert, KeyUsage};

use super::now;

/// DER content of id-kp-codeSigning (1.3.6.1.5.5.7.3.3).
const CODE_SIGNING: &[u8] = &[0x2b, 0x06, 0x01, 0x05, 0x05, 0x07, 0x03, 0x03];

pub async fn resolves_published_firmware(
    provider: &dyn ArtifactProvider,
    reference: &ArtifactReference,
    firmware: &[u8],
) -> ArtifactDescriptor {
    let descriptor = provider.resolve(reference).await.unwrap();
    assert_eq!(descriptor.reference(), reference);
    assert!(descriptor.kind().is_firmware());
    assert_eq!(descriptor.integrity().size_bytes, firmware.len() as u64);
    assert_eq!(
        descriptor.integrity().sha256.as_bytes(),
        &<[u8; 32]>::from(Sha256::digest(firmware))
    );
    assert_eq!(descriptor.test_only(), provider.descriptor().test_only);
    let unknown = ArtifactReference::new("contract-unknown-artifact").unwrap();
    assert_eq!(
        provider.resolve(&unknown).await,
        Err(ArtifactProviderError::UnknownArtifact)
    );
    descriptor
}

pub async fn opens_bounded_upload_destinations(
    provider: &dyn ArtifactProvider,
    requested: u64,
) -> UploadDestination {
    for (kind, maximum_bytes) in [
        (ArtifactKind::Firmware, requested),
        (ArtifactKind::SignedFirmware, requested),
        (ArtifactKind::DiagnosticsLog, 0),
    ] {
        let request = UploadRequest {
            kind,
            maximum_bytes,
        };
        assert_eq!(
            provider.open_upload(request).await,
            Err(ArtifactProviderError::InvalidRequest)
        );
    }
    let open = |kind| {
        provider.open_upload(UploadRequest {
            kind,
            maximum_bytes: requested,
        })
    };
    let destination = open(ArtifactKind::DiagnosticsLog).await.unwrap();
    let security = open(ArtifactKind::SecurityLog).await.unwrap();
    for opened in [&destination, &security] {
        assert!(opened.location.as_str().ends_with('/'));
        assert!(opened.maximum_bytes > 0 && opened.maximum_bytes <= requested);
        assert_eq!(opened.test_only, provider.descriptor().test_only);
        assert_eq!(
            provider.upload_status(&opened.upload_id).await,
            Ok(UploadStatus::Pending)
        );
    }
    assert_eq!(security.kind, ArtifactKind::SecurityLog);
    assert_ne!(destination.upload_id, security.upload_id);
    assert_ne!(destination.location, security.location);
    let unknown = UploadId::new("contract-unknown-upload").unwrap();
    assert_eq!(
        provider.upload_status(&unknown).await,
        Err(ArtifactProviderError::UnknownUpload)
    );
    destination
}

/// The signing certificate chains to the installable manufacturer root, and the signature
/// covers exactly the firmware bytes, as a station checks them.
pub async fn signed_firmware_verifies_against_the_manufacturer_root(
    artifacts: &dyn ArtifactProvider,
    pki: &dyn CertificateProvider,
    reference: &ArtifactReference,
    firmware: &[u8],
) {
    let descriptor = artifacts.resolve(reference).await.unwrap();
    assert_eq!(descriptor.kind(), ArtifactKind::SignedFirmware);
    let signature = descriptor.integrity().signature.as_ref().unwrap();
    let verification = ChainVerification {
        purpose: ChainPurpose::FirmwareSigning,
        chain: CertificateChainPem::new(signature.signing_certificate.as_str()).unwrap(),
        expected_common_name: None,
        at: now(),
    };
    assert_eq!(
        pki.verify_chain(&verification).await,
        Ok(TrustDecision::Trusted)
    );
    let roots = pki
        .trust_anchors(TrustAnchorKind::ManufacturerRoot)
        .await
        .unwrap();
    assert!(station_accepts_firmware(&roots, signature, firmware));
    let mut tampered = firmware.to_vec();
    tampered[0] ^= 1;
    assert!(!station_accepts_firmware(&roots, signature, &tampered));
    let csms_roots = pki.trust_anchors(TrustAnchorKind::CsmsRoot).await.unwrap();
    assert!(!station_accepts_firmware(&csms_roots, signature, firmware));
}

/// OCPP 2.0.1 L01.FR.04: RSA-PSS with SHA-256 over the entire file.
pub fn station_accepts_firmware(
    roots: &TrustAnchors,
    signature: &FirmwareSignature,
    firmware: &[u8],
) -> bool {
    let certificate =
        CertificateDer::from_pem_slice(signature.signing_certificate.as_str().as_bytes()).unwrap();
    let end_entity = EndEntityCert::try_from(&certificate).unwrap();
    let roots: Vec<_> = roots
        .certificates()
        .iter()
        .map(|root| CertificateDer::from_pem_slice(root.as_str().as_bytes()).unwrap())
        .collect();
    let anchors: Vec<_> = roots
        .iter()
        .map(|root| webpki::anchor_from_trusted_cert(root).unwrap())
        .collect();
    let chained = end_entity
        .verify_for_usage(
            webpki::ALL_VERIFICATION_ALGS,
            &anchors,
            &[],
            UnixTime::now(),
            KeyUsage::required(CODE_SIGNING),
            None,
            None,
        )
        .is_ok();
    let raw = STANDARD.decode(signature.signature.as_str()).unwrap();
    chained
        && end_entity
            .verify_signature(
                webpki::aws_lc_rs::RSA_PSS_2048_8192_SHA256_LEGACY_KEY,
                firmware,
                &raw,
            )
            .is_ok()
}

pub fn csr(common_name: &str, organization: &str, key: &KeyPair) -> CsrPem {
    let mut name = DistinguishedName::new();
    name.push(DnType::CommonName, common_name);
    name.push(DnType::OrganizationName, organization);
    let mut params = CertificateParams::default();
    params.distinguished_name = name;
    CsrPem::new(params.serialize_request(key).unwrap().pem().unwrap()).unwrap()
}

pub fn signing_request(
    expected: &str,
    certificate_use: CertificateUse,
    csr: CsrPem,
) -> CertificateSigningRequest {
    CertificateSigningRequest {
        expected_common_name: CommonName::new(expected).unwrap(),
        certificate_use,
        csr,
    }
}

pub fn verification(
    purpose: ChainPurpose,
    chain: &CertificateChainPem,
    expected: Option<&str>,
) -> ChainVerification {
    ChainVerification {
        purpose,
        chain: chain.clone(),
        expected_common_name: expected.map(|name| CommonName::new(name).unwrap()),
        at: now(),
    }
}

/// Signs a CSR for the expected station, refuses mismatching subjects, and trusts the result
/// only for that station.
pub async fn signs_station_csrs_for_the_expected_subject(
    pki: &dyn CertificateProvider,
    organization: &str,
) -> SignedCertificate {
    let key = KeyPair::generate().unwrap();
    let request = signing_request(
        "SN-CONTRACT-1",
        CertificateUse::ChargingStation,
        csr("SN-CONTRACT-1", organization, &key),
    );
    let CsrDecision::Signed(signed) = pki.sign_csr(&request).await.unwrap() else {
        panic!("a valid CSR must be signed");
    };
    assert_eq!(signed.test_only, pki.descriptor().test_only);
    let leaf = CertificateDer::from_pem_slice(signed.chain.as_str().as_bytes()).unwrap();
    let (_, parsed) = x509_parser::parse_x509_certificate(&leaf).unwrap();
    assert_eq!(
        parsed.public_key().raw,
        key.subject_public_key_info().as_slice()
    );
    let trusted = verification(
        ChainPurpose::ChargingStation,
        &signed.chain,
        Some("SN-CONTRACT-1"),
    );
    assert_eq!(pki.verify_chain(&trusted).await, Ok(TrustDecision::Trusted));
    let other = verification(
        ChainPurpose::ChargingStation,
        &signed.chain,
        Some("SN-OTHER"),
    );
    assert_eq!(
        pki.verify_chain(&other).await,
        Ok(TrustDecision::Untrusted(TrustFailure::SubjectMismatch))
    );
    for (common_name, csr_organization) in
        [("SN-OTHER", organization), ("SN-CONTRACT-1", "Another CSO")]
    {
        let mismatched = signing_request(
            "SN-CONTRACT-1",
            CertificateUse::ChargingStation,
            csr(common_name, csr_organization, &key),
        );
        assert_eq!(
            pki.sign_csr(&mismatched).await,
            Ok(CsrDecision::Rejected(CsrRejection::SubjectMismatch))
        );
    }
    signed
}

pub async fn supplies_bounded_installable_roots(pki: &dyn CertificateProvider) {
    for kind in [
        TrustAnchorKind::CsmsRoot,
        TrustAnchorKind::ManufacturerRoot,
        TrustAnchorKind::V2gRoot,
        TrustAnchorKind::MoRoot,
    ] {
        let anchors = pki.trust_anchors(kind).await.unwrap();
        assert!(anchors.certificates().len() <= MAX_TRUST_ANCHORS);
        assert_eq!(anchors.test_only(), pki.descriptor().test_only);
        for root in anchors.certificates() {
            let der = CertificateDer::from_pem_slice(root.as_str().as_bytes()).unwrap();
            assert!(webpki::anchor_from_trusted_cert(&der).is_ok());
        }
    }
    for kind in [TrustAnchorKind::CsmsRoot, TrustAnchorKind::ManufacturerRoot] {
        assert!(
            !pki.trust_anchors(kind)
                .await
                .unwrap()
                .certificates()
                .is_empty()
        );
    }
}
