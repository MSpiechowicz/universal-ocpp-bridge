//! Strict native request parsing: OCPP 1.6 Edition 2 §6.55 and Security Whitepaper Ed4 §5.21.
use serde::Deserialize;
use serde_json::Value;
use time::{OffsetDateTime, format_description::well_known::Rfc3339};
use zeroize::Zeroize;

const LOCATION_LIMIT: usize = 512;
const CERTIFICATE_LIMIT: usize = 5_500;
const SIGNATURE_LIMIT: usize = 800;

/// One validated native firmware request, independent of the CALL action that carried it.
pub(crate) struct FirmwareRequest {
    pub(crate) request_id: Option<i32>,
    pub(crate) location: String,
    pub(crate) retrieve_at: OffsetDateTime,
    pub(crate) install_at: Option<OffsetDateTime>,
    pub(crate) retries: u32,
    pub(crate) retry_interval: u32,
    pub(crate) signing_certificate: Option<String>,
    pub(crate) signature: Option<String>,
}

impl Drop for FirmwareRequest {
    fn drop(&mut self) {
        self.location.zeroize();
        if let Some(value) = &mut self.signing_certificate {
            value.zeroize();
        }
        if let Some(value) = &mut self.signature {
            value.zeroize();
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Legacy {
    location: String,
    retrieve_date: String,
    #[serde(default, deserialize_with = "present")]
    retries: Option<i64>,
    #[serde(default, deserialize_with = "present")]
    retry_interval: Option<i64>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Signed {
    request_id: i64,
    #[serde(default, deserialize_with = "present")]
    retries: Option<i64>,
    #[serde(default, deserialize_with = "present")]
    retry_interval: Option<i64>,
    firmware: SignedFirmware,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SignedFirmware {
    location: String,
    retrieve_date_time: String,
    #[serde(default, deserialize_with = "present")]
    install_date_time: Option<String>,
    signing_certificate: String,
    signature: String,
}

/// Native CALLERROR code for a request that cannot be represented by the pinned schema.
pub(crate) type CallError = &'static str;

/// # Errors
/// Returns `FormationViolation` for an unparseable shape and `PropertyConstraintViolation` for
/// values outside the native range.
pub(crate) fn legacy(payload: &Value) -> Result<FirmwareRequest, CallError> {
    let mut request = Legacy::deserialize(payload).map_err(|_| "FormationViolation")?;
    let result = (|| {
        Ok(FirmwareRequest {
            request_id: None,
            location: location(&request.location, usize::MAX)?,
            retrieve_at: timestamp(&request.retrieve_date)?,
            install_at: None,
            retries: count(request.retries)?,
            retry_interval: count(request.retry_interval)?,
            signing_certificate: None,
            signature: None,
        })
    })();
    request.location.zeroize();
    result
}

/// # Errors
/// As [`legacy`], plus bounds of the Security Whitepaper `FirmwareType`.
pub(crate) fn signed(payload: &Value) -> Result<FirmwareRequest, CallError> {
    let mut request = Signed::deserialize(payload).map_err(|_| "FormationViolation")?;
    let result = (|| {
        let firmware = &request.firmware;
        if firmware.signing_certificate.is_empty()
            || firmware.signing_certificate.len() > CERTIFICATE_LIMIT
            || firmware.signature.is_empty()
            || firmware.signature.len() > SIGNATURE_LIMIT
        {
            return Err("PropertyConstraintViolation");
        }
        Ok(FirmwareRequest {
            request_id: Some(
                i32::try_from(request.request_id).map_err(|_| "PropertyConstraintViolation")?,
            ),
            location: location(&firmware.location, LOCATION_LIMIT)?,
            retrieve_at: timestamp(&firmware.retrieve_date_time)?,
            install_at: firmware
                .install_date_time
                .as_deref()
                .map(timestamp)
                .transpose()?,
            retries: count(request.retries)?,
            retry_interval: count(request.retry_interval)?,
            signing_certificate: Some(firmware.signing_certificate.clone()),
            signature: Some(firmware.signature.clone()),
        })
    })();
    request.firmware.location.zeroize();
    request.firmware.signing_certificate.zeroize();
    request.firmware.signature.zeroize();
    result
}

fn location(value: &str, limit: usize) -> Result<String, CallError> {
    if value.is_empty() || value.len() > limit {
        return Err("PropertyConstraintViolation");
    }
    Ok(value.to_owned())
}

fn timestamp(value: &str) -> Result<OffsetDateTime, CallError> {
    OffsetDateTime::parse(value, &Rfc3339).map_err(|_| "PropertyConstraintViolation")
}

fn count(value: Option<i64>) -> Result<u32, CallError> {
    value.map_or(Ok(0), |value| {
        u32::try_from(value)
            .ok()
            .filter(|value| i32::try_from(*value).is_ok())
            .ok_or("PropertyConstraintViolation")
    })
}

/// Rejects an explicit JSON `null` for an optional native field instead of treating it as absent.
fn present<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    T::deserialize(deserializer).map(Some)
}
