use super::{
    ConfigurationLoadError, EMS_SCADA_HTTP_TARGET_KIND, EMS_SCADA_PROFILE, FileConfiguration,
    MQTT_TARGET_KIND, validate,
};

#[test]
fn configuration_rejects_unknown_fields_and_embedded_event_credentials() {
    let unknown = "[bridge]\nid='demo'\nunknown=true\n";
    assert!(toml::from_str::<FileConfiguration>(unknown).is_err());

    let document = "[bridge]\nid='demo'\nenvironment='demo'\n[events]\nendpoint='http://user:secret@localhost:8080/api/v1/events'\n";
    let parsed = toml::from_str(document).expect("configuration syntax");
    assert!(matches!(
        validate(parsed),
        Err(ConfigurationLoadError::InvalidEventEndpoint)
    ));
}

#[test]
fn api_only_configuration_uses_shared_composition() {
    let document =
        "[bridge]\nid='demo'\nenvironment='demo'\n[management]\nlisten_addr='127.0.0.1:0'\n";
    let parsed = toml::from_str(document).expect("configuration syntax");
    let validated = validate(parsed).expect("valid configuration");

    assert_eq!(
        validated.service.application.identity().bridge_id.as_str(),
        "demo"
    );
    assert!(validated.service.target_selection.is_none());
    assert_eq!(validated.management_address.port(), 0);
}

#[test]
fn remote_events_require_https_and_a_credential_reference() {
    let document =
        "[bridge]\nid='demo'\n[events]\nendpoint='http://events.example/api/v1/events'\n";
    let parsed = toml::from_str(document).expect("configuration syntax");
    assert!(matches!(
        validate(parsed),
        Err(ConfigurationLoadError::UnsafeRemoteEventEndpoint)
    ));
}

#[test]
fn mqtt_uses_one_broker_url_and_trusted_runtime_namespace() {
    let document = "[bridge]\nid='site-01'\nenvironment='demo'\ntarget_id='main'\n\n[[targets]]\nid='main'\nkind='mqtt'\nenabled=true\n\n[targets.settings]\nbroker_url='mqtt://127.0.0.1:1883'\nallow_plaintext=true\n";
    let parsed = toml::from_str(document).expect("configuration syntax");
    let validated = validate(parsed).expect("valid isolated MQTT configuration");

    assert!(validated.service.targets.contains("mqtt"));
    assert!(validated.service.target_selection.is_some());

    let duplicate = "[bridge]\nid='site-01'\nenvironment='demo'\ntarget_id='main'\n\n[[targets]]\nid='main'\nkind='mqtt'\nenabled=true\ntransport={ endpoint='mqtt://other:1883', encryption='plaintext', explicitly_isolated=true }\n\n[targets.settings]\nbroker_url='mqtt://127.0.0.1:1883'\nallow_plaintext=true\n";
    let parsed = toml::from_str(duplicate).expect("configuration syntax");
    assert!(matches!(
        validate(parsed),
        Err(ConfigurationLoadError::InvalidTransport)
    ));

    let implicit_plaintext = "[bridge]\nid='site-01'\nenvironment='demo'\ntarget_id='main'\n\n[[targets]]\nid='main'\nkind='mqtt'\nenabled=true\n\n[targets.settings]\nbroker_url='mqtt://127.0.0.1:1883'\n";
    let parsed = toml::from_str(implicit_plaintext).expect("configuration syntax");
    assert!(matches!(
        validate(parsed),
        Err(ConfigurationLoadError::InvalidTransport)
    ));
}

#[test]
fn the_ems_scada_preset_selects_one_mqtt_target_and_no_second_listener() {
    let document = "[bridge]\nid='site-01'\nenvironment='demo'\ntarget_id='main'\n\n[[targets]]\nid='main'\nkind='mqtt'\nenabled=true\n\n[targets.settings]\nbroker_url='mqtt://127.0.0.1:1883'\nallow_plaintext=true\nprofile='ems-scada'\n";
    let parsed = toml::from_str(document).expect("configuration syntax");
    let validated = validate(parsed).expect("valid EMS/SCADA MQTT configuration");

    let selection = validated
        .service
        .target_selection
        .as_ref()
        .expect("selected target");
    assert_eq!(
        selection.catalog.kind.as_str(),
        MQTT_TARGET_KIND,
        "the preset must select the MQTT target itself, not a second implementation"
    );
    assert!(
        selection.catalog.presets.len() >= 2,
        "the preset is catalog metadata of the same kind, not a separate target"
    );
    assert_ne!(
        selection.catalog.kind.as_str(),
        EMS_SCADA_HTTP_TARGET_KIND,
        "the broker preset must never select the direct EMS/SCADA HTTP listener"
    );
    assert!(
        selection
            .catalog
            .presets
            .iter()
            .any(|preset| preset.id == EMS_SCADA_PROFILE),
        "the preset must be discoverable as catalog metadata of the same kind"
    );

    let unimplemented = document.replace("profile='ems-scada'", "profile='ems-scada.opcua'");
    let parsed = toml::from_str(&unimplemented).expect("configuration syntax");
    assert!(
        validate(parsed).is_err(),
        "an unimplemented driver must never load as an MQTT preset"
    );
}

#[test]
fn a_direct_ems_scada_http_configuration_starts_no_broker_or_mqtt_task() {
    let document = "[bridge]\nid='site-01'\nenvironment='demo'\ntarget_id='main'\n\n[[targets]]\nid='main'\nkind='ems-scada.http'\nenabled=true\n\n[targets.settings]\nlisten_addr='127.0.0.1:9080'\n";
    let parsed = toml::from_str(document).expect("configuration syntax");
    let validated = validate(parsed).expect("valid direct EMS/SCADA HTTP configuration");

    let selection = validated
        .service
        .target_selection
        .as_ref()
        .expect("selected target");
    assert_eq!(selection.catalog.kind.as_str(), EMS_SCADA_HTTP_TARGET_KIND);
    assert_ne!(
        selection.catalog.kind.as_str(),
        MQTT_TARGET_KIND,
        "a direct HTTP configuration must never select or construct the MQTT adapter"
    );
    assert!(
        selection.catalog.presets.is_empty(),
        "the direct listener is its own kind, not a preset of another target"
    );
    // Exactly one target is ever enabled, so no broker session can be started beside it.
    assert!(
        validated.service.targets.contains(MQTT_TARGET_KIND),
        "the MQTT kind stays catalogued for configuration clients without being selected"
    );
    // The independent management API keeps its own loopback listener in this mode.
    assert!(validated.management_address.ip().is_loopback());
}

#[test]
fn a_public_ems_scada_http_listener_is_rejected_without_tls_and_credentials() {
    let exposed = "[bridge]\nid='site-01'\nenvironment='demo'\ntarget_id='main'\n\n[[targets]]\nid='main'\nkind='ems-scada.http'\nenabled=true\n\n[targets.settings]\nlisten_addr='0.0.0.0:9080'\n";
    let parsed = toml::from_str(exposed).expect("configuration syntax");
    assert!(
        validate(parsed).is_err(),
        "an implicitly exposed integration listener must not load"
    );

    let complete = "[bridge]\nid='site-01'\nenvironment='demo'\ntarget_id='main'\n\n[[targets]]\nid='main'\nkind='ems-scada.http'\nenabled=true\n\n[targets.settings]\nlisten_addr='0.0.0.0:9080'\nremote_access_enabled=true\ntls_certificate_file='/run/uob/ems.crt'\ntls_private_key_file='/run/uob/ems.key'\ncredentials_file='/run/uob/ems.toml'\n";
    let parsed = toml::from_str(complete).expect("configuration syntax");
    assert!(
        validate(parsed).is_ok(),
        "an explicitly enabled, encrypted, authenticated listener must load"
    );
}

#[test]
fn an_ems_scada_http_target_rejects_a_duplicate_transport_block() {
    let document = "[bridge]\nid='site-01'\nenvironment='demo'\ntarget_id='main'\n\n[[targets]]\nid='main'\nkind='ems-scada.http'\nenabled=true\ntransport={ endpoint='https://0.0.0.0:9080', encryption='tls', certificate_verification=true }\n\n[targets.settings]\nlisten_addr='127.0.0.1:9080'\n";
    let parsed = toml::from_str(document).expect("configuration syntax");
    assert!(matches!(
        validate(parsed),
        Err(ConfigurationLoadError::InvalidTransport)
    ));
}

#[test]
fn a_direct_api_configuration_starts_no_broker_task() {
    let document =
        "[bridge]\nid='site-01'\nenvironment='demo'\n[management]\nlisten_addr='127.0.0.1:0'\n";
    let parsed = toml::from_str(document).expect("configuration syntax");
    let validated = validate(parsed).expect("valid direct API configuration");

    assert!(
        validated.service.target_selection.is_none(),
        "an API-only deployment must not construct or start the MQTT adapter"
    );
    assert!(
        validated.service.targets.contains(MQTT_TARGET_KIND),
        "the kind stays catalogued for configuration clients without being selected"
    );
}
