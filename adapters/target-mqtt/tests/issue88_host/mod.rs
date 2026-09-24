pub mod freshness;
pub mod outage;
#[path = "../../../../tests/ems-contract-host/protocol.rs"]
pub mod protocol;
#[path = "../../../../tests/ems-contract-host/proxy.rs"]
mod proxy;
mod pump;
#[path = "../../../../tests/ems-contract-host/query.rs"]
pub mod query;
pub mod scenario;
pub mod security;

pub use pump::Pump;
use query::{Source, Store};
use std::{path::PathBuf, sync::Arc, time::Duration};
use tokio::sync::mpsc;
use uob_application::{
    BridgeTargetFactory, CommandAdmissionPort, ConfigurationValue, CredentialReference,
    DeliveryReport, ScopedTargetQueryPort, TargetConfiguration, TargetDelivery,
    TargetQueryAuthorization, TargetQueryPermission, TargetResourceScope, TargetRuntimeLimits,
};
use uob_contracts::{
    BridgeId, Environment, StationId, TargetInstanceId, TransactionSnapshot, UtcTimestamp,
};
use uob_mqtt_target_adapter::{EMS_SCADA_PROFILE, MqttTargetFactory};
use uob_target_conformance::{
    FakeTargetHost, HostCapacities, HostContext, target_port_error_from_admission,
};

pub const STATION_SECRET: &str = "issue87-station-secret";
pub const BASE: &str = "uob/v1/demo/site-01";

fn variable(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| panic!("{name} missing from broker runner"))
}
fn now() -> UtcTimestamp {
    UtcTimestamp::new(time::OffsetDateTime::now_utc())
}

fn target_configuration(target_id: TargetInstanceId) -> TargetConfiguration {
    TargetConfiguration::new(target_id, 1)
        .with_setting(
            "broker_url",
            ConfigurationValue::Text(variable("UOB_MQTT_BROKER_URL")),
        )
        .with_setting(
            "profile",
            ConfigurationValue::Text(EMS_SCADA_PROFILE.to_owned()),
        )
        .with_setting(
            "credentials_file",
            ConfigurationValue::CredentialReference(
                CredentialReference::new(variable("UOB_MQTT_TARGET_CREDENTIALS_FILE")).unwrap(),
            ),
        )
}

pub struct Host {
    pub store: Store,
    pub protocol: protocol::ProtocolHost,
    pub folder: PathBuf,
    pub target: tokio::task::JoinHandle<()>,
    pub command_task: tokio::task::JoinHandle<()>,
    pub reports: mpsc::Receiver<DeliveryReport>,
    pub deliveries: mpsc::Sender<TargetDelivery<TransactionSnapshot>>,
}
impl Host {
    pub async fn start() -> Self {
        let folder = std::env::temp_dir().join(format!("uob-issue88-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&folder).unwrap();
        let store = Store::open(folder.join("operational.sqlite"), 32).unwrap();
        let protocol = protocol::ProtocolHost::start(store.clone()).await;
        let factory =
            MqttTargetFactory::new(&BridgeId::new("site-01").unwrap(), Environment::Demo).unwrap();
        let target_id = TargetInstanceId::new("main").unwrap();
        let config = target_configuration(target_id.clone());
        let validated = <MqttTargetFactory as BridgeTargetFactory<
            TransactionSnapshot,
            serde_json::Value,
        >>::validate(&factory, &config)
        .unwrap();
        let target = <MqttTargetFactory as BridgeTargetFactory<
            TransactionSnapshot,
            serde_json::Value,
        >>::create(&factory, validated)
        .unwrap();
        let authorization = TargetQueryAuthorization::new(
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
            authorization,
        ));
        let hosted: HostContext<TransactionSnapshot, serde_json::Value> = FakeTargetHost::build(
            HostCapacities {
                deliveries: 32,
                commands: 8,
                reports: 64,
                diagnostics: 8,
            },
            queries,
            TargetRuntimeLimits {
                maximum_in_flight_deliveries: 32,
                maximum_in_flight_commands: 8,
                maximum_command_bytes: 65536,
            },
            now(),
        )
        .unwrap();
        let mut host = hosted.host;
        let coordinator = protocol.coordinator.clone();
        let (reports_tx, reports) = mpsc::channel(64);
        let (deliveries, mut delivery_rx) = mpsc::channel(32);
        let command_task = tokio::spawn(async move {
            loop {
                while let Ok(delivery) = delivery_rx.try_recv() {
                    host.try_deliver(delivery).unwrap();
                }
                if let Ok(Some(report)) =
                    tokio::time::timeout(Duration::from_millis(1), host.next_report()).await
                    && reports_tx.send(report).await.is_err()
                {
                    return;
                }
                if let Ok(Some(submission)) =
                    tokio::time::timeout(Duration::from_millis(10), host.next_command()).await
                {
                    let outcome = coordinator
                        .submit(submission.command.clone())
                        .await
                        .map_err(|error| target_port_error_from_admission(&error));
                    let _ = submission.respond(outcome);
                }
            }
        });
        let target = tokio::spawn(async move {
            target.run(hosted.context).await.unwrap();
        });
        Self {
            store,
            protocol,
            folder,
            target,
            command_task,
            reports,
            deliveries,
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
