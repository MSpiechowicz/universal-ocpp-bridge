use super::{
    CertificateChainPem, CertificatePem, CertificateProviderError, CommonName, CsrPem,
    MAX_TRUST_ANCHORS, TrustAnchors,
};

fn block(label: &str) -> String {
    format!("-----BEGIN {label}-----\nAAAA\n-----END {label}-----\n")
}

#[test]
fn pem_values_hold_only_their_expected_block_type() {
    let certificate = block("CERTIFICATE");
    let request = block("CERTIFICATE REQUEST");
    assert!(CertificatePem::new(certificate.clone()).is_ok());
    assert!(CsrPem::new(request.clone()).is_ok());
    assert!(CertificateChainPem::new(certificate.repeat(5)).is_ok());
    for invalid in [
        String::new(),
        "AAAA".to_owned(),
        certificate.repeat(2),
        request.clone(),
        format!("{certificate}{}", block("PRIVATE KEY")),
        format!("{certificate}{}", block("EC PRIVATE KEY")),
        certificate.replace("AAAA", "ÄÄ"),
        certificate.replace("-----END CERTIFICATE-----", ""),
    ] {
        assert_eq!(
            CertificatePem::new(invalid.clone()),
            Err(CertificateProviderError::InvalidRequest),
            "{invalid:?}"
        );
    }
    assert!(CsrPem::new(certificate.clone()).is_err());
    assert!(CsrPem::new(format!("{request}{}", block("PRIVATE KEY"))).is_err());
    assert!(CertificateChainPem::new(certificate.repeat(6)).is_err());
}

#[test]
fn pem_values_are_bounded_by_ocpp_message_limits() {
    let body = |length: usize| {
        format!(
            "-----BEGIN CERTIFICATE-----\n{}\n-----END CERTIFICATE-----",
            "A".repeat(length)
        )
    };
    let fixed = body(0).len();
    assert!(CertificatePem::new(body(5_500 - fixed)).is_ok());
    assert!(CertificatePem::new(body(5_501 - fixed)).is_err());
    assert!(CertificateChainPem::new(body(10_000 - fixed)).is_ok());
    assert!(CertificateChainPem::new(body(10_001 - fixed)).is_err());
}

#[test]
fn common_names_and_anchor_sets_are_bounded() {
    assert!(CommonName::new("SN-0001 A").is_ok());
    for invalid in [String::new(), "a\nb".to_owned(), "a".repeat(65)] {
        assert!(CommonName::new(invalid).is_err());
    }
    let certificate = CertificatePem::new(block("CERTIFICATE")).unwrap();
    let anchors = TrustAnchors::new(vec![certificate.clone(); MAX_TRUST_ANCHORS], true).unwrap();
    assert!(anchors.test_only());
    assert_eq!(anchors.certificates().len(), MAX_TRUST_ANCHORS);
    assert_eq!(
        TrustAnchors::new(vec![certificate; MAX_TRUST_ANCHORS + 1], false),
        Err(CertificateProviderError::InvalidProviderResponse)
    );
}
