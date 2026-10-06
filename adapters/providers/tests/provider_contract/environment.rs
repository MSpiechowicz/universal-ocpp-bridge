use std::time::Duration;

use rcgen::KeyPair;
use uob_application::{
    IsolatedControl, RuntimeSecurityPolicy, SecurityPolicyError,
    artifact_provider::{
        ArtifactKind, ArtifactProvider, ArtifactProviderError, ArtifactReference, UploadRequest,
    },
    certificate_provider::{
        CertificateProvider, CertificateProviderError, CertificateUse, ChainPurpose,
        TrustAnchorKind,
    },
};
use uob_contracts::Environment;
use uob_provider_adapter::{
    TestProviderError, test_artifacts::TestArtifactService, test_ca::TestCertificateAuthority,
};

use super::{
    ORGANIZATION, Spool, authority, configuration,
    contract::{csr, signing_request, verification},
    firmware, transfers,
};

fn refused(control: IsolatedControl) -> TestProviderError {
    TestProviderError::Policy(SecurityPolicyError::IsolatedControlInProduction(control))
}

#[test]
fn production_refuses_the_test_providers_while_staging_and_demo_accept_them() {
    let spool = Spool::new();
    let production = RuntimeSecurityPolicy::new(Environment::Production);
    assert_eq!(
        TestCertificateAuthority::generate(production, ORGANIZATION).err(),
        Some(refused(IsolatedControl::TestCertificateProvider))
    );
    assert_eq!(
        TestArtifactService::new(
            production,
            transfers(1024, Duration::from_secs(1)),
            configuration("http://artifacts.test", &spool),
        )
        .err(),
        Some(refused(IsolatedControl::TestArtifactProvider))
    );
    for environment in [Environment::Staging, Environment::Demo] {
        let policy = RuntimeSecurityPolicy::new(environment);
        assert!(TestCertificateAuthority::generate(policy, ORGANIZATION).is_ok());
        assert!(
            TestArtifactService::new(
                policy,
                transfers(1024, Duration::from_secs(1)),
                configuration("http://artifacts.test", &spool),
            )
            .is_ok()
        );
    }
}

#[tokio::test]
async fn production_refuses_test_material_produced_in_isolated_environments() {
    let spool = Spool::new();
    let pki = authority();
    let artifacts = TestArtifactService::new(
        RuntimeSecurityPolicy::new(Environment::Staging),
        transfers(1024, Duration::from_secs(1)),
        configuration("http://artifacts.test", &spool),
    )
    .unwrap();
    let reference = ArtifactReference::new("fw").unwrap();
    let firmware = artifacts
        .publish_signed_firmware(reference, firmware(64), &pki)
        .unwrap();
    let upload = artifacts
        .open_upload(UploadRequest {
            kind: ArtifactKind::DiagnosticsLog,
            maximum_bytes: 64,
        })
        .await
        .unwrap();
    let anchors = pki.trust_anchors(TrustAnchorKind::CsmsRoot).await.unwrap();
    let key = KeyPair::generate().unwrap();
    let request = signing_request(
        "SN-1",
        CertificateUse::ChargingStation,
        csr("SN-1", ORGANIZATION, &key),
    );
    let Ok(uob_application::certificate_provider::CsrDecision::Signed(signed)) =
        pki.sign_csr(&request).await
    else {
        panic!("CSR must be signed");
    };
    let production = RuntimeSecurityPolicy::new(Environment::Production);
    let staging = RuntimeSecurityPolicy::new(Environment::Staging);
    for test_only in [
        firmware.test_only(),
        upload.test_only,
        anchors.test_only(),
        signed.test_only,
    ] {
        assert_eq!(
            production.authorize_provider_material(test_only),
            Err(SecurityPolicyError::IsolatedControlInProduction(
                IsolatedControl::TestCredential
            ))
        );
        assert!(staging.authorize_provider_material(test_only).is_ok());
    }
    for subject in [anchors.certificates()[0].as_str(), signed.chain.as_str()] {
        let (_, pem) = x509_parser::pem::parse_x509_pem(subject.as_bytes()).unwrap();
        let certificate = pem.parse_x509().unwrap();
        let names = format!("{} / {}", certificate.subject(), certificate.issuer());
        assert!(names.contains("TEST ONLY"), "{names}");
    }
}

#[tokio::test]
async fn responses_and_errors_disclose_no_private_keys_credentials_or_paths() {
    let spool = Spool::new();
    let pki = authority();
    let artifacts = TestArtifactService::new(
        RuntimeSecurityPolicy::new(Environment::Demo),
        transfers(4096, Duration::from_secs(1)),
        configuration("http://artifacts.test", &spool),
    )
    .unwrap();
    let reference = ArtifactReference::new("fw").unwrap();
    artifacts
        .publish_signed_firmware(reference.clone(), firmware(256), &pki)
        .unwrap();
    let key = KeyPair::generate().unwrap();
    let request = signing_request(
        "SN-1",
        CertificateUse::ChargingStation,
        csr("SN-1", ORGANIZATION, &key),
    );
    let signed = pki.sign_csr(&request).await.unwrap();
    let mut rendered = vec![
        format!("{:?}", artifacts.resolve(&reference).await),
        format!(
            "{:?}",
            artifacts
                .open_upload(UploadRequest {
                    kind: ArtifactKind::SecurityLog,
                    maximum_bytes: 64,
                })
                .await
        ),
        format!("{signed:?}"),
        format!(
            "{artifacts:?} {pki:?} {:?} {:?}",
            artifacts.faults(),
            pki.faults()
        ),
        format!(
            "{:?}",
            artifacts.publish_firmware(reference.clone(), firmware(8192))
        ),
    ];
    if let uob_application::certificate_provider::CsrDecision::Signed(signed) = &signed {
        for purpose in [ChainPurpose::ChargingStation, ChainPurpose::V2g] {
            let decision = pki
                .verify_chain(&verification(purpose, &signed.chain, None))
                .await;
            rendered.push(format!("{decision:?}"));
        }
    }
    for kind in [
        TrustAnchorKind::CsmsRoot,
        TrustAnchorKind::ManufacturerRoot,
        TrustAnchorKind::V2gRoot,
    ] {
        rendered.push(format!("{:?}", pki.trust_anchors(kind).await));
    }
    for error in [
        ArtifactProviderError::InvalidRequest,
        ArtifactProviderError::UnknownArtifact,
        ArtifactProviderError::UnknownUpload,
        ArtifactProviderError::Capacity,
        ArtifactProviderError::Unavailable,
        ArtifactProviderError::TimedOut,
        ArtifactProviderError::InvalidProviderResponse,
    ] {
        rendered.push(format!("{error} {error:?}"));
    }
    for error in [
        CertificateProviderError::InvalidRequest,
        CertificateProviderError::Unavailable,
        CertificateProviderError::TimedOut,
        CertificateProviderError::InvalidProviderResponse,
    ] {
        rendered.push(format!("{error} {error:?}"));
    }
    for error in [
        refused(IsolatedControl::TestArtifactProvider),
        refused(IsolatedControl::TestCertificateProvider),
        TestProviderError::InvalidConfiguration,
        TestProviderError::Capacity,
        TestProviderError::Crypto,
    ] {
        rendered.push(format!("{error} {error:?}"));
    }
    let spool_path = spool.0.to_string_lossy().into_owned();
    let temporary = std::env::temp_dir().to_string_lossy().into_owned();
    for text in rendered {
        for forbidden in [
            "PRIVATE KEY",
            "BEGIN RSA",
            "@",
            spool_path.as_str(),
            temporary.as_str(),
        ] {
            assert!(
                !text.contains(forbidden),
                "{forbidden:?} disclosed in {text}"
            );
        }
    }
}
