//! Typed OCPP 1.6 status notifications that may satisfy a `TriggerMessage`.

use rust_ocpp::v1_6::{
    messages::{
        diagnostics_status_notification::DiagnosticsStatusNotificationRequest,
        firmware_status_notification::FirmwareStatusNotificationRequest,
    },
    types::{DiagnosticsStatus, FirmwareStatus},
};
use serde_json::Value;
use uob_application::ChargerObservation;
use uob_contracts::{ProtocolEdition, TriggerMessageClass};

use crate::{DecodeError, DecodeErrorKind};

const PROTOCOL: ProtocolEdition = ProtocolEdition::Ocpp16j;

fn status_field(payload: &Value) -> Result<(), DecodeError> {
    let valid = payload
        .as_object()
        .filter(|fields| fields.len() == 1)
        .and_then(|fields| fields.get("status"))
        .and_then(Value::as_str)
        .is_some_and(|status| !status.is_empty() && status.len() <= 32);
    if valid {
        Ok(())
    } else {
        Err(DecodeError::new(PROTOCOL, DecodeErrorKind::InvalidPayload))
    }
}

/// A native status is a station-level observation, not proof that any pending
/// trigger was accepted or satisfied; the application correlates it separately.
pub(super) fn diagnostics(payload: Value) -> Result<ChargerObservation, DecodeError> {
    status_field(&payload)?;
    let request: DiagnosticsStatusNotificationRequest = serde_json::from_value(payload)
        .map_err(|_| DecodeError::new(PROTOCOL, DecodeErrorKind::InvalidPayload))?;
    let status = match request.status {
        DiagnosticsStatus::Idle => "Idle",
        DiagnosticsStatus::Uploaded => "Uploaded",
        DiagnosticsStatus::UploadFailed => "UploadFailed",
        DiagnosticsStatus::Uploading => "Uploading",
    };
    Ok(ChargerObservation::TriggerStatus {
        class: TriggerMessageClass::DiagnosticsStatusNotification,
        status: status.to_owned(),
    })
}

pub(super) fn firmware(payload: Value) -> Result<ChargerObservation, DecodeError> {
    status_field(&payload)?;
    let request: FirmwareStatusNotificationRequest = serde_json::from_value(payload)
        .map_err(|_| DecodeError::new(PROTOCOL, DecodeErrorKind::InvalidPayload))?;
    let status = match request.status {
        FirmwareStatus::Downloaded => "Downloaded",
        FirmwareStatus::DownloadFailed => "DownloadFailed",
        FirmwareStatus::Downloading => "Downloading",
        FirmwareStatus::Idle => "Idle",
        FirmwareStatus::InstallationFailed => "InstallationFailed",
        FirmwareStatus::Installing => "Installing",
        FirmwareStatus::Installed => "Installed",
    };
    Ok(ChargerObservation::TriggerStatus {
        class: TriggerMessageClass::FirmwareStatusNotification,
        status: status.to_owned(),
    })
}

/// Security Whitepaper Ed. 4 §5.19. Exact fields only; the request identity is mandatory
/// unless the station reports `Idle` (L01.FR.21).
pub(super) fn signed_firmware(payload: Value) -> Result<ChargerObservation, DecodeError> {
    #[derive(serde::Deserialize)]
    #[serde(rename_all = "camelCase", deny_unknown_fields)]
    struct Request {
        status: uob_contracts::FirmwareStatus16,
        #[serde(default)]
        request_id: Option<i32>,
    }
    let invalid = || DecodeError::new(PROTOCOL, DecodeErrorKind::InvalidPayload);
    let request: Request = serde_json::from_value(payload).map_err(|_| invalid())?;
    if request.request_id.is_none() && request.status != uob_contracts::FirmwareStatus16::Idle {
        return Err(invalid());
    }
    Ok(ChargerObservation::SignedFirmwareStatus16 {
        status: request.status,
        request_id: request.request_id,
    })
}
