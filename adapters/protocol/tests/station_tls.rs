#![allow(clippy::result_large_err)]

use std::sync::Arc;

use rcgen::{
    BasicConstraints, CertificateParams, CertifiedIssuer, ExtendedKeyUsagePurpose, IsCa, KeyPair,
    KeyUsagePurpose, date_time_ymd,
};
use rustls::{
    ClientConfig, RootCertStore,
    pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer, ServerName},
};
use tokio::io::DuplexStream;
use tokio_rustls::TlsConnector;
use tokio_tungstenite::{
    accept_hdr_async, client_async,
    tungstenite::{
        client::IntoClientRequest,
        handshake::server::{Request, Response},
        http::{HeaderValue, header::SEC_WEBSOCKET_PROTOCOL},
    },
};
use uob_application::CredentialReference;
use uob_contracts::StationId;
use uob_protocol_adapter::{
    ResolvedStationCredential, StationAuthenticationMode, StationAuthenticator,
    StationCertificateFingerprint, StationCredential, StationRegistration,
    StationSecurityConfiguration, StationTlsHandshakeError, StationTlsMaterial,
    StationTlsReferences, StationTransportAdmissionRequest, build_station_tls_acceptor,
};

const STATION_SECRET: &[u8] = b"station-alpha-credential";

struct Identity {
    certificate: CertificateDer<'static>,
    private_key: Vec<u8>,
}

struct TestPki {
    authority: CertificateDer<'static>,
    server: Identity,
    client: Identity,
}

impl TestPki {
    fn current(name: &str) -> Self {
        Self::with_client_validity(name, 2020, 4096)
    }

    fn with_client_validity(name: &str, not_before: i32, not_after: i32) -> Self {
        let authority = certificate_authority(name);
        let authority_der = authority.der().clone();
        let server = signed_identity(
            &authority,
            vec!["localhost".to_owned()],
            ExtendedKeyUsagePurpose::ServerAuth,
            2020,
            4096,
        );
        let client = signed_identity(
            &authority,
            vec![format!("{name}.station")],
            ExtendedKeyUsagePurpose::ClientAuth,
            not_before,
            not_after,
        );
        Self {
            authority: authority_der,
            server,
            client,
        }
    }
}

fn certificate_authority(name: &str) -> CertifiedIssuer<'static, KeyPair> {
    let mut parameters = CertificateParams::new(vec![format!("{name}.ca")]).expect("CA parameters");
    parameters.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    parameters.key_usages = vec![
        KeyUsagePurpose::KeyCertSign,
        KeyUsagePurpose::DigitalSignature,
    ];
    CertifiedIssuer::self_signed(parameters, KeyPair::generate().expect("CA key"))
        .expect("self-signed CA")
}

fn signed_identity(
    authority: &CertifiedIssuer<'_, KeyPair>,
    names: Vec<String>,
    purpose: ExtendedKeyUsagePurpose,
    not_before: i32,
    not_after: i32,
) -> Identity {
    let mut parameters = CertificateParams::new(names).expect("identity parameters");
    parameters.not_before = date_time_ymd(not_before, 1, 1);
    parameters.not_after = date_time_ymd(not_after, 1, 1);
    parameters.key_usages = vec![KeyUsagePurpose::DigitalSignature];
    parameters.extended_key_usages = vec![purpose];
    let key = KeyPair::generate().expect("identity key");
    let certificate = parameters
        .signed_by(&key, authority)
        .expect("signed identity");
    Identity {
        certificate: certificate.der().clone(),
        private_key: key.serialize_der(),
    }
}

fn private_key(bytes: &[u8]) -> PrivateKeyDer<'static> {
    PrivatePkcs8KeyDer::from(bytes.to_vec()).into()
}

fn server_acceptor(pki: &TestPki) -> uob_protocol_adapter::StationTlsAcceptor {
    build_station_tls_acceptor(
        StationAuthenticationMode::CredentialAndMutualTls,
        StationTlsMaterial {
            server_certificate_chain: vec![pki.server.certificate.clone()],
            server_private_key: private_key(&pki.server.private_key),
            client_certificate_authorities: vec![pki.authority.clone()],
        },
    )
    .expect("server TLS configuration")
}

fn client_configuration(
    server_authority: CertificateDer<'static>,
    identity: Option<&Identity>,
) -> Arc<ClientConfig> {
    let mut roots = RootCertStore::empty();
    roots.add(server_authority).expect("server trust anchor");
    let builder = ClientConfig::builder().with_root_certificates(roots);
    let configuration = match identity {
        Some(identity) => builder
            .with_client_auth_cert(
                vec![identity.certificate.clone()],
                private_key(&identity.private_key),
            )
            .expect("client identity"),
        None => builder.with_no_client_auth(),
    };
    Arc::new(configuration)
}

async fn attempt_wss(
    server_pki: &TestPki,
    client_identity: Option<&Identity>,
    subprotocol: &'static str,
) -> Result<Vec<CertificateDer<'static>>, StationTlsHandshakeError> {
    let acceptor = server_acceptor(server_pki);
    let connector = TlsConnector::from(client_configuration(
        server_pki.authority.clone(),
        client_identity,
    ));
    let (client_io, server_io) = tokio::io::duplex(64 * 1024);

    let server = async move {
        let tls = acceptor.accept(server_io).await?;
        let certificates = tls
            .get_ref()
            .1
            .peer_certificates()
            .unwrap_or_default()
            .to_vec();
        accept_hdr_async(tls, |request: &Request, mut response: Response| {
            if let Some(protocol) = request.headers().get(SEC_WEBSOCKET_PROTOCOL) {
                response
                    .headers_mut()
                    .insert(SEC_WEBSOCKET_PROTOCOL, protocol.clone());
            }
            Ok(response)
        })
        .await
        .map_err(|_| StationTlsHandshakeError)?;
        Ok(certificates)
    };
    let client = connect_websocket(client_io, connector, subprotocol);
    let (server_result, client_result) = tokio::join!(server, client);
    client_result.map_err(|()| StationTlsHandshakeError)?;
    server_result
}

async fn connect_websocket(
    stream: DuplexStream,
    connector: TlsConnector,
    subprotocol: &'static str,
) -> Result<(), ()> {
    let name = ServerName::try_from("localhost").expect("server name");
    let tls = connector.connect(name, stream).await.map_err(|_| ())?;
    let mut request = "wss://localhost/ocpp/alpha"
        .into_client_request()
        .expect("WebSocket request");
    request.headers_mut().insert(
        SEC_WEBSOCKET_PROTOCOL,
        HeaderValue::from_static(subprotocol),
    );
    client_async(request, tls).await.map(|_| ()).map_err(|_| ())
}

fn authenticator(client_certificate: &[u8]) -> StationAuthenticator {
    let station_id = StationId::new("alpha").expect("station ID");
    let configuration = StationSecurityConfiguration {
        authentication: StationAuthenticationMode::CredentialAndMutualTls,
        tls: StationTlsReferences {
            server_certificate_chain: reference("secret://ocpp/server-certificate"),
            server_private_key: reference("secret://ocpp/server-key"),
            client_certificate_authorities: Some(reference("secret://ocpp/client-ca")),
        },
        stations: vec![StationRegistration {
            station_id: station_id.clone(),
            credential: reference("secret://stations/alpha"),
            client_certificate: Some(StationCertificateFingerprint::from_der(client_certificate)),
        }],
    }
    .validate()
    .expect("station configuration");
    StationAuthenticator::new(
        configuration,
        vec![ResolvedStationCredential {
            station_id,
            credential: StationCredential::from_secret(STATION_SECRET).expect("credential"),
        }],
    )
    .expect("authenticator")
}

fn reference(value: &str) -> CredentialReference {
    CredentialReference::new(value).expect("credential reference")
}

#[tokio::test]
async fn both_ocpp_versions_complete_wss_and_station_admission_with_mtls() {
    let pki = TestPki::current("trusted");
    let authenticator = authenticator(pki.client.certificate.as_ref());
    let station_id = StationId::new("alpha").expect("station ID");

    for subprotocol in ["ocpp1.6", "ocpp2.0.1"] {
        let certificates = Box::pin(attempt_wss(&pki, Some(&pki.client), subprotocol))
            .await
            .expect("trusted WSS session");
        assert!(
            authenticator
                .authenticate(StationTransportAdmissionRequest {
                    station_id: &station_id,
                    websocket_subprotocol: subprotocol,
                    credential: Some(STATION_SECRET),
                    peer_certificates: &certificates,
                })
                .is_ok()
        );
    }
}

#[tokio::test]
async fn missing_untrusted_and_expired_client_certificates_fail_closed() {
    let trusted = TestPki::current("trusted");
    assert_eq!(
        Box::pin(attempt_wss(&trusted, None, "ocpp1.6")).await,
        Err(StationTlsHandshakeError)
    );

    let untrusted = TestPki::current("untrusted");
    assert_eq!(
        Box::pin(attempt_wss(&trusted, Some(&untrusted.client), "ocpp1.6")).await,
        Err(StationTlsHandshakeError)
    );

    let expired = TestPki::with_client_validity("trusted", 2018, 2020);
    assert_eq!(
        Box::pin(attempt_wss(&expired, Some(&expired.client), "ocpp2.0.1")).await,
        Err(StationTlsHandshakeError)
    );
    assert_eq!(
        StationTlsHandshakeError.to_string(),
        "station TLS handshake rejected"
    );
}
