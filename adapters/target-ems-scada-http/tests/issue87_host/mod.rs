#[path = "../../../../tests/ems-contract-host/protocol.rs"]
mod protocol;
#[path = "../../../../tests/ems-contract-host/proxy.rs"]
mod proxy;
#[path = "../../../../tests/ems-contract-host/query.rs"]
mod query;
use query::{Source, Store};
use std::{net::TcpListener as StdListener, path::PathBuf, sync::Arc};
use uob_application::{
    BridgeTargetFactory, CommandAdmissionPort, ConfigurationValue, CredentialReference,
    DeliveryReport, ScopedTargetQueryPort, TargetConfiguration, TargetDelivery,
    TargetQueryAuthorization, TargetQueryPermission, TargetResourceScope, TargetRuntimeLimits,
};
use uob_contracts::{
    BridgeId, Environment, StationEvent, StationId, TargetInstanceId, UtcTimestamp,
};
use uob_ems_scada_http_target_adapter::EmsScadaHttpTargetFactory;
use uob_target_conformance::{
    FakeTargetHost, HostCapacities, HostContext, target_port_error_from_admission,
};

pub const READER: &str = "issue87-reader-token";
pub const OPERATOR: &str = "issue87-operator-token";
pub const STATION_SECRET: &str = "issue87-station-secret";

pub struct Host {
    pub base: String,
    pub socket: String,
    pub store: Store,
    pub folder: PathBuf,
    pub target: tokio::task::JoinHandle<()>,
    pub command_task: tokio::task::JoinHandle<()>,
    pub reports: tokio::sync::mpsc::Receiver<DeliveryReport>,
    pub deliveries: tokio::sync::mpsc::Sender<TargetDelivery<StationEvent>>,
    pub protocol: protocol::ProtocolHost,
}
fn free_addr() -> String {
    let listener = StdListener::bind("127.0.0.1:0").unwrap();
    listener.local_addr().unwrap().to_string()
}
fn now() -> UtcTimestamp {
    UtcTimestamp::new(time::OffsetDateTime::now_utc())
}
fn credentials(path: &std::path::Path) -> CredentialReference {
    std::fs::write(path, format!("[[principals]]\nid = 'reader'\ntoken = '{READER}'\npermissions = ['read']\nstations = [{{ bridge_id = 'site-01', station_id = 'station-a' }}]\n\n[[principals]]\nid = 'operator'\ntoken = '{OPERATOR}'\npermissions = ['read', 'control']\nbridges = ['site-01']\n")).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    CredentialReference::new(path.to_str().unwrap()).unwrap()
}
fn hosted(
    store: &Store,
    target_id: TargetInstanceId,
) -> HostContext<StationEvent, serde_json::Value> {
    let auth = TargetQueryAuthorization::new(
        target_id,
        vec![
            TargetQueryPermission::StationSnapshots,
            TargetQueryPermission::DataPoints,
            TargetQueryPermission::Capabilities,
            TargetQueryPermission::CommandStatus,
            TargetQueryPermission::RetainedEvents,
        ],
        ["station-a", "station-b"]
            .into_iter()
            .map(|station| TargetResourceScope::Station {
                bridge_id: BridgeId::new("site-01").unwrap(),
                station_id: StationId::new(station).unwrap(),
            })
            .collect(),
    );
    let queries = Arc::new(ScopedTargetQueryPort::new(
        Arc::new(Source(store.clone())),
        auth,
    ));
    FakeTargetHost::<StationEvent, serde_json::Value>::build(
        HostCapacities {
            deliveries: 8,
            commands: 8,
            reports: 32,
            diagnostics: 8,
        },
        queries,
        TargetRuntimeLimits {
            maximum_in_flight_deliveries: 8,
            maximum_in_flight_commands: 8,
            maximum_command_bytes: 65536,
        },
        now(),
    )
    .unwrap()
}

impl Host {
    pub async fn start() -> Self {
        let folder = std::env::temp_dir().join(format!("uob-issue87-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&folder).unwrap();
        let store = Store::open(folder.join("operational.sqlite"), 32).unwrap();
        let protocol = protocol::ProtocolHost::start(store.clone()).await;
        let address = free_addr();
        let creds = credentials(&folder.join("ems.toml"));
        let factory = EmsScadaHttpTargetFactory::new(Environment::Demo);
        let target_id = TargetInstanceId::new("main").unwrap();
        let config = TargetConfiguration::new(target_id.clone(), 1)
            .with_setting("listen_addr", ConfigurationValue::Text(address.clone()))
            .with_setting(
                "credentials_file",
                ConfigurationValue::CredentialReference(creds),
            );
        let validated = <EmsScadaHttpTargetFactory as BridgeTargetFactory<
            StationEvent,
            serde_json::Value,
        >>::validate(&factory, &config)
        .unwrap();
        let target = <EmsScadaHttpTargetFactory as BridgeTargetFactory<
            StationEvent,
            serde_json::Value,
        >>::create(&factory, validated)
        .unwrap();
        let hosted = hosted(&store, target_id);
        let mut host = hosted.host;
        let coordinator = protocol.coordinator.clone();
        let (reports_tx, reports) = tokio::sync::mpsc::channel(32);
        let (deliveries, mut delivery_rx) = tokio::sync::mpsc::channel(8);
        // Only the host owns its bounded ports. Every command reaches real durable coordination.
        let command_task = tokio::spawn(async move {
            loop {
                while let Ok(delivery) = delivery_rx.try_recv() {
                    host.try_deliver(delivery).unwrap();
                }
                if let Ok(Some(report)) =
                    tokio::time::timeout(std::time::Duration::from_millis(1), host.next_report())
                        .await
                {
                    let _ = reports_tx.send(report).await;
                }
                if let Ok(Some(submission)) =
                    tokio::time::timeout(std::time::Duration::from_millis(10), host.next_command())
                        .await
                {
                    let outcome = coordinator
                        .submit(submission.command.clone())
                        .await
                        .map_err(|error| target_port_error_from_admission(&error));
                    let _ = submission.respond(outcome);
                }
            }
        });
        // Keep delivery reporting independent of client subscription state.
        let target_task = tokio::spawn(async move {
            target.run(hosted.context).await.unwrap();
        });
        let socket = protocol.proxy_address.clone();
        let base = format!("http://{address}");
        Self {
            base,
            socket,
            store,
            folder,
            target: target_task,
            command_task,
            reports,
            deliveries,
            protocol,
        }
    }
}
impl Drop for Host {
    fn drop(&mut self) {
        self.target.abort();
        self.command_task.abort();
        let _ = std::fs::remove_dir_all(&self.folder);
    }
}

#[cfg(test)]
mod proxy_tests {
    use super::{
        Host,
        proxy::{HANDSHAKE_TIMEOUT, MAX_PENDING_HANDSHAKES},
    };
    use std::time::Duration;
    use tokio::{io::AsyncReadExt, net::TcpStream};

    #[tokio::test]
    async fn idle_peer_is_closed_after_handshake_deadline() {
        let host = Host::start().await;
        let endpoint = reqwest::Url::parse(&host.protocol.proxy_address).unwrap();
        let address = (endpoint.host_str().unwrap(), endpoint.port().unwrap());
        let mut peer = TcpStream::connect(address).await.unwrap();
        let mut byte = [0];
        let closed = tokio::time::timeout(
            HANDSHAKE_TIMEOUT + Duration::from_secs(1),
            peer.read(&mut byte),
        )
        .await
        .expect("idle handshake must not retain a socket indefinitely")
        .expect("idle socket read");
        assert_eq!(
            closed, 0,
            "idle handshake must close without sending a response"
        );
    }

    #[tokio::test]
    async fn excess_pending_handshake_is_dropped_before_deadline() {
        let host = Host::start().await;
        let endpoint = reqwest::Url::parse(&host.protocol.proxy_address).unwrap();
        let address = (endpoint.host_str().unwrap(), endpoint.port().unwrap());
        tokio::time::timeout(Duration::from_secs(1), async {
            let mut pending = Vec::with_capacity(MAX_PENDING_HANDSHAKES);
            for _ in 0..MAX_PENDING_HANDSHAKES {
                pending.push(
                    TcpStream::connect(address)
                        .await
                        .expect("connect pending peer"),
                );
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
            let mut excess = TcpStream::connect(address)
                .await
                .expect("connect excess peer");
            let mut byte = [0];
            assert_eq!(
                excess.read(&mut byte).await.expect("read excess peer"),
                0,
                "excess handshake must be dropped without a response"
            );
            for peer in pending {
                match peer.try_read(&mut byte) {
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
                    other => panic!("capacity peer must still be pending: {other:?}"),
                }
            }
        })
        .await
        .expect("excess handshake must be dropped before the two-second deadline");
    }
}
