use serde_json::{Value, json};
use uob_application::ChargerObservation;
use uob_contracts::TriggerMessageClass;
use uob_protocol_adapter::{DecodeErrorKind, v16::decode_call};

fn frame(action: &str, payload: &Value) -> Vec<u8> {
    serde_json::to_vec(&json!([2, "notification-1", action, payload])).unwrap()
}

#[test]
fn native_diagnostic_and_firmware_statuses_decode_to_distinct_typed_observations() {
    for (action, class, statuses) in [
        (
            "DiagnosticsStatusNotification",
            TriggerMessageClass::DiagnosticsStatusNotification,
            &["Idle", "Uploaded", "UploadFailed", "Uploading"][..],
        ),
        (
            "FirmwareStatusNotification",
            TriggerMessageClass::FirmwareStatusNotification,
            &[
                "Downloaded",
                "DownloadFailed",
                "Downloading",
                "Idle",
                "InstallationFailed",
                "Installing",
                "Installed",
            ][..],
        ),
    ] {
        for status in statuses {
            let decoded = decode_call(&frame(action, &json!({"status":status}))).unwrap();
            assert_eq!(decoded.action.as_str(), action);
            assert_eq!(decoded.message_id, "notification-1");
            assert_eq!(
                decoded.observation,
                ChargerObservation::TriggerStatus {
                    class,
                    status: (*status).to_owned(),
                }
            );
        }
    }
}

#[test]
fn status_decode_rejects_unsupported_missing_extra_and_oversized_values() {
    for action in [
        "DiagnosticsStatusNotification",
        "FirmwareStatusNotification",
    ] {
        for payload in [
            json!({}),
            json!({"status":"Unrecognized"}),
            json!({"status":null}),
            json!({"status":1}),
            json!({"status":"Idle", "connectorId":1}),
            json!({"status":"I".repeat(33)}),
        ] {
            let error = decode_call(&frame(action, &payload)).unwrap_err();
            assert_eq!(
                error.kind(),
                DecodeErrorKind::InvalidPayload,
                "{action}: {payload}"
            );
        }
    }
}
