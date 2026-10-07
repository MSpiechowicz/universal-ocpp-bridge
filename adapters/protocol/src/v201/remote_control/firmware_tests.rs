use super::native_reply;
use serde_json::json;
use uob_contracts::{FirmwareReply201, UpdateFirmwareStatus201};

#[test]
fn every_native_status_and_reason_code_is_kept_exactly() {
    for (status, native) in [
        ("Accepted", UpdateFirmwareStatus201::Accepted),
        ("Rejected", UpdateFirmwareStatus201::Rejected),
        (
            "AcceptedCanceled",
            UpdateFirmwareStatus201::AcceptedCanceled,
        ),
        (
            "InvalidCertificate",
            UpdateFirmwareStatus201::InvalidCertificate,
        ),
        (
            "RevokedCertificate",
            UpdateFirmwareStatus201::RevokedCertificate,
        ),
    ] {
        assert_eq!(
            native_reply(&json!({"status": status})),
            Some(FirmwareReply201::Status {
                status: native,
                reason_code: None
            })
        );
    }
    // additionalInfo is free station text: validated, but never retained.
    assert_eq!(
        native_reply(&json!({
            "status": "Rejected",
            "statusInfo": {"reasonCode": "NoFirmware", "additionalInfo": "busy", "customData": {"vendorId": "v"}},
            "customData": {"vendorId": "v"}
        })),
        Some(FirmwareReply201::Status {
            status: UpdateFirmwareStatus201::Rejected,
            reason_code: Some("NoFirmware".to_owned())
        })
    );
}

#[test]
fn malformed_replies_leave_delivery_uncertain() {
    for payload in [
        json!({}),
        json!([]),
        json!({"status": "accepted"}),
        json!({"status": "Accepted", "extra": 1}),
        json!({"status": "Accepted", "statusInfo": {}}),
        json!({"status": "Accepted", "statusInfo": {"reasonCode": "x".repeat(21)}}),
        json!({"status": "Accepted", "statusInfo": {"reasonCode": "has space"}}),
        json!({"status": "Accepted", "statusInfo": {"reasonCode": "Ok", "additionalInfo": "x".repeat(513)}}),
        json!({"status": "Accepted", "statusInfo": {"reasonCode": "Ok", "other": 1}}),
        json!({"status": "Accepted", "customData": {}}),
    ] {
        assert_eq!(native_reply(&payload), None, "{payload}");
    }
}
