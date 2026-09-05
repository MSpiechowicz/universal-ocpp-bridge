//! Canonical reads served by one supervised listener bound to a real socket.
//!
//! The session is given the same scoped query port a composition root supplies, so these tests
//! prove the shipped wiring: the listener answers integration reads out of the host's canonical
//! source and never out of state of its own.

use std::{net::TcpListener as StdTcpListener, sync::Arc, time::Duration};

use uob_application::{
    BridgeTarget, BridgeTargetFactory, CanonicalQuerySource, ConfigurationValue,
    CredentialReference, Page, RetainedEventQuery, ScopedTargetQueryPort, SnapshotCursor,
    TargetConfiguration, TargetPortError, TargetPortErrorCode, TargetPortFuture, TargetQuery,
    TargetQueryAuthorization, TargetQueryPermission, TargetQueryResult, TargetResourceScope,
    TargetRetainedEventStream, TargetRuntimeLimits,
};
use uob_contracts::{
    BridgeId, ContractVersion, Environment, ResourceCapabilities, ResourceRef, StationId,
    StationSnapshot, TargetInstanceId, UtcTimestamp,
};
use uob_ems_scada_http_target_adapter::EmsScadaHttpTargetFactory;
use uob_target_conformance::{FakeTargetHost, HostCapacities};

const READER_TOKEN: &str = "socket-reader-token";

/// One canonical station held by the host, never by the listener.
struct HostState;

fn station_reference() -> ResourceRef {
    ResourceRef {
        bridge_id: BridgeId::new("site-01").expect("bridge identity"),
        station_id: StationId::new("station-a").expect("station identity"),
        resource: None,
        native_protocol_reference: None,
    }
}

fn snapshot() -> StationSnapshot {
    StationSnapshot {
        schema_version: ContractVersion::V1_INITIAL,
        station: station_reference(),
        observed_at: UtcTimestamp::new(time::OffsetDateTime::UNIX_EPOCH),
        connectivity: uob_contracts::Connectivity::Disconnected,
        capabilities: ResourceCapabilities::default(),
        resources: vec![],
        transactions: vec![],
        current_values: vec![],
    }
}

impl CanonicalQuerySource<()> for HostState {
    fn query<'a>(
        &'a self,
        _authorization: &'a TargetQueryAuthorization,
        query: TargetQuery,
    ) -> TargetPortFuture<'a, TargetQueryResult<()>> {
        Box::pin(async move {
            match query {
                TargetQuery::StationSnapshots(_) => Ok(TargetQueryResult::StationSnapshots(Page {
                    items: vec![snapshot()],
                    next_cursor: Option::<SnapshotCursor>::None,
                })),
                _ => Err(TargetPortError::new(
                    TargetPortErrorCode::Unsupported,
                    "query.unsupported",
                )),
            }
        })
    }

    fn subscribe_retained_events<'a>(
        &'a self,
        _authorization: &'a TargetQueryAuthorization,
        query: RetainedEventQuery,
    ) -> TargetPortFuture<'a, TargetRetainedEventStream<()>> {
        Box::pin(async move {
            let cursor = uob_application::RetainedEventCursor::new("uob:event:42").unwrap();
            let event = match query.after {
                Some(after) if after == cursor => None,
                Some(_) => {
                    return Err(TargetPortError::new(
                        TargetPortErrorCode::CursorExpired,
                        "expired",
                    ));
                }
                None => {
                    let mut fixture: serde_json::Value = serde_json::from_str(include_str!(
                        "../../../crates/contracts/tests/fixtures/event-envelope-v1.json"
                    ))
                    .unwrap();
                    fixture["resource"] = serde_json::to_value(station_reference()).unwrap();
                    fixture["payload"] = serde_json::Value::Null;
                    Some(uob_application::RetainedEventItem {
                        cursor,
                        event: serde_json::from_value(fixture).unwrap(),
                    })
                }
            };
            Ok(Box::pin(Subscription(event)) as TargetRetainedEventStream<()>)
        })
    }
}

struct Subscription(Option<uob_application::RetainedEventItem<()>>);
impl uob_application::TargetSubscription<()> for Subscription {
    fn poll_event(
        self: std::pin::Pin<&mut Self>,
        _: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Result<uob_application::RetainedEventItem<()>, TargetPortError>>>
    {
        std::task::Poll::Ready(self.get_mut().0.take().map(Ok))
    }
    fn capacity(&self) -> usize {
        1
    }
    fn backlog(&self) -> usize {
        usize::from(self.0.is_some())
    }
}

fn scoped_query_port() -> Arc<ScopedTargetQueryPort<()>> {
    Arc::new(ScopedTargetQueryPort::new(
        Arc::new(HostState),
        TargetQueryAuthorization::new(
            TargetInstanceId::new("main").expect("target instance"),
            vec![
                TargetQueryPermission::StationSnapshots,
                TargetQueryPermission::RetainedEvents,
            ],
            vec![TargetResourceScope::Station {
                bridge_id: BridgeId::new("site-01").expect("bridge identity"),
                station_id: StationId::new("station-a").expect("station identity"),
            }],
        ),
    ))
}

/// Writes the scoped integration credential document the listener reads at startup.
fn credentials_file() -> (std::path::PathBuf, CredentialReference) {
    let directory = std::env::temp_dir().join(format!(
        "uob-ems-http-socket-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    std::fs::create_dir_all(&directory).expect("create test directory");
    let path = directory.join("integration.toml");
    std::fs::write(
        &path,
        format!(
            "[[principals]]\nid = 'ems-reader'\ntoken = '{READER_TOKEN}'\n\
             permissions = ['read']\nbridges = ['site-01']\n"
        ),
    )
    .expect("write credential file");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
            .expect("restrict credential file");
    }
    let reference = CredentialReference::new(path.to_str().expect("path text")).expect("reference");
    (directory, reference)
}

fn free_loopback_address() -> String {
    let probe = StdTcpListener::bind("127.0.0.1:0").expect("probe listener");
    let address = probe.local_addr().expect("probe address");
    drop(probe);
    format!("127.0.0.1:{}", address.port())
}

fn target(listen_addr: &str, credentials: &CredentialReference) -> Box<dyn BridgeTarget<(), ()>> {
    let factory = EmsScadaHttpTargetFactory::new(Environment::Demo);
    let configuration =
        TargetConfiguration::new(TargetInstanceId::new("main").expect("target instance"), 1)
            .with_setting(
                "listen_addr".to_owned(),
                ConfigurationValue::Text(listen_addr.to_owned()),
            )
            .with_setting(
                "credentials_file".to_owned(),
                ConfigurationValue::CredentialReference(credentials.clone()),
            );
    let validated = <EmsScadaHttpTargetFactory as BridgeTargetFactory<(), ()>>::validate(
        &factory,
        &configuration,
    )
    .expect("validated configuration");
    <EmsScadaHttpTargetFactory as BridgeTargetFactory<(), ()>>::create(&factory, validated)
        .expect("constructed target")
}

#[tokio::test]
async fn a_started_listener_answers_station_reads_from_the_hosts_canonical_source() {
    let address = free_loopback_address();
    let (directory, credentials) = credentials_file();
    let target = target(&address, &credentials);
    let hosted = FakeTargetHost::<(), ()>::build(
        HostCapacities {
            deliveries: 4,
            commands: 2,
            reports: 4,
            diagnostics: 4,
        },
        scoped_query_port(),
        TargetRuntimeLimits {
            maximum_in_flight_deliveries: 4,
            maximum_in_flight_commands: 2,
            maximum_command_bytes: 4096,
        },
        UtcTimestamp::new(time::OffsetDateTime::UNIX_EPOCH),
    )
    .expect("fake host");
    let host = hosted.host;
    let session = tokio::spawn(target.run(hosted.context));

    let authorized = request(&address, "/bridge/v1/stations", Some(READER_TOKEN)).await;
    assert!(authorized.starts_with("HTTP/1.1 200"), "{authorized}");
    assert!(
        authorized.contains("\"station_id\":\"station-a\""),
        "{authorized}"
    );

    // The same read without the configured credential reaches no canonical state at all.
    let anonymous = request(&address, "/bridge/v1/stations", None).await;
    assert!(anonymous.starts_with("HTTP/1.1 401"), "{anonymous}");
    assert!(
        anonymous.contains("ems_scada_http.unauthenticated"),
        "{anonymous}"
    );
    assert!(!anonymous.contains("station-a"), "{anonymous}");

    let events = request(
        &address,
        "/bridge/v1/events?station_id=station-a",
        Some(READER_TOKEN),
    )
    .await;
    assert!(events.starts_with("HTTP/1.1 200"), "{events}");
    assert!(events.contains("event: durable"), "{events}");
    assert!(events.contains("id: uob:event:42"), "{events}");
    let resumed = request(
        &address,
        "/bridge/v1/events?station_id=station-a&after=uob:event:42",
        Some(READER_TOKEN),
    )
    .await;
    assert!(resumed.starts_with("HTTP/1.1 200"), "{resumed}");
    assert!(!resumed.contains("event: durable"), "{resumed}");
    let expired = request(
        &address,
        "/bridge/v1/events?station_id=station-a&after=uob:event:expired",
        Some(READER_TOKEN),
    )
    .await;
    assert!(expired.starts_with("HTTP/1.1 410"), "{expired}");
    assert!(expired.contains("fetch_fresh_snapshot"), "{expired}");

    host.request_shutdown();
    session
        .await
        .expect("session task")
        .expect("graceful shutdown");
    std::fs::remove_dir_all(&directory).expect("remove test directory");
}

/// Sends one minimal HTTP/1.1 request and returns the raw response text.
async fn request(address: &str, path: &str, token: Option<&str>) -> String {
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

    for _ in 0..50 {
        let Ok(mut stream) = tokio::net::TcpStream::connect(address).await else {
            tokio::time::sleep(Duration::from_millis(20)).await;
            continue;
        };
        let authorization = token.map_or_else(String::new, |token| {
            format!("Authorization: Bearer {token}\r\n")
        });
        let request = format!(
            "GET {path} HTTP/1.1\r\nHost: localhost\r\n{authorization}Connection: close\r\n\r\n"
        );
        stream
            .write_all(request.as_bytes())
            .await
            .expect("write request");
        let mut response = Vec::new();
        stream
            .read_to_end(&mut response)
            .await
            .expect("read response");
        return String::from_utf8_lossy(&response).into_owned();
    }
    panic!("integration listener never accepted a connection at {address}");
}
