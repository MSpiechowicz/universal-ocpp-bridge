//! Typed charger reports that can independently evidence OCPP 2.0.1 triggers.
use rust_ocpp::v2_0_1::{
    enumerations::certificate_signing_use_enum_type::CertificateSigningUseEnumType,
    messages::{
        firmware_status_notification::FirmwareStatusNotificationRequest,
        log_status_notification::LogStatusNotificationRequest,
        publish_firmware_status_notification::PublishFirmwareStatusNotificationRequest,
        sign_certificate::SignCertificateRequest,
    },
};
use serde_json::Value;
use uob_application::ChargerObservation;
use uob_contracts::TriggerMessageClass201;

use super::{PROTOCOL, payload_as};
use crate::{DecodeError, DecodeErrorKind};

fn invalid() -> DecodeError {
    DecodeError::new(PROTOCOL, DecodeErrorKind::InvalidPayload)
}

fn fields(payload: &Value, allowed: &[&str]) -> Result<(), DecodeError> {
    if payload.as_object().is_some_and(|object| {
        object
            .keys()
            .all(|key| allowed.contains(&key.as_str()) || key == "customData")
            && object.values().all(|value| !value.is_null())
            && object.get("customData").is_none_or(|data| {
                data.as_object().is_some_and(|fields| {
                    fields
                        .get("vendorId")
                        .and_then(Value::as_str)
                        .is_some_and(|vendor| vendor.chars().count() <= 255)
                })
            })
    }) {
        Ok(())
    } else {
        Err(invalid())
    }
}

fn status(payload: &Value) -> Result<(), DecodeError> {
    if payload
        .get("status")
        .and_then(Value::as_str)
        .is_some_and(|value| !value.is_empty() && value.len() <= 64)
    {
        Ok(())
    } else {
        Err(invalid())
    }
}

fn request_id(payload: &Value) -> Result<(), DecodeError> {
    if payload
        .get("requestId")
        .is_none_or(|value| value.as_i64().is_some_and(|id| i32::try_from(id).is_ok()))
    {
        Ok(())
    } else {
        Err(invalid())
    }
}

/// `LogStatusNotificationRequest`; `requestId` is mandatory unless the status is `Idle`
/// (N01.FR.13: only a triggered report with no upload ongoing may omit it), because every other
/// report belongs to exactly one upload (N01.FR.07).
pub(super) fn log(payload: Value) -> Result<ChargerObservation, DecodeError> {
    #[derive(serde::Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Request {
        status: uob_contracts::LogUploadStatus201,
        #[serde(default)]
        request_id: Option<i32>,
    }
    fields(&payload, &["status", "requestId"])?;
    status(&payload)?;
    request_id(&payload)?;
    let _: LogStatusNotificationRequest = payload_as(payload.clone())?;
    let request: Request = serde_json::from_value(payload).map_err(|_| invalid())?;
    if request.request_id.is_none() && request.status != uob_contracts::LogUploadStatus201::Idle {
        return Err(invalid());
    }
    Ok(ChargerObservation::LogStatus201 {
        status: request.status,
        request_id: request.request_id,
    })
}

/// `FirmwareStatusNotificationRequest`; `requestId` is mandatory unless the status is `Idle`
/// (L01.FR.20), because every other report belongs to exactly one update (L01.FR.10).
pub(super) fn firmware(payload: Value) -> Result<ChargerObservation, DecodeError> {
    #[derive(serde::Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Request {
        status: uob_contracts::FirmwareStatus201,
        #[serde(default)]
        request_id: Option<i32>,
    }
    fields(&payload, &["status", "requestId"])?;
    status(&payload)?;
    request_id(&payload)?;
    let _: FirmwareStatusNotificationRequest = payload_as(payload.clone())?;
    let request: Request = serde_json::from_value(payload).map_err(|_| invalid())?;
    if request.request_id.is_none() && request.status != uob_contracts::FirmwareStatus201::Idle {
        return Err(invalid());
    }
    Ok(ChargerObservation::FirmwareStatus201 {
        status: request.status,
        request_id: request.request_id,
    })
}

pub(super) fn publish_firmware(payload: Value) -> Result<ChargerObservation, DecodeError> {
    fields(&payload, &["status", "requestId", "location"])?;
    status(&payload)?;
    request_id(&payload)?;
    if payload.get("location").is_some_and(|value| {
        value.as_array().is_none_or(|locations| {
            locations.is_empty()
                || locations.len() > 32
                || locations.iter().any(|location| {
                    location
                        .as_str()
                        .is_none_or(|s| s.is_empty() || s.len() > 512)
                })
        })
    }) {
        return Err(invalid());
    }
    let request: PublishFirmwareStatusNotificationRequest = payload_as(payload)?;
    Ok(ChargerObservation::TriggerStatus201 {
        class: TriggerMessageClass201::PublishFirmwareStatusNotification,
        status: format!("{:?}", request.status),
    })
}

pub(super) fn certificate(payload: Value) -> Result<ChargerObservation, DecodeError> {
    fields(&payload, &["csr", "certificateType"])?;
    if payload
        .get("csr")
        .and_then(Value::as_str)
        .is_none_or(|csr| csr.is_empty() || csr.len() > 5500)
    {
        return Err(invalid());
    }
    let request: SignCertificateRequest = payload_as(payload)?;
    let class = match request.certificate_type {
        Some(CertificateSigningUseEnumType::ChargingStationCertificate) => {
            TriggerMessageClass201::SignChargingStationCertificate
        }
        Some(CertificateSigningUseEnumType::V2GCertificate) => {
            TriggerMessageClass201::SignV2GCertificate
        }
        None => TriggerMessageClass201::SignCombinedCertificate,
    };
    // The CSR remains in the wire layer: receipt does not sign or approve a certificate.
    Ok(ChargerObservation::TriggerCertificate201 { class })
}
