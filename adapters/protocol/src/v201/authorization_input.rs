use super::{PROTOCOL, payload_as};
use crate::{DecodeError, DecodeErrorKind};
use rust_ocpp::v2_0_1::{
    enumerations::{
        hash_algorithm_enum_type::HashAlgorithmEnumType, id_token_enum_type::IdTokenEnumType,
    },
    messages::authorize::AuthorizeRequest,
};
use serde_json::Value;
use uob_application::charging_identity::{
    AdditionalChargingIdentity, CertificateHashAlgorithm, ChargingCertificateHash,
    ChargingTokenKind, PresentedChargingIdentity,
};
use validator::Validate;

pub(super) fn observation(payload: Value) -> Result<PresentedChargingIdentity, DecodeError> {
    fields(
        &payload,
        &["idToken", "certificate", "iso15118CertificateHashData"],
    )?;
    let token = payload.get("idToken").ok_or_else(invalid)?;
    fields(token, &["idToken", "type", "additionalInfo"])?;
    if let Some(items) = token.get("additionalInfo") {
        for item in array(items, 16)? {
            fields(item, &["additionalIdToken", "type"])?;
        }
    }
    if let Some(items) = payload.get("iso15118CertificateHashData") {
        for item in array(items, 4)? {
            fields(
                item,
                &[
                    "hashAlgorithm",
                    "issuerNameHash",
                    "issuerKeyHash",
                    "serialNumber",
                    "responderURL",
                ],
            )?;
        }
    }
    let request: AuthorizeRequest = payload_as(payload)?;
    request.validate().map_err(|_| invalid())?;
    if request.id_token.id_token.is_empty() || request.id_token.id_token.chars().count() > 36 {
        return Err(invalid());
    }
    let additional = request
        .id_token
        .additional_info
        .unwrap_or_default()
        .into_iter()
        .map(|value| {
            value.validate().map_err(|_| invalid())?;
            Ok(AdditionalChargingIdentity {
                token: value.additional_id_token,
                kind: value.kind,
            })
        })
        .collect::<Result<Vec<_>, DecodeError>>()?;
    let certificate_hashes = request
        .iso_15118_certificate_hash_data
        .unwrap_or_default()
        .into_iter()
        .map(|value| {
            value.validate().map_err(|_| invalid())?;
            Ok(ChargingCertificateHash {
                algorithm: match value.hash_algorithm {
                    HashAlgorithmEnumType::SHA256 => CertificateHashAlgorithm::Sha256,
                    HashAlgorithmEnumType::SHA384 => CertificateHashAlgorithm::Sha384,
                    HashAlgorithmEnumType::SHA512 => CertificateHashAlgorithm::Sha512,
                },
                issuer_name_hash: value.issuer_name_hash,
                issuer_key_hash: value.issuer_key_hash,
                serial_number: value.serial_number,
                responder_url: value.responder_url,
            })
        })
        .collect::<Result<Vec<_>, DecodeError>>()?;
    Ok(PresentedChargingIdentity {
        token: request.id_token.id_token,
        kind: match request.id_token.kind {
            IdTokenEnumType::Central => ChargingTokenKind::Central,
            IdTokenEnumType::EMAID => ChargingTokenKind::Emaid,
            IdTokenEnumType::ISO14443 => ChargingTokenKind::Iso14443,
            IdTokenEnumType::ISO15693 => ChargingTokenKind::Iso15693,
            IdTokenEnumType::KeyCode => ChargingTokenKind::KeyCode,
            IdTokenEnumType::Local => ChargingTokenKind::Local,
            IdTokenEnumType::MacAddress => ChargingTokenKind::MacAddress,
            IdTokenEnumType::NoAuthorization => ChargingTokenKind::NoAuthorization,
        },
        additional,
        certificate: request.certificate,
        certificate_hashes,
    })
}
fn fields(value: &Value, allowed: &[&str]) -> Result<(), DecodeError> {
    let object = value.as_object().ok_or_else(invalid)?;
    if object.contains_key("customData") {
        // No vendor authorization semantics may be silently discarded.
        return Err(DecodeError::new(
            PROTOCOL,
            DecodeErrorKind::UnsupportedAction,
        ));
    }
    if object
        .iter()
        .any(|(key, value)| !allowed.contains(&key.as_str()) || value.is_null())
    {
        return Err(invalid());
    }
    Ok(())
}
fn array(value: &Value, maximum: usize) -> Result<&[Value], DecodeError> {
    value
        .as_array()
        .filter(|v| !v.is_empty() && v.len() <= maximum)
        .map(Vec::as_slice)
        .ok_or_else(invalid)
}
fn invalid() -> DecodeError {
    DecodeError::new(PROTOCOL, DecodeErrorKind::InvalidPayload)
}
