use super::{
    ArtifactDescriptor, ArtifactIntegrity, ArtifactKind, ArtifactLocation, ArtifactProviderError,
    ArtifactReference, ArtifactSha256, FirmwareSignature, FirmwareSignatureValue, UploadId,
};
use crate::certificate_provider::CertificatePem;

const CERTIFICATE: &str = "-----BEGIN CERTIFICATE-----\nAAAA\n-----END CERTIFICATE-----\n";

fn integrity(signature: Option<FirmwareSignature>) -> ArtifactIntegrity {
    ArtifactIntegrity {
        size_bytes: 3,
        sha256: ArtifactSha256::from_bytes([0xab; 32]),
        signature,
    }
}

fn signature() -> FirmwareSignature {
    FirmwareSignature {
        signing_certificate: CertificatePem::new(CERTIFICATE).unwrap(),
        signature: FirmwareSignatureValue::new("c2lnbmF0dXJl").unwrap(),
    }
}

#[test]
fn references_reject_paths_hidden_names_and_oversize_values() {
    assert!(ArtifactReference::new("firmware-1.2.3_demo").is_ok());
    for invalid in ["", ".hidden", "../etc", "a/b", "a b", "ü", &"a".repeat(129)] {
        assert_eq!(
            ArtifactReference::new(invalid),
            Err(ArtifactProviderError::InvalidRequest),
            "{invalid:?}"
        );
    }
    assert!(UploadId::new("0f3c-upload_1").is_ok());
    assert!(UploadId::new("a/b").is_err());
    assert!(UploadId::new("a".repeat(65)).is_err());
}

#[test]
fn locations_require_known_schemes_and_refuse_embedded_credentials() {
    for valid in [
        "http://artifacts:8080/artifacts/firmware",
        "HTTPS://example.test/a?b=c",
        "ftp://example.test/logs/",
        "ftps://example.test",
    ] {
        assert!(ArtifactLocation::new(valid).is_ok(), "{valid}");
    }
    let oversized = format!("http://example.test/{}", "a".repeat(500));
    for invalid in [
        "",
        "file:///var/lib/uob/firmware.bin",
        "/var/lib/uob/firmware.bin",
        "http://",
        "http://user:secret@example.test/logs/",
        "ftp://user@example.test/",
        "http://example.test/a b",
        "http://example.test/\n",
        oversized.as_str(),
    ] {
        assert_eq!(
            ArtifactLocation::new(invalid),
            Err(ArtifactProviderError::InvalidProviderResponse),
            "{invalid:?}"
        );
    }
}

#[test]
fn descriptors_require_firmware_kinds_bytes_and_matching_signatures() {
    let reference = ArtifactReference::new("fw").unwrap();
    let location = ArtifactLocation::new("http://example.test/artifacts/fw").unwrap();
    let descriptor = |kind, integrity| {
        ArtifactDescriptor::new(reference.clone(), kind, location.clone(), integrity, true)
    };
    let signed = descriptor(ArtifactKind::SignedFirmware, integrity(Some(signature()))).unwrap();
    assert!(signed.test_only());
    assert_eq!(signed.kind(), ArtifactKind::SignedFirmware);
    assert!(descriptor(ArtifactKind::Firmware, integrity(None)).is_ok());
    for (kind, integrity) in [
        (ArtifactKind::Firmware, integrity(Some(signature()))),
        (ArtifactKind::SignedFirmware, integrity(None)),
        (ArtifactKind::DiagnosticsLog, integrity(None)),
        (
            ArtifactKind::Firmware,
            ArtifactIntegrity {
                size_bytes: 0,
                ..integrity(None)
            },
        ),
    ] {
        assert_eq!(
            descriptor(kind, integrity),
            Err(ArtifactProviderError::InvalidProviderResponse)
        );
    }
}

#[test]
fn signatures_and_digests_are_bounded_and_rendered_without_payloads() {
    assert!(FirmwareSignatureValue::new("A".repeat(800)).is_ok());
    assert!(FirmwareSignatureValue::new("A".repeat(801)).is_err());
    assert!(FirmwareSignatureValue::new("not base64!").is_err());
    let digest = ArtifactSha256::from_bytes([0x0f; 32]);
    assert_eq!(digest.to_string(), format!("sha256:{}", "0f".repeat(32)));
    assert_eq!(format!("{digest:?}"), digest.to_string());
    assert!(ArtifactKind::SecurityLog.is_log() && !ArtifactKind::SecurityLog.is_firmware());
}
