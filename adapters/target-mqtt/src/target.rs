use serde::{Serialize, de::DeserializeOwned};
use uob_application::{
    BridgeTarget, DeliverySemantic, TargetCapability, TargetContext, TargetDescriptor,
    TargetLimits, TargetMessageClass, TargetTask,
};
use uob_contracts::{ContractVersion, Operation, TargetKind};

use crate::{
    configuration::{MQTT_TARGET_KIND, MqttRuntimeOptions, MqttSettings, resolve_credentials},
    error::permanent_configuration,
    mapping::TopicNamespace,
    session::Session,
};

pub(crate) struct MqttTarget {
    pub(crate) topics: TopicNamespace,
    pub(crate) settings: MqttSettings,
    pub(crate) runtime: MqttRuntimeOptions,
}

impl MqttTarget {
    pub(crate) const fn new(
        topics: TopicNamespace,
        settings: MqttSettings,
        runtime: MqttRuntimeOptions,
    ) -> Self {
        Self {
            topics,
            settings,
            runtime,
        }
    }
}

impl<E, P> BridgeTarget<E, P> for MqttTarget
where
    E: Serialize + Send + Sync + 'static,
    P: Clone + DeserializeOwned + Send + 'static,
{
    fn descriptor(&self) -> TargetDescriptor {
        TargetDescriptor {
            kind: TargetKind::new(MQTT_TARGET_KIND).expect("static MQTT target kind"),
            instance_id: self.settings.target_instance_id.clone(),
            contract_version: ContractVersion::V1_INITIAL,
            outbound_message_classes: vec![
                TargetMessageClass::StationSnapshot,
                TargetMessageClass::DomainEvent,
                TargetMessageClass::CommandResult,
                TargetMessageClass::Diagnostic,
            ],
            inbound_operations: vec![
                Operation::Start,
                Operation::Stop,
                Operation::SetChargingLimit,
            ],
            limits: TargetLimits {
                maximum_message_bytes: self.runtime.maximum_message_bytes,
                maximum_in_flight_deliveries: self.runtime.maximum_in_flight_deliveries,
                maximum_in_flight_commands: self.runtime.maximum_in_flight_commands,
            },
            delivery_semantics: {
                let mut semantics = vec![
                    DeliverySemantic::NamedPeerAcknowledgement,
                    DeliverySemantic::UncertainHandoff,
                ];
                if self.settings.profile.publishes_point_catalog() {
                    // Retained descriptors and values are exposed on the broker; EMS-client
                    // presence and processing stay unknown without application evidence.
                    semantics.push(DeliverySemantic::LocalExposure);
                }
                semantics
            },
            optional_capabilities: {
                let mut capabilities = vec![
                    TargetCapability("retained-state".to_owned()),
                    TargetCapability("redacted-tracing".to_owned()),
                ];
                if self.settings.home_assistant_discovery {
                    capabilities.push(TargetCapability("home-assistant-discovery".to_owned()));
                }
                if self.settings.profile.publishes_point_catalog() {
                    capabilities.push(TargetCapability("ems-scada-point-catalog".to_owned()));
                }
                capabilities
            },
        }
    }

    fn run(self: Box<Self>, context: TargetContext<E, P>) -> TargetTask {
        Box::pin(async move {
            let credential_reference = self.settings.credentials_file.clone();
            let credentials = tokio::task::spawn_blocking(move || {
                resolve_credentials(credential_reference.as_ref())
            })
            .await
            .map_err(|_| permanent_configuration("mqtt.credentials_task_failed"))?
            .map_err(permanent_configuration)?;
            Session::new(*self, context, credentials).run().await
        })
    }
}
