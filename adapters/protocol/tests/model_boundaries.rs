use uob_application::{
    ChargerObservation, TransactionApplyError, TransactionApplyOutcome, TransactionEventKind,
    apply_measurements, apply_transaction_event,
};
use uob_contracts::{
    NativeProtocolReference, ProtocolEdition, StationSnapshot, TransactionState, UtcTimestamp,
};
use uob_protocol_adapter::{DecodeErrorKind, OcppErrorCode, protocol_for_subprotocol, v16, v201};

const OCPP16_BOOT: &[u8] =
    include_bytes!("../../../tests/ocpp-fixtures/corpus/wire/1.6/boot-notification.json");
const OCPP16_HEARTBEAT: &[u8] =
    include_bytes!("../../../tests/ocpp-fixtures/corpus/wire/1.6/heartbeat.json");
const OCPP16_TRANSACTION: &[u8] =
    include_bytes!("../../../tests/ocpp-fixtures/corpus/wire/1.6/start-transaction.json");
const OCPP16_METER_VALUES: &[u8] =
    include_bytes!("../../../tests/ocpp-fixtures/corpus/wire/1.6/meter-values.json");
const OCPP201_BOOT: &[u8] =
    include_bytes!("../../../tests/ocpp-fixtures/corpus/wire/2.0.1/boot-notification.json");
const OCPP201_HEARTBEAT: &[u8] =
    include_bytes!("../../../tests/ocpp-fixtures/corpus/wire/2.0.1/heartbeat.json");
const OCPP201_TRANSACTION: &[u8] =
    include_bytes!("../../../tests/ocpp-fixtures/corpus/wire/2.0.1/transaction-started.json");
const OCPP201_METER_VALUES: &[u8] =
    include_bytes!("../../../tests/ocpp-fixtures/corpus/wire/2.0.1/transaction-meter-values.json");
const OCPP201_TRANSACTION_UPDATED: &[u8] =
    include_bytes!("../../../tests/ocpp-fixtures/corpus/wire/2.0.1/transaction-updated.json");
const OCPP201_TRANSACTION_ENDED: &[u8] =
    include_bytes!("../../../tests/ocpp-fixtures/corpus/wire/2.0.1/transaction-ended.json");

#[test]
fn independent_ocpp16_fixtures_map_without_leaking_model_types() {
    let boot = v16::decode_call(OCPP16_BOOT).expect("OCPP 1.6 boot fixture");
    assert_eq!(boot.action.as_str(), "BootNotification");
    assert!(matches!(
        boot.observation,
        ChargerObservation::Registration(ref registration)
            if registration.protocol == ProtocolEdition::Ocpp16j
                && registration.vendor == "Independent Vendor"
                && registration.model == "IF-16"
                && registration.boot_reason.is_none()
    ));

    assert!(matches!(
        v16::decode_call(OCPP16_HEARTBEAT)
            .expect("OCPP 1.6 heartbeat")
            .observation,
        ChargerObservation::Heartbeat {
            protocol: ProtocolEdition::Ocpp16j
        }
    ));

    let transaction = v16::decode_call(OCPP16_TRANSACTION).expect("OCPP 1.6 transaction");
    assert!(matches!(
        transaction.observation,
        ChargerObservation::TransactionStarted(ref started)
            if started.native_transaction_id.is_none()
                && started.native_resource == NativeProtocolReference::Ocpp16 { connector_id: 1 }
    ));
}

#[test]
fn independent_ocpp201_fixtures_preserve_evse_and_transaction_identity() {
    let boot = v201::decode_call(OCPP201_BOOT).expect("OCPP 2.0.1 boot fixture");
    assert!(matches!(
        boot.observation,
        ChargerObservation::Registration(ref registration)
            if registration.protocol == ProtocolEdition::Ocpp201
                && registration.vendor == "Independent Fixture Vendor"
                && registration.model == "IF-201"
                && registration.boot_reason.as_deref() == Some("PowerUp")
    ));

    assert!(matches!(
        v201::decode_call(OCPP201_HEARTBEAT)
            .expect("OCPP 2.0.1 heartbeat")
            .observation,
        ChargerObservation::Heartbeat {
            protocol: ProtocolEdition::Ocpp201
        }
    ));

    let transaction = v201::decode_call(OCPP201_TRANSACTION).expect("OCPP 2.0.1 transaction");
    assert!(matches!(
        transaction.observation,
        ChargerObservation::TransactionEvent(ref event)
            if event.event == TransactionEventKind::Started
                && event.sequence_number == 0
                && event.trigger_reason == "Authorized"
                && event.native_transaction_id == "independent-transaction-001"
                && event.native_resource == NativeProtocolReference::Ocpp201 {
                    evse_id: 1,
                    connector_id: Some(1),
                }
    ));
}

#[test]
fn ocpp16_meter_values_preserve_exact_semantics_defaults_and_invalid_quality() {
    let call = v16::decode_call(OCPP16_METER_VALUES).expect("OCPP 1.6 metering fixture");
    let ChargerObservation::Measurements(observation) = call.observation else {
        panic!("expected measurements");
    };
    assert_eq!(observation.protocol, ProtocolEdition::Ocpp16j);
    assert_eq!(
        observation.native_resource,
        NativeProtocolReference::Ocpp16 { connector_id: 1 }
    );
    assert_eq!(observation.native_transaction_id.as_deref(), Some("42"));
    assert_eq!(observation.sequence_number, None);
    assert_eq!(observation.values.len(), 3);

    let exact = serde_json::to_value(&observation.values[0]).expect("canonical exact value");
    assert_eq!(exact["value"]["value"], "12345678901234567890.1234567");
    assert_eq!(
        exact["measurement"]["original_value"],
        "12345678901234567.8901234567"
    );
    assert_eq!(exact["measurement"]["original_unit"], "kWh");
    assert_eq!(
        exact["measurement"]["measurand"],
        "Energy.Active.Import.Register"
    );
    assert_eq!(exact["measurement"]["phase"], "L1-N");
    assert_eq!(exact["measurement"]["context"], "Sample.Periodic");
    assert_eq!(exact["measurement"]["location"], "Outlet");
    assert_eq!(
        exact["measurement"]["protocol_reference"]["connector_id"],
        1
    );

    let signed = serde_json::to_value(&observation.values[1]).expect("signed value evidence");
    assert!(signed.get("value").is_none());
    assert_eq!(signed["quality"]["level"], "uncertain");
    assert_eq!(signed["quality"]["reason"], "signed_data_unverified");
    assert_eq!(observation.signed_values, ["A1B2C3D4"]);

    let unavailable = serde_json::to_value(&observation.values[2]).expect("bad value evidence");
    assert!(unavailable.get("value").is_none());
    assert_eq!(unavailable["quality"]["level"], "bad");
    assert_eq!(unavailable["quality"]["reason"], "invalid_decimal");

    let defaults = br#"[2,"defaults","MeterValues",{"connectorId":0,"meterValue":[{"timestamp":"2026-09-01T00:06:00Z","sampledValue":[{"value":"7"}]}]}]"#;
    let call = v16::decode_call(defaults).expect("protocol defaults");
    let ChargerObservation::Measurements(defaults) = call.observation else {
        panic!("expected default measurement");
    };
    let encoded = serde_json::to_value(&defaults.values[0]).expect("default metadata");
    assert_eq!(encoded["measurement"]["original_unit"], "Wh");
    assert_eq!(encoded["measurement"]["context"], "Sample.Periodic");
    assert_eq!(encoded["measurement"]["location"], "Outlet");
}

#[test]
fn ocpp16_meter_values_update_state_that_survives_persistence_and_recovery() {
    let call = v16::decode_call(OCPP16_METER_VALUES).expect("metering fixture");
    let ChargerObservation::Measurements(observation) = call.observation else {
        panic!("expected measurements");
    };
    let mut snapshot: StationSnapshot = serde_json::from_slice(include_bytes!(
        "../../../crates/contracts/tests/fixtures/station-snapshot-ocpp16-v1.json"
    ))
    .expect("snapshot fixture");
    let observed_at: UtcTimestamp =
        serde_json::from_str("\"2026-09-01T00:07:00Z\"").expect("trusted observation time");

    apply_measurements(&mut snapshot, &observation, observed_at).expect("known connector");
    let stored = serde_json::to_vec(&snapshot).expect("persist snapshot");
    let mut recovered: StationSnapshot = serde_json::from_slice(&stored).expect("recover snapshot");
    let values = &recovered.resources[0].current_values;
    assert_eq!(values.len(), 4);
    assert_eq!(values[1].point_id, observation.values[0].point_id);
    assert_eq!(values[1].value, observation.values[0].value);
    assert_eq!(values[1].source_time, observation.values[0].source_time);
    assert_eq!(values[1].observed_at, observed_at);

    apply_measurements(&mut recovered, &observation, observed_at)
        .expect("replayed after reconnect");
    assert_eq!(recovered.resources[0].current_values.len(), 4);

    let station_level = br#"[2,"station-meter","MeterValues",{"connectorId":0,"meterValue":[{"timestamp":"2026-09-01T00:08:00Z","sampledValue":[{"value":"8"}]}]}]"#;
    let ChargerObservation::Measurements(station_level) = v16::decode_call(station_level)
        .expect("station meter")
        .observation
    else {
        panic!("expected station measurement");
    };
    apply_measurements(&mut recovered, &station_level, observed_at).expect("connector zero");
    assert_eq!(recovered.current_values.len(), 1);
}

#[test]
fn ocpp16_metering_rejects_invalid_payloads_and_unknown_connectors() {
    let empty = br#"[2,"meter","MeterValues",{"connectorId":1,"meterValue":[]}]"#;
    assert_eq!(
        v16::decode_call(empty)
            .expect_err("empty meter values")
            .kind(),
        DecodeErrorKind::InvalidPayload
    );
    let empty_samples = br#"[2,"meter","MeterValues",{"connectorId":1,"meterValue":[{"timestamp":"2026-09-01T00:00:00Z","sampledValue":[]}]}]"#;
    assert_eq!(
        v16::decode_call(empty_samples)
            .expect_err("empty samples")
            .kind(),
        DecodeErrorKind::InvalidPayload
    );
    let unknown_unit = br#"[2,"meter","MeterValues",{"connectorId":1,"meterValue":[{"timestamp":"2026-09-01T00:00:00Z","sampledValue":[{"value":"1","unit":"parsecs"}]}]}]"#;
    assert_eq!(
        v16::decode_call(unknown_unit)
            .expect_err("unknown unit")
            .kind(),
        DecodeErrorKind::InvalidPayload
    );

    let call = v16::decode_call(OCPP16_METER_VALUES).expect("metering fixture");
    let ChargerObservation::Measurements(mut observation) = call.observation else {
        panic!("expected measurements");
    };
    observation.native_resource = NativeProtocolReference::Ocpp16 { connector_id: 99 };
    let mut snapshot: StationSnapshot = serde_json::from_slice(include_bytes!(
        "../../../crates/contracts/tests/fixtures/station-snapshot-ocpp16-v1.json"
    ))
    .expect("snapshot fixture");
    let observed_at = serde_json::from_str("\"2026-09-01T00:07:00Z\"").expect("timestamp");
    assert!(apply_measurements(&mut snapshot, &observation, observed_at).is_err());
}

#[test]
fn ocpp201_meter_values_preserve_exact_semantics_and_native_identity() {
    let call = v201::decode_call(OCPP201_METER_VALUES).expect("OCPP 2.0.1 metering fixture");
    let ChargerObservation::TransactionEvent(event) = call.observation else {
        panic!("expected transaction event");
    };
    let observation = event.measurements.expect("embedded meter values");
    assert_eq!(observation.protocol, ProtocolEdition::Ocpp201);
    assert_eq!(
        observation.native_resource,
        NativeProtocolReference::Ocpp201 {
            evse_id: 2,
            connector_id: Some(1),
        }
    );
    assert_eq!(
        observation.native_transaction_id.as_deref(),
        Some("independent-transaction-001")
    );
    assert_eq!(observation.sequence_number, Some(7));
    assert_eq!(observation.values.len(), 1);
    let encoded = serde_json::to_value(&observation.values[0]).expect("canonical value");
    assert_eq!(encoded["value"]["value"], "12345678901234567.8901234567");
    assert_eq!(
        encoded["measurement"]["original_value"],
        "12345678901234567890.1234567"
    );
    assert_eq!(encoded["measurement"]["original_unit"], "Wh");
    assert_eq!(
        encoded["measurement"]["measurand"],
        "EnergyActiveImportRegister"
    );
    assert_eq!(encoded["measurement"]["phase"], "L1N");
    assert_eq!(encoded["measurement"]["context"], "SamplePeriodic");
    assert_eq!(encoded["measurement"]["location"], "Outlet");
    assert_eq!(encoded["measurement"]["protocol_reference"]["evse_id"], 2);
    assert_eq!(observation.signed_values.len(), 1);
    assert!(observation.signed_values[0].contains("fixture-public-key"));
}

#[test]
fn ocpp201_meter_values_update_state_that_survives_persistence_and_recovery() {
    let call = v201::decode_call(OCPP201_METER_VALUES).expect("metering fixture");
    let ChargerObservation::TransactionEvent(event) = call.observation else {
        panic!("expected transaction event");
    };
    let observation = event.measurements.expect("embedded meter values");
    let mut persisted: serde_json::Value = serde_json::from_slice(include_bytes!(
        "../../../crates/contracts/tests/fixtures/station-snapshot-ocpp16-v1.json"
    ))
    .expect("snapshot fixture");
    persisted["resources"][0]["resource"]["native_protocol_reference"] =
        serde_json::json!({"protocol":"ocpp201","evse_id":2,"connector_id":1});
    let mut snapshot: StationSnapshot =
        serde_json::from_value(persisted).expect("canonical snapshot");
    let observed_at: UtcTimestamp =
        serde_json::from_str("\"2026-09-01T00:05:03Z\"").expect("trusted observation time");

    apply_measurements(&mut snapshot, &observation, observed_at).expect("known EVSE");
    let stored = serde_json::to_vec(&snapshot).expect("persist snapshot");
    let recovered: StationSnapshot = serde_json::from_slice(&stored).expect("recover snapshot");
    let values = &recovered.resources[0].current_values;
    assert_eq!(values.len(), 2);
    assert_eq!(values[1].point_id, observation.values[0].point_id);
    assert_eq!(values[1].value, observation.values[0].value);
    assert_eq!(values[1].observed_at, observed_at);

    apply_measurements(&mut snapshot, &observation, observed_at).expect("replayed after reconnect");
    assert_eq!(snapshot.resources[0].current_values.len(), 2);
}

#[test]
fn ocpp201_transaction_lifecycle_is_durable_idempotent_and_ordered() {
    let started = v201::decode_call(OCPP201_TRANSACTION).expect("started fixture");
    let ChargerObservation::TransactionEvent(started) = started.observation else {
        panic!("expected started transaction event");
    };
    let ChargerObservation::TransactionEvent(updated) =
        v201::decode_call(OCPP201_TRANSACTION_UPDATED)
            .expect("updated event")
            .observation
    else {
        panic!("expected updated transaction event");
    };
    let ChargerObservation::TransactionEvent(ended) = v201::decode_call(OCPP201_TRANSACTION_ENDED)
        .expect("ended event")
        .observation
    else {
        panic!("expected ended transaction event");
    };
    let mut snapshot: StationSnapshot = serde_json::from_slice(include_bytes!(
        "../../../crates/contracts/tests/fixtures/station-snapshot-ocpp201-v1.json"
    ))
    .expect("snapshot fixture");
    let observed_at: UtcTimestamp =
        serde_json::from_str("\"2026-09-01T13:00:01Z\"").expect("observation time");

    assert_eq!(
        apply_transaction_event(&mut snapshot, &started, observed_at),
        Ok(TransactionApplyOutcome::Applied)
    );
    let recovered: StationSnapshot =
        serde_json::from_slice(&serde_json::to_vec(&snapshot).expect("persisted snapshot"))
            .expect("recovered snapshot");
    snapshot = recovered;
    assert_eq!(
        apply_transaction_event(&mut snapshot, &started, observed_at),
        Ok(TransactionApplyOutcome::Duplicate)
    );
    assert_eq!(
        apply_transaction_event(&mut snapshot, &ended, observed_at),
        Err(TransactionApplyError::OutOfOrder)
    );
    assert_eq!(
        apply_transaction_event(&mut snapshot, &updated, observed_at),
        Ok(TransactionApplyOutcome::Applied)
    );
    assert_eq!(snapshot.transactions[0].state, TransactionState::Active);
    assert_eq!(
        apply_transaction_event(&mut snapshot, &ended, observed_at),
        Ok(TransactionApplyOutcome::Applied)
    );
    assert_eq!(snapshot.transactions[0].state, TransactionState::Ended);
    assert_eq!(snapshot.transactions[0].ended_at, Some(ended.occurred_at));
    let state = snapshot.transactions[0]
        .protocol_state
        .as_ref()
        .expect("recovery evidence");
    assert_eq!(state.last_sequence_number, 2);
    assert_eq!(state.last_trigger_reason, "RemoteStop");
}

#[test]
fn ocpp201_transaction_lifecycle_rejects_invalid_and_conflicting_events() {
    let no_evse = br#"[2,"bad","TransactionEvent",{"eventType":"Started","timestamp":"2026-09-01T12:30:00Z","triggerReason":"Authorized","seqNo":0,"transactionInfo":{"transactionId":"tx"}}]"#;
    assert_eq!(
        v201::decode_call(no_evse)
            .expect_err("EVSE is required")
            .kind(),
        DecodeErrorKind::InvalidPayload
    );

    let started = v201::decode_call(OCPP201_TRANSACTION).expect("started fixture");
    let ChargerObservation::TransactionEvent(started) = started.observation else {
        panic!("expected transaction event");
    };
    let mut snapshot: StationSnapshot = serde_json::from_slice(include_bytes!(
        "../../../crates/contracts/tests/fixtures/station-snapshot-ocpp201-v1.json"
    ))
    .expect("snapshot fixture");
    let observed_at: UtcTimestamp =
        serde_json::from_str("\"2026-09-01T12:30:01Z\"").expect("observation time");
    apply_transaction_event(&mut snapshot, &started, observed_at).expect("initial start");
    let mut conflicting = started.clone();
    conflicting.trigger_reason = "RemoteStart".to_owned();
    assert_eq!(
        apply_transaction_event(&mut snapshot, &conflicting, observed_at),
        Err(TransactionApplyError::ConflictingReplay)
    );
}

#[test]
fn ocpp201_metering_rejects_invalid_resources_and_empty_samples() {
    let negative_evse = br#"[2,"meter","MeterValues",{"evseId":-1,"meterValue":[]}]"#;
    assert_eq!(
        v201::decode_call(negative_evse)
            .expect_err("negative EVSE")
            .kind(),
        DecodeErrorKind::InvalidPayload
    );
    let empty_samples = br#"[2,"meter","MeterValues",{"evseId":1,"meterValue":[{"timestamp":"2026-09-01T00:00:00Z","sampledValue":[]}]}]"#;
    assert_eq!(
        v201::decode_call(empty_samples)
            .expect_err("empty samples")
            .kind(),
        DecodeErrorKind::InvalidPayload
    );
}

#[test]
fn unsupported_and_invalid_input_map_to_explicit_call_errors() {
    let unsupported = br#"[2,"id","DataTransfer",{}]"#;
    let error = v16::decode_call(unsupported).expect_err("mapping is intentionally absent");
    assert_eq!(error.kind(), DecodeErrorKind::UnsupportedAction);
    assert_eq!(error.call_error().code, OcppErrorCode::NotImplemented);
    assert_eq!(error.call_error().protocol, ProtocolEdition::Ocpp16j);

    let invalid = br#"[2,"id","BootNotification",{"reason":"PowerUp"}]"#;
    let error = v201::decode_call(invalid).expect_err("required station identity is absent");
    assert_eq!(error.kind(), DecodeErrorKind::InvalidPayload);
    assert_eq!(
        error.call_error().code,
        OcppErrorCode::PropertyConstraintViolation
    );
    assert_eq!(error.call_error().protocol, ProtocolEdition::Ocpp201);
}

#[test]
fn negotiation_does_not_imply_ocpp21_or_soap_support() {
    assert_eq!(
        protocol_for_subprotocol("ocpp1.6"),
        Some(ProtocolEdition::Ocpp16j)
    );
    assert_eq!(
        protocol_for_subprotocol("ocpp2.0.1"),
        Some(ProtocolEdition::Ocpp201)
    );
    assert_eq!(protocol_for_subprotocol("ocpp2.1"), None);
    assert_eq!(protocol_for_subprotocol("soap"), None);
}
