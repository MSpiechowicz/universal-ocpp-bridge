//! OCPP 1.6 Security Whitepaper `SecurityEventNotification`: every inbound corpus fixture decodes
//! to a redacted, UTC-normalised observation, every schema-level negative case is a property
//! violation, and neither the observation nor a failure ever carries the raw `techInfo`.
use std::path::{Path, PathBuf};

use serde_json::{Value, json};
use uob_application::ChargerObservation;
use uob_contracts::UtcTimestamp;
use uob_protocol_adapter::{DecodeErrorKind, v16, v201};

const SECRETS: [&str; 2] = ["not-a-real-password-0000", "ZmFrZXVzZXI6ZmFrZXBhc3N3b3Jk"];

fn corpus() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/ocpp-fixtures/corpus")
}

fn json_file(relative: &str) -> Value {
    serde_json::from_slice(&std::fs::read(corpus().join(relative)).unwrap()).unwrap()
}

fn frame(payload: &Value) -> Vec<u8> {
    serde_json::to_vec(&json!([2, "sec-1", "SecurityEventNotification", payload])).unwrap()
}

fn utc(text: &str) -> UtcTimestamp {
    serde_json::from_value(json!(text)).unwrap()
}

fn decode(payload: &Value) -> uob_application::SecurityEventObservation16 {
    let decoded = v16::decode_call(&frame(payload)).unwrap();
    assert_eq!(decoded.message_id, "sec-1");
    assert_eq!(decoded.action.as_str(), "SecurityEventNotification");
    match decoded.observation {
        ChargerObservation::SecurityEvent16(event) => event,
        other => panic!("unexpected observation {other:?}"),
    }
}

/// The inbound security-event fixtures with their independently expected content.
fn inbound_fixtures() -> Vec<(String, Vec<u8>)> {
    let registry = json_file("fixtures.json");
    registry["fixtures"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|fixture| {
            fixture["message_type"] == 2
                && fixture["protocol_version"] == "1.6"
                && fixture["action"] == "SecurityEventNotification"
        })
        .map(|fixture| {
            let id = fixture["id"].as_str().unwrap().to_owned();
            let bytes = std::fs::read(corpus().join(fixture["wire"].as_str().unwrap())).unwrap();
            (id, bytes)
        })
        .collect()
}

#[test]
fn every_inbound_fixture_decodes_to_its_expected_observation() {
    // (fixture id, type, standard, UTC timestamp, techInfo)
    let expected = [
        (
            "firmware-updated",
            "FirmwareUpdated",
            true,
            "2026-10-07T06:15:00Z",
            Some("firmware 4.2.1 is now active"),
        ),
        (
            "minimal",
            "StartupOfTheDevice",
            true,
            "2026-10-07T06:16:00Z",
            None,
        ),
        (
            "reset-or-reboot",
            "ResetOrReboot",
            true,
            "2026-10-07T06:17:30.250Z",
            Some("soft reset requested by the central system"),
        ),
        (
            "vendor-type",
            "AcmeLidSwitchOpened",
            false,
            "2026-10-07T06:15:00Z",
            Some("cabinet lid switch 2"),
        ),
        (
            "offset-timestamp",
            "SettingSystemTime",
            true,
            "2026-10-07T06:15:00Z",
            None,
        ),
    ];
    let fixtures = inbound_fixtures();
    assert_eq!(fixtures.len(), 8);

    for (name, event_type, standard, timestamp, tech_info) in expected {
        let id = format!("wire.ocpp16.security-event-{name}");
        let (_, bytes) = fixtures.iter().find(|(known, _)| *known == id).unwrap();
        let call = v16::decode_call(bytes).unwrap();
        let ChargerObservation::SecurityEvent16(event) = call.observation else {
            panic!("{id}");
        };

        assert_eq!(event.event_type(), event_type, "{id}");
        assert_eq!(event.standard(), standard, "{id}");
        assert_eq!(event.timestamp(), utc(timestamp), "{id}");
        assert_eq!(event.tech_info(), tech_info, "{id}");
        assert_eq!(event.tech_info_redactions(), 0, "{id}");
    }

    // Every inbound fixture, including the bound and credential ones, is accepted.
    for (id, bytes) in &fixtures {
        let call = v16::decode_call(bytes).unwrap_or_else(|_| panic!("{id}"));
        assert_eq!(call.action.as_str(), "SecurityEventNotification", "{id}");
    }
}

#[test]
fn offset_timestamp_is_normalised_to_utc() {
    let event =
        decode(&json!({"type":"SettingSystemTime","timestamp":"2026-10-07T08:15:00+02:00"}));
    let utc_form = decode(&json!({"type":"SettingSystemTime","timestamp":"2026-10-07T06:15:00Z"}));

    assert_eq!(event.timestamp(), utc("2026-10-07T06:15:00Z"));
    assert_eq!(event.timestamp(), utc_form.timestamp());
}

#[test]
fn credential_in_tech_info_is_redacted_everywhere_the_observation_goes() {
    let bytes = inbound_fixtures()
        .into_iter()
        .find(|(id, _)| id.ends_with("tech-info-credential"))
        .unwrap()
        .1;
    for secret in SECRETS {
        assert!(String::from_utf8_lossy(&bytes).contains(secret));
    }

    let call = v16::decode_call(&bytes).unwrap();
    let ChargerObservation::SecurityEvent16(event) = &call.observation else {
        panic!("not a security event");
    };

    assert_eq!(event.event_type(), "MaintenanceLoginFailed");
    assert!(event.tech_info_redactions() > 0);
    let tech_info = event.tech_info().unwrap();
    assert!(tech_info.contains("user=svc-demo origin=touchscreen"));
    assert!(tech_info.contains("[redacted]"));
    for rendering in [
        tech_info.to_owned(),
        format!("{event:?}"),
        format!("{:?}", call.observation),
        format!("{:?}", event.clone().into_record(0)),
    ] {
        for secret in SECRETS {
            assert!(!rendering.contains(secret), "{rendering}");
        }
    }
}

#[test]
fn native_length_bounds_are_counted_in_characters() {
    let type_max = "t".repeat(50);
    let multibyte_type = "é".repeat(50);
    let tech_max = "i".repeat(255);
    let multibyte_tech = "ü".repeat(255);

    for event_type in [&type_max, &multibyte_type] {
        let event = decode(&json!({"type":event_type,"timestamp":"2026-10-07T06:15:00Z"}));
        assert_eq!(event.event_type(), event_type.as_str());
        assert!(!event.standard());
    }
    for tech_info in [&tech_max, &multibyte_tech] {
        let event = decode(
            &json!({"type":"MemoryExhaustion","timestamp":"2026-10-07T06:15:00Z","techInfo":tech_info}),
        );
        assert_eq!(event.tech_info(), Some(tech_info.as_str()));
    }

    for payload in [
        json!({"type":"t".repeat(51),"timestamp":"2026-10-07T06:15:00Z"}),
        json!({"type":"MemoryExhaustion","timestamp":"2026-10-07T06:15:00Z","techInfo":"i".repeat(256)}),
    ] {
        assert!(v16::decode_call(&frame(&payload)).is_err());
    }
}

fn texts(value: &Value, found: &mut Vec<String>) {
    match value {
        Value::String(text) if text.len() >= 4 => found.push(text.clone()),
        Value::Array(items) => items.iter().for_each(|item| texts(item, found)),
        Value::Object(fields) => fields.values().for_each(|item| texts(item, found)),
        _ => {}
    }
}

fn assert_property_violation_without_payload_text(payload: &Value, label: &str) {
    let error = v16::decode_call(&frame(payload)).unwrap_err();
    assert_eq!(error.kind(), DecodeErrorKind::InvalidPayload, "{label}");
    assert_eq!(
        error.call_error().code.as_str(),
        "PropertyConstraintViolation",
        "{label}"
    );

    let rendered = format!(
        "{error} {error:?} {:?} {}",
        error.call_error(),
        error.call_error()
    );
    let mut found = Vec::new();
    texts(payload, &mut found);
    // The pinned wire values themselves must not be echoed back to the charger or logs.
    for text in found
        .into_iter()
        .filter(|text| text != "SecurityEventNotification")
    {
        assert!(!rendered.contains(&text), "{label}: {rendered}");
    }
    for secret in SECRETS {
        assert!(!rendered.contains(secret), "{label}");
    }
}

#[test]
fn every_schema_level_negative_case_is_a_property_violation_without_payload_text() {
    let cases = json_file("wire/1.6/security-negative-cases.json");
    let mut refused = 0;

    for case in cases["cases"].as_array().unwrap() {
        if case["wire"][2] != "SecurityEventNotification" || case["level"] != "schema" {
            continue;
        }
        assert_property_violation_without_payload_text(
            &case["wire"][3],
            case["id"].as_str().unwrap(),
        );
        refused += 1;
    }

    assert_eq!(refused, 11);
}

#[test]
fn further_malformed_requests_are_refused_without_echoing_the_secret() {
    let secret = SECRETS[0];
    let over_long = format!("password={secret} {}", "x".repeat(300));
    for (label, payload) in [
        (
            "null techInfo",
            json!({"type":"A","timestamp":"2026-10-07T06:15:00Z","techInfo":null}),
        ),
        ("null timestamp", json!({"type":"A","timestamp":null})),
        ("date only", json!({"type":"A","timestamp":"2026-10-07"})),
        // Parseable, but past year 9999 or before year 0 once normalised to UTC.
        (
            "utc year overflow",
            json!({"type":"A","timestamp":"9999-12-31T23:00:00-01:00"}),
        ),
        (
            "utc year underflow",
            json!({"type":"A","timestamp":"0000-01-01T00:00:00+00:01"}),
        ),
        (
            "no zone",
            json!({"type":"A","timestamp":"2026-10-07T06:15:00"}),
        ),
        (
            "array techInfo",
            json!({"type":"A","timestamp":"2026-10-07T06:15:00Z","techInfo":[secret]}),
        ),
        (
            "extra field with secret",
            json!({"type":"A","timestamp":"2026-10-07T06:15:00Z","password":secret}),
        ),
        (
            "techInfo too long with secret",
            json!({"type":"A","timestamp":"2026-10-07T06:15:00Z","techInfo":over_long}),
        ),
    ] {
        assert_property_violation_without_payload_text(&payload, label);
    }
}

#[test]
fn the_acknowledged_fixture_is_the_adapter_response() {
    let acknowledged = json_file("wire/1.6/security-event-acknowledged.json");

    assert_eq!(acknowledged[0], 3);
    assert_eq!(acknowledged[2], v16::security_event::response());
}

#[test]
fn ocpp_201_security_event_notification_stays_unsupported() {
    let wire = serde_json::to_vec(&json!([
        2,
        "sec-201",
        "SecurityEventNotification",
        {"type":"StartupOfTheDevice","timestamp":"2026-10-07T06:15:00Z"}
    ]))
    .unwrap();

    let error = v201::decode_call(&wire).unwrap_err();

    assert_eq!(error.kind(), DecodeErrorKind::UnsupportedAction);
    assert_eq!(error.call_error().code.as_str(), "NotImplemented");
}
