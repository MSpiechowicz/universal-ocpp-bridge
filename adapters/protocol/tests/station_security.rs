use rustls::pki_types::CertificateDer;
use uob_application::CredentialReference;
use uob_contracts::{ProtocolEdition, StationId};
use uob_protocol_adapter::{
    ResolvedStationCredential, StationAuthenticationMode, StationAuthenticator,
    StationCertificateFingerprint, StationCredential, StationRegistration,
    StationSecurityConfiguration, StationSecurityConfigurationErrorCode, StationTlsReferences,
    StationTransportAdmissionError, StationTransportAdmissionRequest,
};

const ALPHA_SECRET: &[u8] = b"alpha-credential-with-entropy";
const BETA_SECRET: &[u8] = b"beta-credential-with-entropy";

fn station(value: &str) -> StationId {
    StationId::new(value).expect("station identity")
}

fn reference(value: &str) -> CredentialReference {
    CredentialReference::new(value).expect("credential reference")
}

fn configuration(authentication: StationAuthenticationMode) -> StationSecurityConfiguration {
    let mutual_tls = authentication == StationAuthenticationMode::CredentialAndMutualTls;
    StationSecurityConfiguration {
        authentication,
        tls: StationTlsReferences {
            server_certificate_chain: reference("secret://ocpp/server-certificate"),
            server_private_key: reference("secret://ocpp/server-key"),
            client_certificate_authorities: mutual_tls
                .then(|| reference("secret://ocpp/client-ca")),
        },
        stations: vec![
            StationRegistration {
                station_id: station("alpha"),
                credential: reference("secret://stations/alpha"),
                client_certificate: mutual_tls
                    .then(|| StationCertificateFingerprint::from_der(b"trusted-alpha-certificate")),
            },
            StationRegistration {
                station_id: station("beta"),
                credential: reference("secret://stations/beta"),
                client_certificate: mutual_tls
                    .then(|| StationCertificateFingerprint::from_der(b"trusted-beta-certificate")),
            },
        ],
    }
}

fn authenticator(authentication: StationAuthenticationMode) -> StationAuthenticator {
    StationAuthenticator::new(
        configuration(authentication)
            .validate()
            .expect("configuration"),
        vec![
            ResolvedStationCredential {
                station_id: station("alpha"),
                credential: StationCredential::from_secret(ALPHA_SECRET).expect("credential"),
            },
            ResolvedStationCredential {
                station_id: station("beta"),
                credential: StationCredential::from_secret(BETA_SECRET).expect("credential"),
            },
        ],
    )
    .expect("authenticator")
}

fn request<'a>(
    station_id: &'a StationId,
    subprotocol: &'a str,
    credential: Option<&'a [u8]>,
    peer_certificates: &'a [CertificateDer<'static>],
) -> StationTransportAdmissionRequest<'a> {
    StationTransportAdmissionRequest {
        station_id,
        websocket_subprotocol: subprotocol,
        credential,
        peer_certificates,
    }
}

#[test]
fn both_ocpp_versions_admit_the_registered_identity_without_target_context() {
    let authenticator = authenticator(StationAuthenticationMode::Credential);
    let alpha = station("alpha");

    for (subprotocol, expected) in [
        ("ocpp1.6", ProtocolEdition::Ocpp16j),
        ("ocpp2.0.1", ProtocolEdition::Ocpp201),
    ] {
        let authenticated = authenticator
            .authenticate(request(&alpha, subprotocol, Some(ALPHA_SECRET), &[]))
            .expect("registered credential");
        assert_eq!(authenticated.station_id, alpha);
        assert_eq!(authenticated.protocol, expected);
    }
}

#[test]
fn unknown_missing_invalid_and_cross_station_credentials_are_denied() {
    let authenticator = authenticator(StationAuthenticationMode::Credential);
    let unknown = station("unknown");
    let alpha = station("alpha");

    assert_eq!(
        authenticator.authenticate(request(&unknown, "ocpp1.6", Some(ALPHA_SECRET), &[],)),
        Err(StationTransportAdmissionError::UnknownStation)
    );
    assert_eq!(
        authenticator.authenticate(request(&alpha, "ocpp1.6", None, &[])),
        Err(StationTransportAdmissionError::MissingCredential)
    );
    assert_eq!(
        authenticator.authenticate(request(
            &alpha,
            "ocpp1.6",
            Some(b"invalid-credential-with-entropy"),
            &[],
        )),
        Err(StationTransportAdmissionError::InvalidCredential)
    );
    assert_eq!(
        authenticator.authenticate(request(&alpha, "ocpp1.6", Some(BETA_SECRET), &[],)),
        Err(StationTransportAdmissionError::InvalidCredential)
    );
}

#[test]
fn mutual_tls_binds_the_trusted_end_entity_certificate_to_the_station() {
    let authenticator = authenticator(StationAuthenticationMode::CredentialAndMutualTls);
    let alpha = station("alpha");
    let alpha_certificate = [CertificateDer::from(b"trusted-alpha-certificate".to_vec())];
    let beta_certificate = [CertificateDer::from(b"trusted-beta-certificate".to_vec())];

    assert!(
        authenticator
            .authenticate(request(
                &alpha,
                "ocpp2.0.1",
                Some(ALPHA_SECRET),
                &alpha_certificate,
            ))
            .is_ok()
    );
    assert_eq!(
        authenticator.authenticate(request(&alpha, "ocpp2.0.1", Some(ALPHA_SECRET), &[],)),
        Err(StationTransportAdmissionError::MissingClientCertificate)
    );
    assert_eq!(
        authenticator.authenticate(request(
            &alpha,
            "ocpp2.0.1",
            Some(ALPHA_SECRET),
            &beta_certificate,
        )),
        Err(StationTransportAdmissionError::ClientCertificateBinding)
    );
}

#[test]
fn configuration_rejects_shared_references_bindings_and_resolved_secrets() {
    let mut shared_reference = configuration(StationAuthenticationMode::Credential);
    shared_reference.stations[1].credential = reference("secret://stations/alpha");
    assert_eq!(
        shared_reference.validate().unwrap_err().code(),
        StationSecurityConfigurationErrorCode::SharedCredentialReference
    );

    let mut shared_certificate = configuration(StationAuthenticationMode::CredentialAndMutualTls);
    shared_certificate.stations[1].client_certificate =
        shared_certificate.stations[0].client_certificate.clone();
    assert_eq!(
        shared_certificate.validate().unwrap_err().code(),
        StationSecurityConfigurationErrorCode::SharedClientCertificate
    );

    let validated = configuration(StationAuthenticationMode::Credential)
        .validate()
        .expect("configuration");
    let error = StationAuthenticator::new(
        validated,
        vec![
            ResolvedStationCredential {
                station_id: station("alpha"),
                credential: StationCredential::from_secret(ALPHA_SECRET).expect("credential"),
            },
            ResolvedStationCredential {
                station_id: station("beta"),
                credential: StationCredential::from_secret(ALPHA_SECRET).expect("credential"),
            },
        ],
    )
    .unwrap_err();
    assert_eq!(
        error.code(),
        StationSecurityConfigurationErrorCode::SharedResolvedCredential
    );
}

#[test]
fn configuration_requires_a_consistent_nonempty_allowlist_and_complete_credentials() {
    let mut missing_ca = configuration(StationAuthenticationMode::CredentialAndMutualTls);
    missing_ca.tls.client_certificate_authorities = None;
    assert_eq!(
        missing_ca.validate().unwrap_err().code(),
        StationSecurityConfigurationErrorCode::ClientCertificateAuthorities
    );

    let mut empty = configuration(StationAuthenticationMode::Credential);
    empty.stations.clear();
    assert_eq!(
        empty.validate().unwrap_err().code(),
        StationSecurityConfigurationErrorCode::EmptyStationAllowlist
    );

    let validated = configuration(StationAuthenticationMode::Credential)
        .validate()
        .expect("configuration");
    assert_eq!(validated.station_count(), 2);
    assert_eq!(
        StationAuthenticator::new(validated, Vec::new())
            .unwrap_err()
            .code(),
        StationSecurityConfigurationErrorCode::ResolvedCredential
    );
    assert_eq!(
        StationCredential::from_secret(b"short").unwrap_err().code(),
        StationSecurityConfigurationErrorCode::ResolvedCredential
    );
}

#[test]
fn errors_and_debug_views_never_echo_secret_material() {
    let secret = b"visible-only-inside-this-test";
    let credential = StationCredential::from_secret(secret).expect("credential");
    let debug = format!(
        "{credential:?} {:?}",
        configuration(StationAuthenticationMode::Credential)
    );
    let error = StationTransportAdmissionError::InvalidCredential.to_string();

    assert!(!debug.contains("visible-only-inside-this-test"));
    assert!(!debug.contains("secret://"));
    assert!(!error.contains("visible-only-inside-this-test"));
    assert!(!error.contains("InvalidCredential"));
}
