//! The security station model is derived from the pinned OCA sources, never from bridge code: no
//! bridge crate is reachable, emitted events obey the pinned schema, and every
//! `ExtendedTriggerMessage` class and answer of the pinned schemas is configurable.
mod security16_support;
use security16_support::{drain, now, station};
use serde_json::{Value, json};
use std::collections::BTreeSet;
use uob_sim::security16::{ExtendedTriggerConfig, TECH_INFO_LIMIT, TYPE_LIMIT, validate_event};

fn schema(text: &str) -> Value {
    serde_json::from_str(text).unwrap()
}

fn request_schema() -> Value {
    schema(include_str!(
        "../../../tests/ocpp-fixtures/corpus/schemas/1.6-security/SecurityEventNotification.json"
    ))
}

fn enum_values(schema: &Value, pointer: &str) -> BTreeSet<String> {
    schema
        .pointer(pointer)
        .unwrap()
        .as_array()
        .unwrap()
        .iter()
        .map(|value| value.as_str().unwrap().to_owned())
        .collect()
}

fn snake_case(name: &str) -> String {
    let mut out = String::new();
    for (index, character) in name.chars().enumerate() {
        if character.is_uppercase() && index > 0 {
            out.push('_');
        }
        out.push(character.to_ascii_lowercase());
    }
    out
}

/// Security Whitepaper Ed4 section 8: the standard security event names.
const STANDARD_EVENTS: &[&str] = &[
    "FirmwareUpdated",
    "FailedToAuthenticateAtCentralSystem",
    "CentralSystemFailedToAuthenticate",
    "SettingSystemTime",
    "StartupOfTheDevice",
    "ResetOrReboot",
    "SecurityLogWasCleared",
    "ReconfigurationOfSecurityParameters",
    "MemoryExhaustion",
    "InvalidMessages",
    "AttemptedReplayAttacks",
    "TamperDetectionActivated",
    "InvalidFirmwareSignature",
    "InvalidFirmwareSigningCertificate",
    "InvalidCentralSystemCertificate",
    "InvalidChargePointCertificate",
    "InvalidTLSVersion",
    "InvalidTLSCipherSuite",
    "MaintenanceLoginAccepted",
    "MaintenanceLoginFailed",
    "DiscardedRenewedClientCertificate",
];

#[test]
fn simulator_reaches_no_bridge_crate() {
    let manifest = include_str!("../Cargo.toml");
    let bridge: Vec<_> = manifest
        .lines()
        .filter(|line| line.starts_with("uob"))
        .collect();
    assert!(
        bridge.is_empty(),
        "simulator depends on a bridge crate: {bridge:?}"
    );
    for source in [
        include_str!("../src/security16.rs"),
        include_str!("../src/security16/config.rs"),
        include_str!("../src/security16/model.rs"),
        include_str!("../src/security16/outbox.rs"),
        include_str!("../src/security16/transport.rs"),
        include_str!("../src/scenario/security16.rs"),
    ] {
        assert!(!source.contains("uob_"), "bridge path in simulator source");
    }
}

#[test]
fn schema_bounds_are_the_pinned_ones() {
    let pinned = request_schema();
    assert_eq!(pinned["properties"]["type"]["maxLength"], TYPE_LIMIT);
    assert_eq!(
        pinned["properties"]["techInfo"]["maxLength"],
        TECH_INFO_LIMIT
    );
    assert_eq!(pinned["additionalProperties"], false);
    assert_eq!(pinned["required"], json!(["type", "timestamp"]));
}

#[test]
fn every_emitted_event_conforms_to_the_pinned_request_schema() {
    let pinned = request_schema();
    let properties = pinned["properties"].as_object().unwrap();
    let handle = station(json!({}));
    for (index, kind) in STANDARD_EVENTS.iter().enumerate() {
        let info = (index % 2 == 0).then_some("detail");
        handle.emit(kind, None, info, now()).unwrap();
    }
    handle
        .emit(
            "VendorSpecificEvent",
            Some("2026-10-01T12:00:00+02:00"),
            None,
            now(),
        )
        .unwrap();
    let sent = drain(&handle);
    assert_eq!(sent.len(), STANDARD_EVENTS.len() + 1);
    for payload in sent {
        let fields = payload.as_object().unwrap();
        assert!(
            fields.keys().all(|key| properties.contains_key(key)),
            "{payload}"
        );
        assert!(
            fields.contains_key("type") && fields.contains_key("timestamp"),
            "{payload}"
        );
        assert!(fields.values().all(Value::is_string), "{payload}");
        assert!(payload["type"].as_str().unwrap().chars().count() <= 50);
        assert!(
            validate_event(
                payload["type"].as_str().unwrap(),
                payload["timestamp"].as_str().unwrap(),
                payload["techInfo"].as_str(),
            )
            .is_ok()
        );
    }
}

#[test]
fn every_pinned_extended_trigger_class_and_answer_is_configurable() {
    let request = schema(include_str!(
        "../../../tests/ocpp-fixtures/corpus/schemas/1.6-security/ExtendedTriggerMessage.json"
    ));
    let classes = enum_values(&request, "/definitions/MessageTriggerEnumType/enum");
    assert_eq!(classes.len(), 7);
    for class in &classes {
        let key = snake_case(class);
        for answer in ["accepted", "rejected", "not_implemented"] {
            let config: ExtendedTriggerConfig =
                serde_json::from_value(json!({ key.as_str(): answer })).unwrap();
            assert_eq!(config.reply_delay_ms, 0, "{class}");
        }
    }
    assert!(
        serde_json::from_value::<ExtendedTriggerConfig>(json!({"unknown_class": "accepted"}))
            .is_err()
    );
    let response = schema(include_str!(
        "../../../tests/ocpp-fixtures/corpus/schemas/1.6-security/ExtendedTriggerMessageResponse.json"
    ));
    let statuses = enum_values(&response, "/definitions/TriggerMessageStatusEnumType/enum");
    let configurable: BTreeSet<String> = ["Accepted", "Rejected", "NotImplemented"]
        .map(str::to_owned)
        .into();
    assert_eq!(statuses, configurable);
}
