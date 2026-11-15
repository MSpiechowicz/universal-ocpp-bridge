use std::collections::BTreeSet;

use jsonschema::Draft;
use serde_json::{Value, json};

#[path = "monitoring201/rules.rs"]
mod rules;
use rules::{action, contextual_refusal, payload, single_frame_refusal};

const NEGATIVE: &str = include_str!("../corpus/wire/2.0.1/monitoring-negative-cases.json");
const REGISTRY: &str = include_str!("../corpus/fixtures.json");
const REQUIREMENTS: &str = include_str!("../corpus/monitoring201-requirements.json");
const PREFIX: &str = "wire.ocpp201.monitoring-";
/// Native-invalid `NotifyEvent` frames that the CSMS nevertheless accepts, with the native rule
/// each one breaks: a refusal would only make the charger retry the queued event forever.
const TOLERATED: [(&str, &str); 2] = [
    ("event-no-monitor-id", "event-monitor-id"),
    ("event-control-characters", "control-chars"),
];

fn schema(name: &str) -> jsonschema::Validator {
    let root = uob_ocpp_fixtures::corpus_root()
        .join("schemas/2.0.1")
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

fn monitoring_fixtures() -> Vec<Value> {
    let registry: Value = serde_json::from_str(REGISTRY).unwrap();
    registry["fixtures"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|fixture| fixture["id"].as_str().unwrap().starts_with(PREFIX))
        .cloned()
        .collect()
}

fn fixture_frame(name: &str) -> Value {
    wire(&format!("wire/2.0.1/monitoring-{name}.json"))
}

#[test]
fn every_monitoring_fixture_is_valid_against_its_pinned_schema() {
    let fixtures = monitoring_fixtures();
    assert_eq!(fixtures.len(), 58);
    for fixture in &fixtures {
        let id = fixture["id"].as_str().unwrap();
        let frame = wire(fixture["wire"].as_str().unwrap());
        let validator = schema(
            fixture["schema"]
                .as_str()
                .unwrap()
                .trim_start_matches("schemas/2.0.1/")
                .trim_end_matches(".json"),
        );
        assert!(validator.is_valid(payload(&frame)), "{id}");
        assert_eq!(frame[0], fixture["message_type"], "{id}");
        // No positive fixture trips a native single-frame rule, except the tolerated frames.
        let tolerated = TOLERATED
            .iter()
            .find(|(name, _)| id == format!("{PREFIX}{name}"));
        match tolerated {
            None => assert!(single_frame_refusal(&frame).is_empty(), "{id}"),
            Some((_, rule)) => {
                let broken = single_frame_refusal(&frame);
                assert!(
                    !broken.is_empty() && broken.iter().all(|found| found == rule),
                    "{id}"
                );
            }
        }
    }
}

#[test]
fn tolerated_event_frames_are_native_invalid_yet_registered_as_accepted_fixtures() {
    for (name, rule) in TOLERATED {
        let frame = fixture_frame(name);
        // The charger broke a native rule, the schema floor holds, and the CSMS still answers.
        assert_eq!(action(&frame), "NotifyEvent", "{name}");
        assert!(
            schema("NotifyEventRequest").is_valid(payload(&frame)),
            "{name}"
        );
        assert!(single_frame_refusal(&frame).contains(&rule), "{name}");
    }

    // Neither rule is a refusal in the negative-case file any more.
    let cases: Value = serde_json::from_str(NEGATIVE).unwrap();
    for case in cases["cases"].as_array().unwrap() {
        let notify_event = case["wire"][2] == "NotifyEvent";
        assert!(
            case["rule"] != "event-monitor-id"
                && !(notify_event && case["rule"] == "control-chars"),
            "{} is tolerated, not refused",
            case["id"]
        );
    }
}

#[test]
fn positive_request_reply_pairs_satisfy_the_contextual_rules() {
    for (request, reply) in [
        ("set-all-types", "set-reply-accepted"),
        ("set-replace-id", "set-reply-replaced"),
        ("set-minimal", "set-reply-minimal"),
        ("set-mixed", "set-reply-mixed"),
        ("set-delta-non-numeric", "set-reply-delta-non-numeric"),
    ] {
        let (request, reply) = (fixture_frame(request), fixture_frame(reply));
        for rule in ["set-count", "set-identity", "set-id-echo"] {
            assert!(
                !contextual_refusal(rule, std::slice::from_ref(&request), &reply),
                "{rule}"
            );
        }
    }
    // Only an Accepted result needs an id; refusals carry none.
    let mixed = fixture_frame("set-reply-mixed");
    assert!(!contextual_refusal("set-accepted-id", &[], &mixed));
    for (request, reply) in [
        ("clear-single", "clear-reply-accepted"),
        ("clear-single", "clear-reply-not-found"),
        ("clear-single", "clear-reply-rejected"),
        ("clear-multi", "clear-reply-multi-mixed"),
    ] {
        assert!(!contextual_refusal(
            "clear-ids",
            &[fixture_frame(request)],
            &fixture_frame(reply)
        ));
    }
    let request = fixture_frame("get-report-component-variable");
    let first = fixture_frame("report-fragment-0");
    let second = fixture_frame("report-fragment-1");
    let third = fixture_frame("report-fragment-2");
    for (context, part) in [
        (vec![request.clone()], &first),
        (vec![request.clone(), first.clone()], &second),
        (vec![request.clone(), first.clone(), second.clone()], &third),
    ] {
        assert!(!contextual_refusal("fragment-sequence", &context, part));
        assert!(!contextual_refusal("report-request-id", &context, part));
        assert!(!contextual_refusal("report-filter", &context, part));
    }
    let burst: Vec<Value> = ["event-burst-0", "event-burst-1", "event-burst-2"]
        .into_iter()
        .map(fixture_frame)
        .collect();
    for (index, part) in burst.iter().enumerate() {
        assert!(!contextual_refusal(
            "fragment-sequence",
            &burst[..index],
            part
        ));
    }
    // The criteria-filtered report keeps the Delta monitor out of a Threshold-only request.
    let thresholds = fixture_frame("get-report-criteria");
    let mut threshold_only = fixture_frame("report-single");
    threshold_only[3]["requestId"] = json!(7002);
    threshold_only[3]["monitor"][1]["variableMonitoring"]
        .as_array_mut()
        .unwrap()
        .remove(0);
    assert!(!contextual_refusal(
        "report-criteria",
        &[thresholds],
        &threshold_only
    ));
}

#[test]
fn negative_cases_separate_schema_floor_from_native_semantics() {
    let cases: Value = serde_json::from_str(NEGATIVE).unwrap();
    let cases = cases["cases"].as_array().unwrap();
    assert!(cases.len() >= 100);
    let mut seen = BTreeSet::new();
    for case in cases {
        let id = case["id"].as_str().unwrap();
        assert!(seen.insert(id), "duplicate negative case {id}");
        let frame = &case["wire"];
        // CALL frames name their action; CALLRESULT cases name their response schema.
        let name = match frame[0].as_u64() {
            Some(2) => format!("{}Request", action(frame)),
            Some(3) => case["schema"].as_str().unwrap().to_owned(),
            _ => panic!("{id} has no OCPP-J message type"),
        };
        assert_eq!(
            schema(&name).is_valid(payload(frame)),
            case["schema_valid"].as_bool().unwrap(),
            "{id}"
        );
        // No case is accepted natively.
        assert!(!case["native_valid"].as_bool().unwrap(), "{id}");
        let rule = case["rule"].as_str().unwrap();
        if rule == "schema" {
            assert!(!case["schema_valid"].as_bool().unwrap(), "{id}");
            continue;
        }
        // A native refusal above a valid schema is demonstrated by the named rule.
        assert!(case["schema_valid"].as_bool().unwrap(), "{id}");
        let context: Vec<Value> = case
            .get("context")
            .map(|frames| frames.as_array().unwrap().clone())
            .unwrap_or_default();
        let refused = match rule {
            "severity-range" | "int32" | "control-chars" | "delta-negative" => {
                single_frame_refusal(frame).contains(&rule)
            }
            other => contextual_refusal(other, &context, frame),
        };
        assert!(refused, "{id} is not refused by rule {rule}");
    }
}

#[test]
fn enumerations_and_optional_fields_match_the_pinned_schemas() {
    let set = schema("SetVariableMonitoringRequest");
    let item = |kind: &str| {
        json!({"setMonitoringData":[{"value":1,"type":kind,"severity":5,
        "component":{"name":"EVSE"},"variable":{"name":"Power"}}]})
    };
    for kind in [
        "UpperThreshold",
        "LowerThreshold",
        "Delta",
        "Periodic",
        "PeriodicClockAligned",
    ] {
        assert!(set.is_valid(&item(kind)), "{kind}");
    }
    assert!(!set.is_valid(&item("TargetDelta")));

    let reply = schema("SetVariableMonitoringResponse");
    let result = |status: &str| {
        json!({"setMonitoringResult":[{"status":status,"type":"Delta","severity":3,
        "component":{"name":"EVSE"},"variable":{"name":"Power"}}]})
    };
    for status in [
        "Accepted",
        "UnknownComponent",
        "UnknownVariable",
        "UnsupportedMonitorType",
        "Rejected",
        "Duplicate",
    ] {
        // The schema never requires the id; native rules require it for Accepted.
        assert!(reply.is_valid(&result(status)), "{status}");
    }
    assert!(!reply.is_valid(&result("NotFound")));

    let clear = schema("ClearVariableMonitoringResponse");
    for status in ["Accepted", "Rejected", "NotFound"] {
        assert!(clear.is_valid(&json!({"clearMonitoringResult":[{"status":status,"id":1}]})));
    }
    let generic = ["SetMonitoringBaseResponse", "GetMonitoringReportResponse"];
    for name in generic {
        let validator = schema(name);
        for status in ["Accepted", "Rejected", "NotSupported", "EmptyResultSet"] {
            assert!(
                validator.is_valid(&json!({ "status": status })),
                "{name} {status}"
            );
        }
    }
    let level = schema("SetMonitoringLevelResponse");
    assert!(level.is_valid(&json!({"status":"Accepted"})));
    assert!(level.is_valid(&json!({"status":"Rejected"})));
    assert!(!level.is_valid(&json!({"status":"NotSupported"})));

    let base = schema("SetMonitoringBaseRequest");
    for value in ["All", "FactoryDefault", "HardWiredOnly"] {
        assert!(base.is_valid(&json!({ "monitoringBase": value })));
    }
    let report = schema("GetMonitoringReportRequest");
    assert!(report.is_valid(&json!({"requestId":1,"monitoringCriteria":["ThresholdMonitoring","DeltaMonitoring","PeriodicMonitoring"]})));

    let event = schema("NotifyEventRequest");
    for trigger in ["Alerting", "Delta", "Periodic"] {
        for notification in [
            "HardWiredNotification",
            "HardWiredMonitor",
            "PreconfiguredMonitor",
            "CustomMonitor",
        ] {
            assert!(event.is_valid(&json!({"generatedAt":"2026-11-10T10:00:00Z","seqNo":0,"eventData":[{
                "eventId":1,"timestamp":"2026-11-10T10:00:00Z","trigger":trigger,"actualValue":"",
                "eventNotificationType":notification,"component":{"name":"EVSE"},"variable":{"name":"Power"}}]})));
        }
    }
    for name in ["NotifyEventResponse", "NotifyMonitoringReportResponse"] {
        let empty = schema(name);
        assert!(empty.is_valid(&json!({})));
        assert!(!empty.is_valid(&json!({"status":"Accepted"})));
    }
}

#[test]
fn requirements_file_covers_every_monitoring_requirement_with_existing_evidence() {
    let requirements: Value = serde_json::from_str(REQUIREMENTS).unwrap();
    let fixtures: BTreeSet<String> = monitoring_fixtures()
        .iter()
        .map(|fixture| fixture["wire"].as_str().unwrap().to_owned())
        .collect();

    let mut listed = BTreeSet::new();
    let mut cited = BTreeSet::new();
    let root = uob_ocpp_fixtures::corpus_root();
    for assertion in requirements["assertions"].as_array().unwrap() {
        assert!(!assertion["assertion"].as_str().unwrap().is_empty());
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
            .filter(|e| !e.is_empty())
        {
            assert!(root.join(evidence).is_file(), "missing evidence {evidence}");
            cited.insert(evidence.to_owned());
        }
    }
    for exclusion in requirements["excluded_requirements"].as_array().unwrap() {
        assert!(!exclusion["reason"].as_str().unwrap().is_empty());
        for id in exclusion["requirements"].as_array().unwrap() {
            assert!(
                listed.insert(id.as_str().unwrap().to_owned()),
                "{id} is listed twice"
            );
        }
    }

    // The spec numbers N02.FR.15, N07.FR.01, N07.FR.08, N07.FR.09 and N08.FR.01 do not exist.
    let mut expected = BTreeSet::new();
    for (prefix, numbers) in [
        ("N02", (1..=21).filter(|n| *n != 15).collect::<Vec<_>>()),
        ("N03", (1..=5).collect()),
        ("N04", (1..=20).collect()),
        ("N05", (1..=3).collect()),
        ("N06", (1..=7).collect()),
        (
            "N07",
            vec![2, 3, 4, 5, 6, 7, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19],
        ),
        ("N08", (2..=7).collect()),
    ] {
        expected.extend(numbers.into_iter().map(|n| format!("{prefix}.FR.{n:02}")));
    }
    assert_eq!(listed, expected);

    // Every positive fixture and the negative-case file back at least one assertion.
    for wire in &fixtures {
        assert!(
            cited.contains(wire),
            "{wire} is not cited by any requirement"
        );
    }
    assert!(cited.contains("wire/2.0.1/monitoring-negative-cases.json"));
}

#[test]
fn coverage_row_lists_exactly_the_registered_monitoring_fixtures() {
    let coverage: Value = serde_json::from_slice(
        &std::fs::read(uob_ocpp_fixtures::corpus_root().join("coverage.json")).unwrap(),
    )
    .unwrap();
    let row = coverage["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["requirement_id"] == "ocpp201.monitoring-security.monitoring")
        .unwrap();
    let listed: BTreeSet<&str> = row["fixture_ids"]
        .as_array()
        .unwrap()
        .iter()
        .map(|id| id.as_str().unwrap())
        .collect();
    let registered = monitoring_fixtures();
    let registered: BTreeSet<&str> = registered
        .iter()
        .map(|f| f["id"].as_str().unwrap())
        .collect();
    assert_eq!(listed, registered);
}
