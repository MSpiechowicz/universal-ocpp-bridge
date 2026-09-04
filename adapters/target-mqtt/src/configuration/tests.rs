use std::time::Duration;

use uob_application::{ConfigurationErrorCode, ConfigurationValue, TargetConfiguration};
use uob_contracts::{BridgeId, Environment, TargetInstanceId};

use super::MqttTargetFactory;

fn configuration(url: &str) -> TargetConfiguration {
    TargetConfiguration::new(TargetInstanceId::new("main").expect("target identity"), 1)
        .with_setting("broker_url", ConfigurationValue::Text(url.to_owned()))
}

#[test]
fn plaintext_needs_an_explicit_demo_only_acknowledgement() {
    let bridge = BridgeId::new("bridge-a").expect("bridge identity");
    let demo = MqttTargetFactory::new(&bridge, Environment::Demo).expect("factory");
    let Err(missing) =
        <MqttTargetFactory as uob_application::BridgeTargetFactory<(), ()>>::validate(
            &demo,
            &configuration("mqtt://127.0.0.1:1883"),
        )
    else {
        panic!("plaintext must not be inferred from demo");
    };
    assert_eq!(missing.code(), ConfigurationErrorCode::MissingField);

    let explicit = configuration("mqtt://127.0.0.1:1883")
        .with_setting("allow_plaintext", ConfigurationValue::Boolean(true));
    assert!(
        <MqttTargetFactory as uob_application::BridgeTargetFactory<(), ()>>::validate(
            &demo, &explicit,
        )
        .is_ok()
    );

    let production = MqttTargetFactory::new(&bridge, Environment::Production).expect("factory");
    assert!(
        <MqttTargetFactory as uob_application::BridgeTargetFactory<(), ()>>::validate(
            &production,
            &explicit,
        )
        .is_err()
    );
}

#[test]
fn tls_rejects_a_stale_plaintext_acknowledgement() {
    let bridge = BridgeId::new("bridge-a").expect("bridge identity");
    let factory = MqttTargetFactory::new(&bridge, Environment::Staging).expect("factory");
    let configuration = configuration("mqtts://broker.example:8883")
        .with_setting("allow_plaintext", ConfigurationValue::Boolean(true));

    assert!(
        <MqttTargetFactory as uob_application::BridgeTargetFactory<(), ()>>::validate(
            &factory,
            &configuration,
        )
        .is_err()
    );
}

#[test]
fn target_settings_cannot_override_trusted_namespace_identity() {
    let bridge = BridgeId::new("trusted-bridge").expect("bridge identity");
    let factory = MqttTargetFactory::new(&bridge, Environment::Demo).expect("factory");
    for forbidden in ["bridge_id", "environment", "client_id"] {
        let configuration = configuration("mqtt://127.0.0.1:1883")
            .with_setting("allow_plaintext", ConfigurationValue::Boolean(true))
            .with_setting(forbidden, ConfigurationValue::Text("spoofed".to_owned()));
        let result = <MqttTargetFactory as uob_application::BridgeTargetFactory<(), ()>>::validate(
            &factory,
            &configuration,
        );
        assert!(result.is_err(), "accepted forbidden setting {forbidden}");
    }
}

#[test]
fn only_implemented_presets_are_selectable_and_discovery_stays_opt_in() {
    let bridge = BridgeId::new("bridge-a").expect("bridge identity");
    let factory = MqttTargetFactory::new(&bridge, Environment::Demo).expect("factory");
    let base = configuration("mqtt://127.0.0.1:1883")
        .with_setting("allow_plaintext", ConfigurationValue::Boolean(true));

    for profile in ["standard", "ems-scada"] {
        let selected = base
            .clone()
            .with_setting("profile", ConfigurationValue::Text(profile.to_owned()));
        assert!(
            <MqttTargetFactory as uob_application::BridgeTargetFactory<(), ()>>::validate(
                &factory, &selected,
            )
            .is_ok(),
            "implemented preset {profile} must validate"
        );
    }

    let unknown = base.clone().with_setting(
        "profile",
        ConfigurationValue::Text("ems-scada.opcua".to_owned()),
    );
    let Err(rejected) =
        <MqttTargetFactory as uob_application::BridgeTargetFactory<(), ()>>::validate(
            &factory, &unknown,
        )
    else {
        panic!("an unimplemented driver must never load as an MQTT preset");
    };
    assert_eq!(rejected.code(), ConfigurationErrorCode::Unsupported);

    let ems = base.with_setting(
        "profile",
        ConfigurationValue::Text(super::EMS_SCADA_PROFILE.to_owned()),
    );
    let settings = factory.settings(&ems).expect("EMS/SCADA settings");
    assert!(settings.profile.publishes_point_catalog());
    assert!(
        !settings.home_assistant_discovery,
        "the industrial preset must not enable consumer discovery by default"
    );
}

#[test]
fn runtime_keepalive_must_fit_the_mqtt_v4_seconds_field_exactly() {
    let bridge = BridgeId::new("bridge-a").expect("bridge identity");
    let factory = MqttTargetFactory::new(&bridge, Environment::Demo).expect("factory");
    for keep_alive in [
        Duration::from_millis(1_500),
        Duration::from_secs(u64::from(u16::MAX) + 1),
    ] {
        let runtime = super::MqttRuntimeOptions {
            keep_alive,
            ..super::MqttRuntimeOptions::default()
        };
        assert!(factory.clone().with_runtime_options(runtime).is_err());
    }
}
