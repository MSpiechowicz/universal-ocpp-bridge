use uob_application::ChargerObservation;
use uob_contracts::{NativeProtocolReference, ProtocolEdition};
use uob_protocol_adapter::{DecodeErrorKind, OcppErrorCode, protocol_for_subprotocol, v16, v201};

const OCPP16_BOOT: &[u8] =
    include_bytes!("../../../tests/ocpp-fixtures/corpus/wire/1.6/boot-notification.json");
const OCPP16_HEARTBEAT: &[u8] =
    include_bytes!("../../../tests/ocpp-fixtures/corpus/wire/1.6/heartbeat.json");
const OCPP16_TRANSACTION: &[u8] =
    include_bytes!("../../../tests/ocpp-fixtures/corpus/wire/1.6/start-transaction.json");
const OCPP201_BOOT: &[u8] =
    include_bytes!("../../../tests/ocpp-fixtures/corpus/wire/2.0.1/boot-notification.json");
const OCPP201_HEARTBEAT: &[u8] =
    include_bytes!("../../../tests/ocpp-fixtures/corpus/wire/2.0.1/heartbeat.json");
const OCPP201_TRANSACTION: &[u8] =
    include_bytes!("../../../tests/ocpp-fixtures/corpus/wire/2.0.1/transaction-started.json");

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
        ChargerObservation::TransactionStarted(ref started)
            if started.native_transaction_id.as_deref() == Some("independent-transaction-001")
                && started.native_resource == NativeProtocolReference::Ocpp201 {
                    evse_id: 1,
                    connector_id: Some(1),
                }
    ));
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
