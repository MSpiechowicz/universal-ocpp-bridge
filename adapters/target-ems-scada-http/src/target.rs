use uob_application::{
    BridgeTarget, DeliverySemantic, PageLimit, TargetContext, TargetDescriptor, TargetLimits,
    TargetMessageClass, TargetTask,
};
use uob_contracts::{ContractVersion, TargetKind};

use crate::{
    capabilities::ListenerLimits,
    configuration::{EMS_SCADA_HTTP_TARGET_KIND, EmsScadaHttpRuntimeOptions, EmsScadaHttpSettings},
    session::Session,
};

pub(crate) struct EmsScadaHttpTarget {
    pub(crate) settings: EmsScadaHttpSettings,
    pub(crate) runtime: EmsScadaHttpRuntimeOptions,
}

impl EmsScadaHttpTarget {
    pub(crate) const fn new(
        settings: EmsScadaHttpSettings,
        runtime: EmsScadaHttpRuntimeOptions,
    ) -> Self {
        Self { settings, runtime }
    }

    /// Returns the listener's own bounds.
    ///
    /// # Panics
    ///
    /// Panics only if the scan budget became invalid after configuration validation accepted it.
    pub(crate) fn listener_limits(&self) -> ListenerLimits {
        ListenerLimits {
            maximum_request_bytes: self.runtime.maximum_request_bytes,
            maximum_concurrent_requests: self.runtime.maximum_concurrent_clients,
            station_scan_limit: PageLimit::new(self.runtime.maximum_station_scan)
                .expect("validated station scan budget"),
        }
    }

    pub(crate) fn descriptor(&self) -> TargetDescriptor {
        TargetDescriptor {
            kind: TargetKind::new(EMS_SCADA_HTTP_TARGET_KIND)
                .expect("static EMS/SCADA HTTP target kind"),
            instance_id: self.settings.target_instance_id.clone(),
            contract_version: ContractVersion::V1_INITIAL,
            // Canonical state, durable events, and command results reach the integration
            // surface; they are not asserted to have been consumed by an EMS client.
            outbound_message_classes: vec![
                TargetMessageClass::StationSnapshot,
                TargetMessageClass::DomainEvent,
                TargetMessageClass::CommandResult,
            ],
            // Ordinary commands use the host admission port; privileged OCPP is not exposed.
            inbound_operations: vec![
                uob_contracts::Operation::Start,
                uob_contracts::Operation::Stop,
                uob_contracts::Operation::SetChargingLimit,
            ],
            limits: TargetLimits {
                maximum_message_bytes: self.runtime.maximum_message_bytes,
                maximum_in_flight_deliveries: self.runtime.maximum_in_flight_deliveries,
                maximum_in_flight_commands: self.runtime.maximum_in_flight_commands,
            },
            delivery_semantics: vec![DeliverySemantic::LocalExposure],
            optional_capabilities: vec![],
        }
    }
}

impl<E, P> BridgeTarget<E, P> for EmsScadaHttpTarget
where
    E: Send + Sync + 'static,
    P: serde::de::DeserializeOwned + Send + 'static,
{
    fn descriptor(&self) -> TargetDescriptor {
        Self::descriptor(self)
    }

    fn run(self: Box<Self>, context: TargetContext<E, P>) -> TargetTask {
        Box::pin(async move { Session::new(*self, context).run().await })
    }
}
