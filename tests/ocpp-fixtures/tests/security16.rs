use std::collections::BTreeSet;

use jsonschema::Draft;
use serde_json::{Value, json};

const NEGATIVE: &str = include_str!("../corpus/wire/1.6/security-negative-cases.json");
const REGISTRY: &str = include_str!("../corpus/fixtures.json");
const REQUIREMENTS: &str = include_str!("../corpus/security16-requirements.json");
const EVENT_PREFIX: &str = "wire.ocpp16.security-event-";
const TRIGGER_PREFIX: &str = "wire.ocpp16.extended-trigger-";

/// Whitepaper Edition 4 section 8, spelled exactly as the table spells them.
const STANDARD_EVENTS: [&str; 21] = [
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

/// Section 6.16 `MessageTriggerEnumType`.
const TRIGGER_MESSAGES: [&str; 7] = [
    "BootNotification",
    "LogStatusNotification",
    "FirmwareStatusNotification",
    "Heartbeat",
    "MeterValues",
    "SignChargePointCertificate",
    "StatusNotification",
];

fn schema(name: &str) -> jsonschema::Validator {
    let root = uob_ocpp_fixtures::corpus_root()
        .join("schemas/1.6-security")
        .join(format!("{name}.json"));
    let source: Value = serde_json::from_slice(&std::fs::read(root).unwrap()).unwrap();
    jsonschema::options()
        .with_draft(Draft::Draft6)
        .should_validate_formats(true)
        .build(&source)
        .unwrap()
}

fn wire(relative: &str) -> Value {
    let path = uob_ocpp_fixtures::corpus_root().join(relative);
    serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
}

fn security_fixtures() -> Vec<Value> {
    let registry: Value = serde_json::from_str(REGISTRY).unwrap();
    registry["fixtures"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|fixture| {
            let id = fixture["id"].as_str().unwrap();
            id.starts_with(EVENT_PREFIX) || id.starts_with(TRIGGER_PREFIX)
        })
        .cloned()
        .collect()
}

fn schema_name(fixture: &Value) -> String {
    let path = fixture["schema"].as_str().unwrap();
    path.rsplit('/')
        .next()
        .unwrap()
        .trim_end_matches(".json")
        .to_owned()
}

fn payload(frame: &Value) -> &Value {
    frame.as_array().unwrap().last().unwrap()
}

fn fixture_payload(name: &str) -> Value {
    payload(&wire(&format!("wire/1.6/{name}.json"))).clone()
}

fn negative_cases() -> Vec<Value> {
    let cases: Value = serde_json::from_str(NEGATIVE).unwrap();
    cases["cases"].as_array().unwrap().clone()
}

/// CALL frames name their action; CALLRESULT cases name their response schema.
fn case_schema_and_payload(case: &Value) -> (String, &Value) {
    let wire = &case["wire"];
    match wire[0].as_u64() {
        Some(2) => (wire[2].as_str().unwrap().to_owned(), &wire[3]),
        Some(3) => (case["schema"].as_str().unwrap().to_owned(), &wire[2]),
        _ => panic!("{} has no OCPP-J message type", case["id"]),
    }
}

#[test]
fn every_security_wire_fixture_validates_against_its_pinned_schema_with_formats() {
    let fixtures = security_fixtures();
    assert_eq!(fixtures.len(), 20);

    for fixture in &fixtures {
        let id = fixture["id"].as_str().unwrap();
        let frame = wire(fixture["wire"].as_str().unwrap());

        assert!(
            schema(&schema_name(fixture)).is_valid(payload(&frame)),
            "{id}"
        );
        assert_eq!(frame[0], fixture["message_type"], "{id}");
        // A reply frame has no action of its own; its registry entry names the answered action.
        if frame[0] == 2 {
            assert_eq!(frame[2], fixture["action"], "{id}");
        }
    }
}

#[test]
fn schema_level_negatives_fail_and_application_level_negatives_pass_the_schema() {
    let cases = negative_cases();
    assert!(cases.len() >= 25);
    let mut levels = BTreeSet::new();

    for case in &cases {
        let id = case["id"].as_str().unwrap();
        let (name, payload) = case_schema_and_payload(case);
        let level = case["level"].as_str().unwrap();
        levels.insert(level);

        // No case is accepted natively, whichever level refuses it.
        assert!(!case["native_valid"].as_bool().unwrap(), "{id}");
        match level {
            "schema" => {
                assert!(!case["schema_valid"].as_bool().unwrap(), "{id}");
                assert!(!schema(&name).is_valid(payload), "{id} passes the schema");
                assert!(case.get("bridge_refusal").is_none(), "{id}");
            }
            "application" => {
                assert!(case["schema_valid"].as_bool().unwrap(), "{id}");
                assert!(schema(&name).is_valid(payload), "{id} fails the schema");
                assert!(
                    matches!(
                        case["bridge_refusal"].as_str(),
                        Some("UnsupportedOperation" | "InvalidParameters")
                    ),
                    "{id}"
                );
            }
            other => panic!("{id} has unknown level {other}"),
        }
    }
    assert_eq!(levels, BTreeSet::from(["application", "schema"]));
}

#[test]
fn application_level_refusals_are_exactly_deferred_certificates_and_non_positive_connectors() {
    let applications: Vec<Value> = negative_cases()
        .into_iter()
        .filter(|case| case["level"] == "application")
        .collect();
    assert_eq!(applications.len(), 5);

    for case in &applications {
        let id = case["id"].as_str().unwrap();
        let (name, payload) = case_schema_and_payload(case);
        assert_eq!(name, "ExtendedTriggerMessage", "{id}");

        let certificate = payload["requestedMessage"] == "SignChargePointCertificate";
        let non_positive = payload["connectorId"].as_i64().is_some_and(|id| id <= 0);
        // Each refusal has exactly one reason and the code that names it.
        assert!(certificate ^ non_positive, "{id}");
        let expected = if certificate {
            "UnsupportedOperation"
        } else {
            "InvalidParameters"
        };
        assert_eq!(case["bridge_refusal"], expected, "{id}");
    }
}

#[test]
fn positive_trigger_fixtures_never_request_certificates_or_non_positive_connectors() {
    for fixture in security_fixtures() {
        let id = fixture["id"].as_str().unwrap();
        if !id.starts_with(TRIGGER_PREFIX) || fixture["message_type"] != 2 {
            continue;
        }
        let request = payload(&wire(fixture["wire"].as_str().unwrap())).clone();

        assert!(
            TRIGGER_MESSAGES.contains(&request["requestedMessage"].as_str().unwrap()),
            "{id}"
        );
        assert_ne!(
            request["requestedMessage"], "SignChargePointCertificate",
            "{id}"
        );
        if let Some(connector) = request.get("connectorId") {
            assert!(connector.as_i64().unwrap() > 0, "{id}");
        }
    }
}

#[test]
fn security_event_field_limits_hold_exactly_at_the_boundary() {
    let validator = schema("SecurityEventNotification");
    let at = |type_length: usize, info_length: usize| {
        json!({
            "type": "T".repeat(type_length),
            "timestamp": "2026-10-07T06:15:00Z",
            "techInfo": "i".repeat(info_length),
        })
    };

    assert!(validator.is_valid(&at(50, 255)));
    assert!(!validator.is_valid(&at(51, 0)));
    assert!(!validator.is_valid(&at(1, 256)));

    let type_max = fixture_payload("security-event-type-max");
    assert_eq!(type_max["type"].as_str().unwrap().chars().count(), 50);
    let info_max = fixture_payload("security-event-tech-info-max");
    assert_eq!(info_max["techInfo"].as_str().unwrap().chars().count(), 255);
    // The empty reply is the whole CALLRESULT payload; the schema permits no field.
    assert!(schema("SecurityEventNotificationResponse").is_valid(&json!({})));
}

#[test]
fn section_eight_names_are_spelled_as_the_whitepaper_spells_them() {
    let requirements: Value = serde_json::from_str(REQUIREMENTS).unwrap();
    let listed: Vec<&str> = requirements["security_events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|event| event["name"].as_str().unwrap())
        .collect();
    assert_eq!(listed, STANDARD_EVENTS);

    for fixture in security_fixtures() {
        let id = fixture["id"].as_str().unwrap();
        if !id.starts_with(EVENT_PREFIX) || fixture["message_type"] != 2 {
            continue;
        }
        let event_type = payload(&wire(fixture["wire"].as_str().unwrap()))["type"]
            .as_str()
            .unwrap()
            .to_owned();
        let vendor = id.ends_with("vendor-type") || id.ends_with("type-max");
        assert_eq!(
            !STANDARD_EVENTS.contains(&event_type.as_str()),
            vendor,
            "{id} uses {event_type}"
        );
    }
}

#[test]
fn credential_fixture_carries_labelled_fake_secrets_and_the_offset_fixture_a_local_offset() {
    let credential = fixture_payload("security-event-tech-info-credential");
    let info = credential["techInfo"].as_str().unwrap();

    assert!(info.contains("password=not-a-real-password"));
    assert!(info.contains("Basic ZmFrZXVzZXI6ZmFrZXBhc3N3b3Jk"));
    assert_eq!(credential["type"], "MaintenanceLoginFailed");

    let offset = fixture_payload("security-event-offset-timestamp");
    let timestamp = offset["timestamp"].as_str().unwrap();
    assert!(timestamp.ends_with("+02:00"), "{timestamp}");
}

#[test]
fn trigger_enumerations_and_statuses_match_the_pinned_schemas() {
    let request = schema("ExtendedTriggerMessage");
    for message in TRIGGER_MESSAGES {
        assert!(
            request.is_valid(&json!({"requestedMessage": message})),
            "{message}"
        );
        assert!(
            request.is_valid(&json!({"requestedMessage": message, "connectorId": 1})),
            "{message}"
        );
    }
    // The legacy TriggerMessage names that 6.16 leaves out stay invalid.
    for message in ["DiagnosticsStatusNotification", "heartbeat", ""] {
        assert!(
            !request.is_valid(&json!({"requestedMessage": message})),
            "{message}"
        );
    }

    let reply = schema("ExtendedTriggerMessageResponse");
    for status in ["Accepted", "Rejected", "NotImplemented"] {
        assert!(reply.is_valid(&json!({"status": status})), "{status}");
    }
    assert!(!reply.is_valid(&json!({"status": "Scheduled"})));
    assert!(!reply.is_valid(&json!({})));
}

#[test]
fn requirements_cite_every_registered_fixture_and_every_citation_exists() {
    let requirements: Value = serde_json::from_str(REQUIREMENTS).unwrap();
    let registered: BTreeSet<String> = security_fixtures()
        .iter()
        .map(|fixture| fixture["wire"].as_str().unwrap().to_owned())
        .collect();
    let root = uob_ocpp_fixtures::corpus_root();
    let mut cited = BTreeSet::new();
    let mut listed = BTreeSet::new();

    for assertion in requirements["assertions"].as_array().unwrap() {
        assert!(!assertion["assertion"].as_str().unwrap().is_empty());
        assert!(
            !assertion["behavioural_evidence_pending"]
                .as_array()
                .unwrap()
                .is_empty()
        );
        for id in assertion["requirements"].as_array().unwrap() {
            let id = id.as_str().unwrap();
            if id.contains(".FR.") {
                assert!(listed.insert(id.to_owned()), "{id} is asserted twice");
            }
        }
        for evidence in assertion["evidence"]
            .as_str()
            .unwrap()
            .split(';')
            .filter(|evidence| !evidence.is_empty())
        {
            assert!(root.join(evidence).is_file(), "missing evidence {evidence}");
            cited.insert(evidence.to_owned());
        }
    }

    assert!(
        registered.is_subset(&cited),
        "{:?}",
        registered.difference(&cited)
    );
    assert!(cited.contains("wire/1.6/security-negative-cases.json"));
    for id in [
        "A04.FR.01",
        "A04.FR.02",
        "A04.FR.03",
        "N01.FR.12",
        "L01.FR.28",
        "L01.FR.29",
    ] {
        assert!(listed.contains(id), "{id} is not asserted");
    }
    // The excluded neighbours stay named, so nobody reads this corpus as covering them.
    let not_covered = requirements["not_covered"].to_string();
    for neighbour in [
        "A01",
        "#345",
        "#346",
        "#129",
        "SignChargePointCertificate",
        "#128",
    ] {
        assert!(not_covered.contains(neighbour), "{neighbour}");
    }
}
