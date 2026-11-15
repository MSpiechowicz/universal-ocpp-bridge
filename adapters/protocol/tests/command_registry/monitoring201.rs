use super::*;

const ACTIONS: [&str; 5] = [
    "SetVariableMonitoring",
    "ClearVariableMonitoring",
    "GetMonitoringReport",
    "SetMonitoringBase",
    "SetMonitoringLevel",
];
const NEGATIVE: &str = include_str!(
    "../../../../tests/ocpp-fixtures/corpus/wire/2.0.1/monitoring-negative-cases.json"
);

fn corpus(name: &str) -> Value {
    let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/ocpp-fixtures/corpus/wire/2.0.1")
        .join(format!("monitoring-{name}.json"));
    let frame: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    frame[3].clone()
}

fn operation(action: &str, payload: Value) -> PrivilegedOcppOperation<Value> {
    PrivilegedOcppOperation {
        protocol: ProtocolEdition::Ocpp201,
        action: ProtocolActionName::new(action).unwrap(),
        payload_schema: PayloadSchemaId::new(format!("urn:OCPP:Cp:2:2020:3:{action}Request"))
            .unwrap(),
        payload,
    }
}

fn advertise(capabilities: &mut uob_contracts::ResourceCapabilities) {
    for action in ACTIONS {
        capabilities.operations.push(SupportedOperation {
            operation: Operation::ProtocolAction {
                protocol: ProtocolEdition::Ocpp201,
                action: action.to_owned(),
            },
            parameters: vec![],
        });
    }
}

fn refused(station: &uob_contracts::ResourceRef, action: &str, payload: Value) {
    let shown = format!("{action} {payload}");
    assert_eq!(
        validate_privileged_operation(station, &operation(action, payload)),
        Err(CommandErrorCode::InvalidParameters),
        "{shown}"
    );
}

fn set(items: Vec<Value>) -> Value {
    json!({ "setMonitoringData": Value::Array(items) })
}

fn item(kind: &str, severity: i64, component: Value, variable: Value) -> Value {
    let mut item = json!({"value": 5, "type": kind, "severity": severity});
    item["component"] = component;
    item["variable"] = variable;

    item
}

#[test]
fn monitoring_discovery_is_station_root_only_and_needs_explicit_capability() {
    let mut state = snapshot(ProtocolEdition::Ocpp201);
    advertise(&mut state.resources[0].capabilities);
    assert!(command_schemas(&state).is_empty());

    advertise(&mut state.capabilities);
    let descriptors = command_schemas(&state);
    assert_eq!(descriptors.len(), 5);
    for (descriptor, action) in descriptors.iter().zip(ACTIONS) {
        assert_eq!(descriptor.resource, state.station);
        assert_eq!(descriptor.protocol, ProtocolEdition::Ocpp201);
        assert_eq!(descriptor.action, action);
        assert_eq!(
            descriptor.payload_schema,
            format!("urn:OCPP:Cp:2:2020:3:{action}Request")
        );
    }

    let fields = |index: usize| -> Vec<String> {
        serde_json::to_value(&descriptors[index]).unwrap()["fields"]
            .as_array()
            .unwrap()
            .iter()
            .map(|field| field["name"].as_str().unwrap().to_owned())
            .collect()
    };
    assert!(fields(0).contains(&"setMonitoringData[].severity".to_owned()));
    assert_eq!(fields(1), ["id[]"]);
    assert!(fields(2).contains(&"monitoringCriteria[]".to_owned()));
    assert_eq!(fields(3), ["monitoringBase"]);
    assert_eq!(fields(4), ["severity"]);
    let base = serde_json::to_value(&descriptors[3]).unwrap();
    assert_eq!(
        base["fields"][0]["enum_values"],
        json!(["All", "FactoryDefault", "HardWiredOnly"])
    );

    state.connectivity = Connectivity::Disconnected;
    assert!(command_schemas(&state).is_empty());
}

#[test]
fn every_positive_corpus_request_validates_on_the_station_root() {
    let state = snapshot(ProtocolEdition::Ocpp201);
    let cases = [
        ("SetVariableMonitoring", "set-all-types"),
        ("SetVariableMonitoring", "set-delta-non-numeric"),
        ("SetVariableMonitoring", "set-minimal"),
        ("SetVariableMonitoring", "set-replace-id"),
        ("ClearVariableMonitoring", "clear-single"),
        ("ClearVariableMonitoring", "clear-multi"),
        ("GetMonitoringReport", "get-report-both"),
        ("GetMonitoringReport", "get-report-component-variable"),
        ("GetMonitoringReport", "get-report-criteria-all"),
        ("GetMonitoringReport", "get-report-criteria"),
        ("GetMonitoringReport", "get-report-no-filter"),
        ("GetMonitoringReport", "get-report-request-id-zero"),
        ("SetMonitoringBase", "base-all"),
        ("SetMonitoringBase", "base-factory-default"),
        ("SetMonitoringBase", "base-hard-wired-only"),
        ("SetMonitoringLevel", "level-0"),
        ("SetMonitoringLevel", "level-4"),
        ("SetMonitoringLevel", "level-9"),
    ];
    for (action, name) in cases {
        assert_eq!(
            validate_privileged_operation(&state.station, &operation(action, corpus(name))),
            Ok(()),
            "{name}"
        );
    }

    // The corpus `set-mixed` request repeats one monitor to provoke a native `Duplicate` status.
    // The bridge refuses to send a request that names the same monitor twice (N04.FR.10).
    assert_eq!(
        validate_privileged_operation(
            &state.station,
            &operation("SetVariableMonitoring", corpus("set-mixed"))
        ),
        Err(CommandErrorCode::InvalidParameters)
    );

    // The NativeProtocolReference of an EVSE-0 root is the same station scope.
    let mut evse_root = state.station.clone();
    evse_root.native_protocol_reference = Some(NativeProtocolReference::Ocpp201 {
        evse_id: 0,
        connector_id: None,
    });
    assert_eq!(
        validate_privileged_operation(
            &evse_root,
            &operation("SetMonitoringLevel", corpus("level-4"))
        ),
        Ok(())
    );
}

#[test]
fn every_corpus_negative_request_is_refused_before_transmission() {
    let state = snapshot(ProtocolEdition::Ocpp201);
    let cases: Value = serde_json::from_str(NEGATIVE).unwrap();
    let mut checked = 0;
    for case in cases["cases"].as_array().unwrap() {
        let wire = &case["wire"];
        let Some(action) = wire[2].as_str().filter(|action| ACTIONS.contains(action)) else {
            continue;
        };
        if wire[0] != 2 {
            continue;
        }
        assert_eq!(
            validate_privileged_operation(&state.station, &operation(action, wire[3].clone())),
            Err(CommandErrorCode::InvalidParameters),
            "{}",
            case["id"]
        );
        checked += 1;
    }
    assert!(checked >= 40, "{checked}");
}

/// `EVSE 1 / Power`, the variable most of these requests monitor.
fn power(kind: &str, severity: i64) -> Value {
    item(
        kind,
        severity,
        json!({"name": "EVSE", "evse": {"id": 1}}),
        json!({"name": "Power"}),
    )
}

fn refuse_set(items: Vec<Value>) {
    let state = snapshot(ProtocolEdition::Ocpp201);
    refused(&state.station, "SetVariableMonitoring", set(items));
}

fn allow_set(items: Vec<Value>) {
    let state = snapshot(ProtocolEdition::Ocpp201);
    assert_eq!(
        validate_privileged_operation(
            &state.station,
            &operation("SetVariableMonitoring", set(items))
        ),
        Ok(())
    );
}

#[test]
fn set_requests_refuse_severities_values_and_deltas_the_charger_would_not_honor() {
    // A native severity is 0 through 9.
    for severity in [-1, 10, 255, 256] {
        refuse_set(vec![power("Delta", severity)]);
    }
    // Delta may not be negative (N04.FR.14); other types may.
    let mut negative = power("Delta", 5);
    negative["value"] = json!(-0.5);
    refuse_set(vec![negative.clone()]);
    negative["type"] = json!("LowerThreshold");
    allow_set(vec![negative]);
    // The value must be exactly representable.
    for text in [
        "1e100",
        "1.0e-100",
        "1e999",
        "0.00000000000000000000000000000000000000000000000000000000000000000000001",
    ] {
        let mut huge = power("UpperThreshold", 5);
        huge["value"] = serde_json::from_str(text).unwrap();
        refuse_set(vec![huge]);
    }
}

#[test]
fn set_requests_refuse_a_monitor_or_an_id_named_twice() {
    // One monitor per variable, type and severity, compared case-insensitively (N04.FR.10).
    refuse_set(vec![
        power("UpperThreshold", 5),
        item(
            "UpperThreshold",
            5,
            json!({"name": "evse", "evse": {"id": 1}}),
            json!({"name": "POWER"}),
        ),
    ]);
    // Any difference in severity, type or component scope makes a different monitor.
    allow_set(vec![
        power("UpperThreshold", 5),
        power("UpperThreshold", 6),
        power("LowerThreshold", 5),
        item(
            "UpperThreshold",
            5,
            json!({"name": "EVSE", "evse": {"id": 2}}),
            json!({"name": "Power"}),
        ),
    ]);
    // A replacement ID appears once.
    let mut first = power("UpperThreshold", 5);
    let mut second = power("LowerThreshold", 5);
    first["id"] = json!(7);
    second["id"] = json!(7);
    refuse_set(vec![first, second]);
}

#[test]
fn set_requests_never_forward_custom_data_control_characters_or_wide_integers() {
    let mut custom = power("Delta", 5);
    custom["customData"] = json!({"vendorId": "v"});
    refuse_set(vec![custom]);
    let mut nested = power("Delta", 5);
    nested["component"]["customData"] = json!({"vendorId": "v"});
    refuse_set(vec![nested]);

    let mut control = power("Delta", 5);
    control["component"]["name"] = json!("EV\u{0}SE");
    refuse_set(vec![control]);
    let mut delete = power("Delta", 5);
    delete["variable"]["name"] = json!("Po\u{7f}wer");
    refuse_set(vec![delete]);

    // Integers stay within 32 bits; EVSE and connector identities are positive.
    let mut big = power("Delta", 5);
    big["id"] = json!(2_147_483_648_i64);
    refuse_set(vec![big]);
    let mut evse = power("Delta", 5);
    evse["component"]["evse"]["id"] = json!(0);
    refuse_set(vec![evse]);
    let mut connector = power("Delta", 5);
    connector["component"]["evse"]["connectorId"] = json!(0);
    refuse_set(vec![connector]);
}

#[test]
fn clear_and_report_requests_refuse_duplicates_overflow_and_scope() {
    let state = snapshot(ProtocolEdition::Ocpp201);
    let station = &state.station;
    refused(station, "ClearVariableMonitoring", json!({"id": [4, 5, 4]}));
    refused(
        station,
        "ClearVariableMonitoring",
        json!({"id": [4_294_967_296_i64]}),
    );
    refused(
        station,
        "ClearVariableMonitoring",
        json!({"id": [1], "customData": {"vendorId": "v"}}),
    );
    refused(
        station,
        "GetMonitoringReport",
        json!({"requestId": 2_147_483_648_i64}),
    );
    refused(
        station,
        "GetMonitoringReport",
        json!({"requestId": 1, "monitoringCriteria": []}),
    );
    refused(
        station,
        "GetMonitoringReport",
        json!({"requestId": 1, "componentVariable": [{"component": {"name": "EVSE", "evse": {"id": 0}}}]}),
    );
    refused(
        station,
        "GetMonitoringReport",
        json!({"requestId": 1, "componentVariable": [{"component": {"name": "A\u{1b}"}}]}),
    );
    refused(
        station,
        "SetMonitoringBase",
        json!({"monitoringBase": "None"}),
    );
    refused(station, "SetMonitoringLevel", json!({"severity": 10}));
    refused(
        station,
        "SetMonitoringLevel",
        json!({"severity": 3, "customData": {"vendorId": "v"}}),
    );

    // Monitors belong to the charger's device model: never an EVSE or connector resource.
    for action in ACTIONS {
        let payload = match action {
            "SetVariableMonitoring" => corpus("set-minimal"),
            "ClearVariableMonitoring" => corpus("clear-single"),
            "GetMonitoringReport" => corpus("get-report-no-filter"),
            "SetMonitoringBase" => corpus("base-all"),
            _ => corpus("level-4"),
        };
        refused(&state.resources[0].resource, action, payload.clone());
        let mut native = state.station.clone();
        native.native_protocol_reference = Some(NativeProtocolReference::Ocpp201 {
            evse_id: 1,
            connector_id: None,
        });
        refused(&native, action, payload);
    }
}

#[test]
fn monitoring_requests_need_the_exact_native_schema_and_edition() {
    let state = snapshot(ProtocolEdition::Ocpp201);
    let mut foreign = operation("SetMonitoringLevel", corpus("level-4"));
    foreign.payload_schema = PayloadSchemaId::new("urn:uob:ocpp201:SetMonitoringLevel:1").unwrap();
    assert_eq!(
        validate_privileged_operation(&state.station, &foreign),
        Err(CommandErrorCode::InvalidParameters)
    );
    let mut wrong_request = operation("SetMonitoringLevel", corpus("level-4"));
    wrong_request.payload_schema =
        PayloadSchemaId::new("urn:OCPP:Cp:2:2020:3:SetMonitoringBaseRequest").unwrap();
    assert_eq!(
        validate_privileged_operation(&state.station, &wrong_request),
        Err(CommandErrorCode::InvalidParameters)
    );
    // OCPP 1.6 has none of these actions.
    let mut edition = operation("SetMonitoringLevel", corpus("level-4"));
    edition.protocol = ProtocolEdition::Ocpp16j;
    assert_eq!(
        validate_privileged_operation(&state.station, &edition),
        Err(CommandErrorCode::UnsupportedOperation)
    );
    // A charger-initiated action is not a command.
    for action in ["NotifyMonitoringReport", "NotifyEvent"] {
        assert_eq!(
            validate_privileged_operation(&state.station, &operation(action, json!({}))),
            Err(CommandErrorCode::UnsupportedOperation)
        );
    }
}
