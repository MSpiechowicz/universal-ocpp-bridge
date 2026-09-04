use std::{sync::Arc, time::Duration};

use time::OffsetDateTime;
use tokio::task::JoinHandle;
use uob_application::{
    BridgeTargetFactory, ConfigurationValue, TargetConfiguration, TargetError, TargetRuntimeLimits,
};
use uob_contracts::{BridgeId, Environment, UtcTimestamp};
use uob_mqtt_target_adapter::{
    EMS_SCADA_PROFILE, MqttRuntimeOptions, MqttTargetFactory, STANDARD_PROFILE,
};
use uob_target_conformance::{FakeTargetHost, HostCapacities, UnsupportedQueryPort};

use super::{
    TestBroker,
    fixtures::{TestEvent, target_id},
};

pub struct RunningTarget {
    pub host: FakeTargetHost<TestEvent, ()>,
    pub task: JoinHandle<Result<(), TargetError>>,
}

pub fn start_target(
    broker: &TestBroker,
    bridge: &str,
    runtime: MqttRuntimeOptions,
    capacities: HostCapacities,
) -> RunningTarget {
    start_target_url(&broker.url(), bridge, runtime, capacities)
}

pub fn start_target_url(
    broker_url: &str,
    bridge: &str,
    runtime: MqttRuntimeOptions,
    capacities: HostCapacities,
) -> RunningTarget {
    start_configured_target(broker_url, bridge, runtime, capacities, Preset::Standard)
}

pub fn start_target_with_discovery(
    broker: &TestBroker,
    bridge: &str,
    runtime: MqttRuntimeOptions,
    capacities: HostCapacities,
) -> RunningTarget {
    start_configured_target(
        &broker.url(),
        bridge,
        runtime,
        capacities,
        Preset::HomeAssistant,
    )
}

pub fn start_ems_scada_target(
    broker: &TestBroker,
    bridge: &str,
    runtime: MqttRuntimeOptions,
    capacities: HostCapacities,
) -> RunningTarget {
    start_configured_target(&broker.url(), bridge, runtime, capacities, Preset::EmsScada)
}

/// Exactly one target instance is started; a preset never adds a second target or listener.
#[derive(Clone, Copy, Eq, PartialEq)]
pub enum Preset {
    Standard,
    HomeAssistant,
    EmsScada,
}

fn start_configured_target(
    broker_url: &str,
    bridge: &str,
    runtime: MqttRuntimeOptions,
    capacities: HostCapacities,
    preset: Preset,
) -> RunningTarget {
    let bridge = BridgeId::new(bridge).expect("bridge identity");
    let factory = MqttTargetFactory::new(&bridge, Environment::Demo)
        .expect("MQTT factory")
        .with_runtime_options(runtime)
        .expect("runtime bounds");
    let configuration = TargetConfiguration::new(target_id(), 1)
        .with_setting(
            "broker_url",
            ConfigurationValue::Text(broker_url.to_owned()),
        )
        .with_setting("allow_plaintext", ConfigurationValue::Boolean(true))
        .with_setting(
            "home_assistant_discovery",
            ConfigurationValue::Boolean(preset == Preset::HomeAssistant),
        )
        .with_setting(
            "profile",
            ConfigurationValue::Text(
                match preset {
                    Preset::EmsScada => EMS_SCADA_PROFILE,
                    Preset::Standard | Preset::HomeAssistant => STANDARD_PROFILE,
                }
                .to_owned(),
            ),
        );
    let validated = <MqttTargetFactory as BridgeTargetFactory<TestEvent, ()>>::validate(
        &factory,
        &configuration,
    )
    .expect("valid MQTT configuration");
    let target =
        <MqttTargetFactory as BridgeTargetFactory<TestEvent, ()>>::create(&factory, validated)
            .expect("MQTT target");
    assert!(!target.descriptor().inbound_operations.is_empty());
    let host = FakeTargetHost::build(
        capacities,
        Arc::new(UnsupportedQueryPort),
        TargetRuntimeLimits {
            maximum_in_flight_deliveries: runtime.maximum_in_flight_deliveries,
            maximum_in_flight_commands: runtime.maximum_in_flight_commands,
            maximum_command_bytes: runtime.maximum_message_bytes,
        },
        UtcTimestamp::new(OffsetDateTime::now_utc() + Duration::from_secs(10)),
    )
    .expect("fake target host");
    RunningTarget {
        host: host.host,
        task: tokio::spawn(target.run(host.context)),
    }
}

pub const fn standard_capacities() -> HostCapacities {
    HostCapacities {
        deliveries: 8,
        commands: 1,
        reports: 8,
        diagnostics: 8,
    }
}
