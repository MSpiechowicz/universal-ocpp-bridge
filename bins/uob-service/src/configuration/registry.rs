//! The one target registry shared by start-up, `uob config check`, and the configuration API.
use serde_json::Value;
use uob_application::TargetCapability;
use uob_contracts::{BridgeId, Environment};
use uob_ems_scada_http_target_adapter::EmsScadaHttpTargetFactory;
use uob_mqtt_target_adapter::{EMS_SCADA_PROFILE, MqttTargetFactory, STANDARD_PROFILE};
use uob_target_adapter::{
    TargetDisplayFamily, TargetPreset, TargetRegistration, TargetRegistry, TransportPolicy,
};

use super::ConfigurationLoadError;

/// Builds the one registry used by startup, `uob config check`, and the configuration API.
pub(crate) fn target_registry(
    bridge_id: &BridgeId,
    environment: Environment,
) -> Result<TargetRegistry<Value, Value>, ConfigurationLoadError> {
    let mut targets = TargetRegistry::<Value, Value>::new();
    targets
        .register(
            MqttTargetFactory::new(bridge_id, environment)
                .map_err(|_| ConfigurationLoadError::Composition)?,
            mqtt_registration(),
        )
        .map_err(|_| ConfigurationLoadError::Composition)?;
    targets
        .register(
            EmsScadaHttpTargetFactory::new(environment),
            ems_scada_http_registration(),
        )
        .map_err(|_| ConfigurationLoadError::Composition)?;
    targets
        .declare_first_release_unavailable_targets()
        .map_err(|_| ConfigurationLoadError::Composition)?;
    Ok(targets)
}

fn mqtt_registration() -> TargetRegistration {
    TargetRegistration {
        display_family: TargetDisplayFamily {
            id: "mqtt".to_owned(),
            display_name: "MQTT".to_owned(),
        },
        presets: vec![
            TargetPreset {
                id: STANDARD_PROFILE.to_owned(),
                display_name: "Standard bridge namespace".to_owned(),
            },
            TargetPreset {
                id: EMS_SCADA_PROFILE.to_owned(),
                display_name: "EMS/SCADA over MQTT".to_owned(),
            },
        ],
        capabilities: vec![
            TargetCapability("retained-state".to_owned()),
            TargetCapability("redacted-tracing".to_owned()),
        ],
        transport_policy: Some(TransportPolicy::Outbound),
    }
}

fn ems_scada_http_registration() -> TargetRegistration {
    TargetRegistration {
        display_family: TargetDisplayFamily {
            id: "ems-scada".to_owned(),
            display_name: "EMS/SCADA".to_owned(),
        },
        presets: vec![],
        capabilities: vec![],
        // The factory validates listener exposure itself; see `configured_target`.
        transport_policy: None,
    }
}
