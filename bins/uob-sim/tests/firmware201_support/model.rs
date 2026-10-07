//! In-memory 2.0.1 firmware model drivers shared by the native semantics tests.
use super::{TestPki, timestamp};
use serde_json::{Value, json};
use time::OffsetDateTime;
use uob_sim::firmware201::{
    Firmware201Handle, FirmwareConfig201, FirmwareEffects201, StationFacts201,
};

pub const IMAGE: &[u8] = b"independent simulator firmware image 123";

#[must_use]
pub fn config(fields: Value) -> FirmwareConfig201 {
    let mut base = json!({"private_state_file": "/unused/firmware.json", "mode": "non_secure"});
    let Value::Object(fields) = fields else {
        panic!("configuration fields")
    };
    base.as_object_mut().unwrap().extend(fields);
    serde_json::from_value(base).unwrap()
}

#[must_use]
pub fn non_secure(fields: Value) -> Firmware201Handle {
    Firmware201Handle::in_memory("alpha", &config(fields), None).unwrap()
}

#[must_use]
pub fn secure_with_root(root_pem: &str, fields: Value) -> Firmware201Handle {
    let mut fields = fields;
    fields["mode"] = "secure".into();
    Firmware201Handle::in_memory("alpha", &config(fields), Some(root_pem.as_bytes())).unwrap()
}

#[must_use]
pub fn secure(pki: &TestPki, fields: Value) -> Firmware201Handle {
    secure_with_root(&pki.root_pem, fields)
}

#[must_use]
pub const fn up(generation: u64) -> StationFacts201 {
    StationFacts201 {
        idle: true,
        boot_generation: Some(generation),
    }
}

/// Deliver every pending status as an acknowledged CALL, returning `(status, requestId)`.
pub fn drain(handle: &Firmware201Handle) -> Vec<(String, i32)> {
    let now = OffsetDateTime::now_utc();
    let mut sent = Vec::new();
    while let Some(status) = handle.next_status(now) {
        handle.status_finished(&status, true, now).unwrap();
        sent.push((status.status, status.request_id));
    }
    sent
}

#[must_use]
pub fn names(sent: &[(String, i32)]) -> Vec<&str> {
    sent.iter().map(|(status, _)| status.as_str()).collect()
}

/// A native `UpdateFirmwareRequest`; `signing` adds the certificate and signature (L01).
#[must_use]
pub fn update(
    id: i64,
    retrieve: OffsetDateTime,
    install: Option<OffsetDateTime>,
    signing: Option<(&str, String)>,
) -> Value {
    let mut firmware = json!({
        "location": "http://127.0.0.1:9/firmware/station.bin",
        // OCPP 2.0.1 date-time carries at most millisecond precision.
        "retrieveDateTime": timestamp(retrieve.replace_nanosecond(0).unwrap()),
    });
    if let Some(install) = install {
        firmware["installDateTime"] = timestamp(install.replace_nanosecond(0).unwrap()).into();
    }
    if let Some((certificate, signature)) = signing {
        firmware["signingCertificate"] = certificate.into();
        firmware["signature"] = signature.into();
    }
    json!({"requestId": id, "retries": 0, "firmware": firmware})
}

#[must_use]
pub fn signed_update(pki: &TestPki, id: i64, retrieve: OffsetDateTime) -> Value {
    update(
        id,
        retrieve,
        None,
        Some((&pki.certificate_pem, pki.sign(IMAGE))),
    )
}

/// Run one download attempt to completion and advance past it.
pub fn download(
    handle: &Firmware201Handle,
    facts: StationFacts201,
    image: Option<&[u8]>,
) -> FirmwareEffects201 {
    let now = OffsetDateTime::now_utc();
    let ticket = handle
        .advance(now, facts)
        .unwrap()
        .download
        .expect("download attempt");
    handle.download_finished(&ticket, image, now).unwrap();
    handle.advance(now, facts).unwrap()
}
