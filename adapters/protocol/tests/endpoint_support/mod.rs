#![allow(dead_code)]

use std::{net::SocketAddr, sync::Arc};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use rcgen::{
    BasicConstraints, CertificateParams, CertifiedIssuer, ExtendedKeyUsagePurpose, IsCa, KeyPair,
    KeyUsagePurpose, date_time_ymd,
};
use rustls::{
    ClientConfig, RootCertStore,
    pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer, ServerName},
};
use tokio::{net::TcpStream, task::JoinHandle};
use tokio_rustls::{TlsConnector, client::TlsStream};
use tokio_tungstenite::{
    MaybeTlsStream, WebSocketStream, client_async, connect_async,
    tungstenite::{
        ClientRequestBuilder, Error as WebSocketError, handshake::client::Response,
        http::StatusCode,
    },
};
use uob_application::{Application, ComponentHealth, ComponentHealthState, ComponentKind};
use uob_contracts::{
    ArtifactDigest, BridgeId, Environment, ProcessInstanceId, ReleaseId, RuntimeIdentity,
    ServiceIdentity, StationId, TargetInstanceId,
};
use uob_protocol_adapter::{
    OcppEndpoint, ResolvedStationCredential, StationAuthenticationMode, StationAuthenticator,
    StationCertificateFingerprint, StationConnectionReceiver, StationCredential,
    StationRegistration, StationSecurityConfiguration, StationTlsAcceptor, StationTlsMaterial,
    StationTlsReferences, build_station_tls_acceptor,
};

pub const SECRET: &[u8] = b"station-alpha-credential";
pub const TEST_BOUND: std::time::Duration = std::time::Duration::from_secs(4);

pub struct Identity {
    pub certificate: CertificateDer<'static>,
    private_key: Vec<u8>,
}

pub struct TestPki {
    pub authority: CertificateDer<'static>,
    pub server: Identity,
    pub client: Identity,
}

impl TestPki {
    pub fn current(name: &str) -> Self {
        let authority = certificate_authority(name);
        let authority_der = authority.der().clone();
        let server = signed_identity(
            &authority,
            vec!["localhost".to_owned()],
            ExtendedKeyUsagePurpose::ServerAuth,
        );
        let client = signed_identity(
            &authority,
            vec![format!("{name}.station")],
            ExtendedKeyUsagePurpose::ClientAuth,
        );
        Self {
            authority: authority_der,
            server,
            client,
        }
    }

    pub fn acceptor(&self, mode: StationAuthenticationMode) -> StationTlsAcceptor {
        build_station_tls_acceptor(
            mode,
            StationTlsMaterial {
                server_certificate_chain: vec![self.server.certificate.clone()],
                server_private_key: private_key(&self.server.private_key),
                client_certificate_authorities: if mode
                    == StationAuthenticationMode::CredentialAndMutualTls
                {
                    vec![self.authority.clone()]
                } else {
                    Vec::new()
                },
            },
        )
        .expect("server TLS configuration")
    }
}

pub struct ServerTask(JoinHandle<()>);

impl Drop for ServerTask {
    fn drop(&mut self) {
        self.0.abort();
    }
}

pub fn application(environment: Environment, target: Option<&str>) -> Application {
    let application = Application::new(ServiceIdentity {
        bridge_id: BridgeId::new("endpoint-test").expect("bridge ID"),
        runtime: RuntimeIdentity {
            environment,
            release_id: ReleaseId::new("endpoint-test").expect("release ID"),
            release_digest: ArtifactDigest::new("sha256:endpoint-test").expect("release digest"),
            process_instance_id: ProcessInstanceId::new("endpoint-test").expect("process ID"),
        },
        selected_target_id: target.map(|value| TargetInstanceId::new(value).expect("target ID")),
    });
    if target.is_some() {
        application.health().report_component(
            ComponentKind::Target,
            ComponentHealth {
                state: ComponentHealthState::Reconnecting,
                reconnects: 3,
                backlog_items: 2,
                in_flight_items: 0,
                active_connections: 0,
                reason: Some("target.unavailable".to_owned()),
            },
        );
    }
    application
}

pub fn authenticator(
    mode: StationAuthenticationMode,
    client_certificate: Option<&[u8]>,
) -> StationAuthenticator {
    let station_id = StationId::new("alpha").expect("station ID");
    let configuration = StationSecurityConfiguration {
        authentication: mode,
        tls: StationTlsReferences {
            server_certificate_chain: reference("secret://ocpp/server-certificate"),
            server_private_key: reference("secret://ocpp/server-key"),
            client_certificate_authorities: (mode
                == StationAuthenticationMode::CredentialAndMutualTls)
                .then(|| reference("secret://ocpp/client-ca")),
        },
        stations: vec![StationRegistration {
            station_id: station_id.clone(),
            credential: reference("secret://stations/alpha"),
            client_certificate: client_certificate.map(StationCertificateFingerprint::from_der),
        }],
    }
    .validate()
    .expect("station configuration");
    StationAuthenticator::new(
        configuration,
        vec![ResolvedStationCredential {
            station_id,
            credential: StationCredential::from_secret(SECRET).expect("credential"),
        }],
    )
    .expect("authenticator")
}

pub fn authenticator_many(count: usize) -> StationAuthenticator {
    let stations = (0..count)
        .map(|index| StationRegistration {
            station_id: station_id(index),
            credential: reference(&format!("secret://stations/{index}")),
            client_certificate: None,
        })
        .collect();
    let configuration = StationSecurityConfiguration {
        authentication: StationAuthenticationMode::Credential,
        tls: StationTlsReferences {
            server_certificate_chain: reference("secret://ocpp/server-certificate"),
            server_private_key: reference("secret://ocpp/server-key"),
            client_certificate_authorities: None,
        },
        stations,
    }
    .validate()
    .expect("station configuration");
    let credentials = (0..count)
        .map(|index| ResolvedStationCredential {
            station_id: station_id(index),
            credential: StationCredential::from_secret(station_secret(index).as_bytes())
                .expect("credential"),
        })
        .collect();
    StationAuthenticator::new(configuration, credentials).expect("authenticator")
}

pub fn station_name(index: usize) -> String {
    format!("station-{index:02}")
}

pub fn station_secret(index: usize) -> String {
    format!("station-{index:02}-credential")
}

pub async fn plaintext_endpoint(
    target: Option<&str>,
) -> (SocketAddr, StationConnectionReceiver, ServerTask) {
    let app = application(Environment::Demo, target);
    let (endpoint, receiver) = OcppEndpoint::new(
        authenticator(StationAuthenticationMode::Credential, None),
        &app,
        32,
    )
    .expect("endpoint");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("listener");
    let address = listener.local_addr().expect("listener address");
    let task = tokio::spawn(async move {
        endpoint
            .serve_plaintext(listener)
            .await
            .expect("plaintext endpoint");
    });
    (address, receiver, ServerTask(task))
}

pub async fn tls_endpoint(pki: &TestPki) -> (SocketAddr, StationConnectionReceiver, ServerTask) {
    let app = application(Environment::Production, None);
    let mode = StationAuthenticationMode::CredentialAndMutualTls;
    let (endpoint, receiver) = OcppEndpoint::new(
        authenticator(mode, Some(pki.client.certificate.as_ref())),
        &app,
        4,
    )
    .expect("endpoint");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("listener");
    let address = listener.local_addr().expect("listener address");
    let acceptor = pki.acceptor(mode);
    let task = tokio::spawn(async move {
        endpoint
            .serve_tls(listener, acceptor)
            .await
            .expect("TLS endpoint");
    });
    (address, receiver, ServerTask(task))
}

pub fn request(url: &str, station: &str, secret: &[u8], protocol: &str) -> ClientRequestBuilder {
    let authorization = STANDARD.encode([station.as_bytes(), b":", secret].concat());
    ClientRequestBuilder::new(url.parse().expect("URI"))
        .with_sub_protocol(protocol)
        .with_header("authorization", format!("Basic {authorization}"))
}

pub async fn connect_plain(
    address: SocketAddr,
    station: &str,
    secret: &[u8],
    protocol: &str,
) -> Result<(WebSocketStream<MaybeTlsStream<TcpStream>>, Response), StatusCode> {
    let url = format!("ws://{address}/ocpp/{station}");
    connect_async(request(&url, station, secret, protocol))
        .await
        .map_err(|error| match error {
            WebSocketError::Http(response) => response.status(),
            _ => StatusCode::INTERNAL_SERVER_ERROR,
        })
}

pub async fn connect_plain_without_credential(
    address: SocketAddr,
    station: &str,
    protocol: &str,
) -> Result<(WebSocketStream<MaybeTlsStream<TcpStream>>, Response), StatusCode> {
    let url = format!("ws://{address}/ocpp/{station}");
    let request = ClientRequestBuilder::new(url.parse().expect("URI")).with_sub_protocol(protocol);
    connect_async(request).await.map_err(|error| match error {
        WebSocketError::Http(response) => response.status(),
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    })
}

pub async fn connect_tls(
    address: SocketAddr,
    pki: &TestPki,
    protocol: &str,
) -> Result<(WebSocketStream<TlsStream<TcpStream>>, Response), String> {
    let tcp = TcpStream::connect(address)
        .await
        .map_err(|error| error.to_string())?;
    let connector = TlsConnector::from(client_configuration(pki));
    let tls = connector
        .connect(ServerName::try_from("localhost").expect("server name"), tcp)
        .await
        .map_err(|error| error.to_string())?;
    let url = format!("wss://localhost:{}/ocpp/alpha", address.port());
    let request = request(&url, "alpha", SECRET, protocol);
    client_async(request, tls)
        .await
        .map_err(|error| error.to_string())
}

fn client_configuration(pki: &TestPki) -> Arc<ClientConfig> {
    let mut roots = RootCertStore::empty();
    roots
        .add(pki.authority.clone())
        .expect("server trust anchor");
    Arc::new(
        ClientConfig::builder()
            .with_root_certificates(roots)
            .with_client_auth_cert(
                vec![pki.client.certificate.clone()],
                private_key(&pki.client.private_key),
            )
            .expect("client identity"),
    )
}

fn certificate_authority(name: &str) -> CertifiedIssuer<'static, KeyPair> {
    let mut parameters = CertificateParams::new(vec![format!("{name}.ca")]).expect("CA params");
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
) -> Identity {
    let mut parameters = CertificateParams::new(names).expect("identity params");
    parameters.not_before = date_time_ymd(2020, 1, 1);
    parameters.not_after = date_time_ymd(4096, 1, 1);
    parameters.key_usages = vec![KeyUsagePurpose::DigitalSignature];
    parameters.extended_key_usages = vec![purpose];
    let key = KeyPair::generate().expect("identity key");
    let certificate = parameters.signed_by(&key, authority).expect("identity");
    Identity {
        certificate: certificate.der().clone(),
        private_key: key.serialize_der(),
    }
}

fn private_key(bytes: &[u8]) -> PrivateKeyDer<'static> {
    PrivatePkcs8KeyDer::from(bytes.to_vec()).into()
}

fn reference(value: &str) -> uob_application::CredentialReference {
    uob_application::CredentialReference::new(value).expect("credential reference")
}

fn station_id(index: usize) -> StationId {
    StationId::new(station_name(index)).expect("station ID")
}
