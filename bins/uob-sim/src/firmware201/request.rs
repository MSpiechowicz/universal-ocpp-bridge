//! Strict native `UpdateFirmwareRequest` parsing against the pinned OCPP 2.0.1 schema.
use serde::Deserialize;
use serde_json::Value;
use std::sync::LazyLock;
use time::{OffsetDateTime, format_description::well_known::Rfc3339};
use zeroize::Zeroize;

static SCHEMA: LazyLock<Value> = LazyLock::new(|| {
    serde_json::from_str(include_str!(
        "../../../../tests/ocpp-fixtures/corpus/schemas/2.0.1/UpdateFirmwareRequest.json"
    ))
    .expect("pinned schema")
});

/// One validated native request; signing material is present only when the CSMS sent it.
pub(crate) struct FirmwareRequest201 {
    pub(crate) request_id: i32,
    pub(crate) location: String,
    pub(crate) retrieve_at: OffsetDateTime,
    pub(crate) install_at: Option<OffsetDateTime>,
    pub(crate) retries: u32,
    pub(crate) retry_interval: u32,
    pub(crate) signing_certificate: Option<String>,
    pub(crate) signature: Option<String>,
}

impl Drop for FirmwareRequest201 {
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

// The pinned schema already refused unknown fields; vendor customData is ignored here.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Native {
    request_id: i64,
    retries: Option<i64>,
    retry_interval: Option<i64>,
    firmware: NativeFirmware,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct NativeFirmware {
    location: String,
    retrieve_date_time: String,
    install_date_time: Option<String>,
    signing_certificate: Option<String>,
    signature: Option<String>,
}

/// # Errors
/// `FormationViolation` for a payload outside the pinned schema and
/// `PropertyConstraintViolation` for values outside the native integer or time range.
pub(crate) fn parse(payload: &Value) -> Result<FirmwareRequest201, &'static str> {
    // An explicit null is never an absent optional field.
    if payload.pointer("/firmware").is_some_and(|firmware| {
        firmware
            .as_object()
            .is_some_and(|fields| fields.values().any(Value::is_null))
    }) || payload
        .as_object()
        .is_some_and(|fields| fields.values().any(Value::is_null))
        || !crate::local_authorization201::validation::valid_native(&SCHEMA, payload)
    {
        return Err("FormationViolation");
    }
    let mut native = Native::deserialize(payload).map_err(|_| "FormationViolation")?;
    let result = (|| {
        let firmware = &native.firmware;
        if firmware.location.is_empty()
            || firmware
                .signing_certificate
                .as_ref()
                .is_some_and(String::is_empty)
            || firmware.signature.as_ref().is_some_and(String::is_empty)
        {
            return Err("PropertyConstraintViolation");
        }
        Ok(FirmwareRequest201 {
            request_id: i32::try_from(native.request_id)
                .map_err(|_| "PropertyConstraintViolation")?,
            location: firmware.location.clone(),
            retrieve_at: timestamp(&firmware.retrieve_date_time)?,
            install_at: firmware
                .install_date_time
                .as_deref()
                .map(timestamp)
                .transpose()?,
            retries: count(native.retries)?,
            retry_interval: count(native.retry_interval)?,
            signing_certificate: firmware.signing_certificate.clone(),
            signature: firmware.signature.clone(),
        })
    })();
    native.firmware.location.zeroize();
    if let Some(value) = &mut native.firmware.signing_certificate {
        value.zeroize();
    }
    if let Some(value) = &mut native.firmware.signature {
        value.zeroize();
    }
    result
}

fn timestamp(value: &str) -> Result<OffsetDateTime, &'static str> {
    OffsetDateTime::parse(value, &Rfc3339).map_err(|_| "PropertyConstraintViolation")
}

fn count(value: Option<i64>) -> Result<u32, &'static str> {
    value.map_or(Ok(0), |value| {
        u32::try_from(value)
            .ok()
            .filter(|value| i32::try_from(*value).is_ok())
            .ok_or("PropertyConstraintViolation")
    })
}
