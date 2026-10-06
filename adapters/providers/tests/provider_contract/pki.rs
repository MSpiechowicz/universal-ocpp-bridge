use std::time::Duration;

use base64::{Engine as _, engine::general_purpose::STANDARD};
use rcgen::{CertificateParams, KeyPair, PKCS_ECDSA_P384_SHA384, PKCS_ED25519};
use rustls_pki_types::{CertificateDer, CertificateSigningRequestDer, pem::PemObject};
use time::OffsetDateTime;
use uob_application::certificate_provider::{
    CertificateChainPem, CertificateProvider, CertificateProviderError, CertificateUse,
    ChainPurpose, CsrDecision, CsrPem, CsrRejection, TrustAnchorKind, TrustDecision, TrustFailure,
};
use uob_contracts::UtcTimestamp;
use webpki::{EndEntityCert, KeyUsage};

use uob_provider_adapter::test_ca::TestCertificateAuthority;

use super::{
    ORGANIZATION, authority,
    contract::{
        csr, signing_request, signs_station_csrs_for_the_expected_subject,
        supplies_bounded_installable_roots, verification,
    },
    demo,
};

fn pem(label: &str, der: &[u8]) -> String {
    let encoded = STANDARD.encode(der);
    let lines: Vec<_> = encoded
        .as_bytes()
        .chunks(64)
        .map(|line| std::str::from_utf8(line).unwrap())
        .collect();
    format!(
        "-----BEGIN {label}-----\n{}\n-----END {label}-----\n",
        lines.join("\n")
    )
}

fn rejected(result: Result<CsrDecision, CertificateProviderError>) -> CsrRejection {
    match result {
        Ok(CsrDecision::Rejected(rejection)) => rejection,
        other => panic!("expected a CSR rejection, got {other:?}"),
    }
}

fn untrusted(result: Result<TrustDecision, CertificateProviderError>) -> TrustFailure {
    match result {
        Ok(TrustDecision::Untrusted(failure)) => failure,
        other => panic!("expected an untrusted decision, got {other:?}"),
    }
}

#[tokio::test]
async fn test_ca_passes_the_pki_contract_and_its_station_chain_reaches_the_csms_root() {
    let pki = authority();
    supplies_bounded_installable_roots(&pki).await;
    let signed = signs_station_csrs_for_the_expected_subject(&pki, ORGANIZATION).await;
    let certificates: Vec<_> = CertificateDer::pem_slice_iter(signed.chain.as_str().as_bytes())
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(certificates.len(), 2, "leaf and issuing CA");
    let root = pki.trust_anchors(TrustAnchorKind::CsmsRoot).await.unwrap();
    let root = CertificateDer::from_pem_slice(root.certificates()[0].as_str().as_bytes()).unwrap();
    EndEntityCert::try_from(&certificates[0])
        .unwrap()
        .verify_for_usage(
            webpki::ALL_VERIFICATION_ALGS,
            &[webpki::anchor_from_trusted_cert(&root).unwrap()],
            &certificates[1..],
            rustls_pki_types::UnixTime::now(),
            KeyUsage::client_auth(),
            None,
            None,
        )
        .unwrap();
    assert!(
        pki.trust_anchors(TrustAnchorKind::MoRoot)
            .await
            .unwrap()
            .certificates()
            .is_empty()
    );
}

#[tokio::test]
async fn csr_signing_follows_key_signature_and_request_policy() {
    let pki = authority();
    let request = |csr| signing_request("SN-1", CertificateUse::ChargingStation, csr);
    let p384 = KeyPair::generate_for(&PKCS_ECDSA_P384_SHA384).unwrap();
    assert!(matches!(
        pki.sign_csr(&request(csr("SN-1", ORGANIZATION, &p384)))
            .await,
        Ok(CsrDecision::Signed(_))
    ));
    let ed25519 = KeyPair::generate_for(&PKCS_ED25519).unwrap();
    assert_eq!(
        rejected(
            pki.sign_csr(&request(csr("SN-1", ORGANIZATION, &ed25519)))
                .await
        ),
        CsrRejection::UnsupportedKey
    );
    let key = KeyPair::generate().unwrap();
    let valid = csr("SN-1", ORGANIZATION, &key);
    let mut der = CertificateSigningRequestDer::from_pem_slice(valid.as_str().as_bytes())
        .unwrap()
        .as_ref()
        .to_vec();
    *der.last_mut().unwrap() ^= 0x01;
    let tampered = CsrPem::new(pem("CERTIFICATE REQUEST", &der)).unwrap();
    assert_eq!(
        rejected(pki.sign_csr(&request(tampered)).await),
        CsrRejection::InvalidSignature
    );
    let garbage = CsrPem::new(pem("CERTIFICATE REQUEST", b"not a request")).unwrap();
    assert_eq!(
        rejected(pki.sign_csr(&request(garbage)).await),
        CsrRejection::Malformed
    );
    let mut params = CertificateParams::new(vec!["station.example".to_owned()]).unwrap();
    params.distinguished_name = rcgen::DistinguishedName::new();
    params
        .distinguished_name
        .push(rcgen::DnType::CommonName, "SN-1");
    params
        .distinguished_name
        .push(rcgen::DnType::OrganizationName, ORGANIZATION);
    params.custom_extensions = vec![rcgen::CustomExtension::from_oid_content(
        &[1, 2, 3, 4],
        vec![5, 0],
    )];
    let unsupported = CsrPem::new(params.serialize_request(&key).unwrap().pem().unwrap()).unwrap();
    assert_eq!(
        rejected(pki.sign_csr(&request(unsupported)).await),
        CsrRejection::UnsupportedRequest
    );
}

#[tokio::test]
async fn v2g_certificates_chain_only_to_the_v2g_root() {
    let pki = authority();
    let key = KeyPair::generate().unwrap();
    let request = signing_request(
        "SECC-1",
        CertificateUse::V2g,
        csr("SECC-1", ORGANIZATION, &key),
    );
    let Ok(CsrDecision::Signed(signed)) = pki.sign_csr(&request).await else {
        panic!("V2G CSR must be signed");
    };
    let v2g = verification(ChainPurpose::V2g, &signed.chain, Some("SECC-1"));
    assert_eq!(pki.verify_chain(&v2g).await, Ok(TrustDecision::Trusted));
    let station = verification(ChainPurpose::ChargingStation, &signed.chain, None);
    assert_eq!(
        untrusted(pki.verify_chain(&station).await),
        TrustFailure::UnknownIssuer,
        "the CSO root did not issue V2G certificates"
    );
    let firmware = verification(ChainPurpose::FirmwareSigning, &signed.chain, None);
    assert_eq!(
        untrusted(pki.verify_chain(&firmware).await),
        TrustFailure::WrongPurpose,
        "firmware signing requires an explicit codeSigning usage"
    );
}

#[tokio::test]
async fn trust_decisions_follow_configured_roots_and_validity() {
    let pki = authority();
    let key = KeyPair::generate().unwrap();
    let request = signing_request(
        "SN-2",
        CertificateUse::ChargingStation,
        csr("SN-2", ORGANIZATION, &key),
    );
    let Ok(CsrDecision::Signed(signed)) = pki.sign_csr(&request).await else {
        panic!("CSR must be signed");
    };
    let impostor = authority();
    let Ok(CsrDecision::Signed(impostor)) = impostor.sign_csr(&request).await else {
        panic!("impostor CSR must be signed");
    };
    assert_eq!(
        untrusted(
            pki.verify_chain(&verification(
                ChainPurpose::ChargingStation,
                &impostor.chain,
                None
            ))
            .await
        ),
        TrustFailure::InvalidSignature,
        "an identically named CA does not hold the configured root key"
    );
    let foreign = TestCertificateAuthority::generate(demo(), "Foreign CSO").unwrap();
    let request = signing_request(
        "SN-2",
        CertificateUse::ChargingStation,
        csr("SN-2", "Foreign CSO", &key),
    );
    let Ok(CsrDecision::Signed(foreign)) = foreign.sign_csr(&request).await else {
        panic!("foreign CSR must be signed");
    };
    assert_eq!(
        untrusted(
            pki.verify_chain(&verification(
                ChainPurpose::ChargingStation,
                &foreign.chain,
                None
            ))
            .await
        ),
        TrustFailure::UnknownIssuer
    );
    let mut at = verification(ChainPurpose::ChargingStation, &signed.chain, None);
    at.at = UtcTimestamp::new(OffsetDateTime::now_utc() + time::Duration::days(400));
    assert_eq!(
        untrusted(pki.verify_chain(&at).await),
        TrustFailure::Expired
    );
    at.at = UtcTimestamp::new(OffsetDateTime::now_utc() - time::Duration::days(2));
    assert_eq!(
        untrusted(pki.verify_chain(&at).await),
        TrustFailure::NotYetValid
    );
    let malformed = CertificateChainPem::new(pem("CERTIFICATE", b"not a certificate")).unwrap();
    assert_eq!(
        untrusted(
            pki.verify_chain(&verification(
                ChainPurpose::ChargingStation,
                &malformed,
                None
            ))
            .await
        ),
        TrustFailure::Malformed
    );
    let leaf_only = signed
        .chain
        .as_str()
        .split_inclusive("-----END CERTIFICATE-----\n")
        .next()
        .unwrap();
    let leaf_only = CertificateChainPem::new(leaf_only).unwrap();
    assert_eq!(
        untrusted(
            pki.verify_chain(&verification(
                ChainPurpose::ChargingStation,
                &leaf_only,
                None
            ))
            .await
        ),
        TrustFailure::UnknownIssuer,
        "the issuing CA is not a trust anchor"
    );
}

#[tokio::test(start_paused = true)]
async fn pki_faults_deny_reject_and_delay_requests() {
    let pki = authority();
    let key = KeyPair::generate().unwrap();
    let request = signing_request(
        "SN-3",
        CertificateUse::ChargingStation,
        csr("SN-3", ORGANIZATION, &key),
    );
    pki.faults().set_unavailable(true);
    assert_eq!(
        pki.sign_csr(&request).await,
        Err(CertificateProviderError::Unavailable)
    );
    assert_eq!(
        pki.trust_anchors(TrustAnchorKind::CsmsRoot).await,
        Err(CertificateProviderError::Unavailable)
    );
    pki.faults().set_unavailable(false);
    pki.faults().set_reject_csr(true);
    assert_eq!(
        rejected(pki.sign_csr(&request).await),
        CsrRejection::PolicyRejected
    );
    pki.faults().set_reject_csr(false);
    pki.faults().set_delay(Duration::from_secs(30));
    let started = tokio::time::Instant::now();
    assert!(matches!(
        pki.sign_csr(&request).await,
        Ok(CsrDecision::Signed(_))
    ));
    assert!(started.elapsed() >= Duration::from_secs(30));
    assert!(
        tokio::time::timeout(
            Duration::from_secs(29),
            pki.trust_anchors(TrustAnchorKind::V2gRoot)
        )
        .await
        .is_err()
    );
}
