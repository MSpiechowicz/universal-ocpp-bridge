//! In-memory firmware model drivers shared by the native semantics tests.
use super::{TestPki, timestamp};
use serde_json::{Value, json};
use time::OffsetDateTime;
use uob_sim::firmware16::{FirmwareConfig, FirmwareEffects, FirmwareHandle, StationFacts};

pub const IMAGE: &[u8] = b"independent simulator firmware image 122";

#[must_use]
pub fn config(fields: Value) -> FirmwareConfig {
    let mut base = json!({"private_state_file": "/unused/firmware.json", "mode": "legacy"});
    let Value::Object(fields) = fields else {
        panic!("configuration fields")
    };
    base.as_object_mut().unwrap().extend(fields);
    serde_json::from_value(base).unwrap()
}

#[must_use]
pub fn legacy(fields: Value) -> FirmwareHandle {
    FirmwareHandle::in_memory("alpha", &config(fields), None).unwrap()
}

#[must_use]
pub fn signed(pki: &TestPki, fields: Value) -> FirmwareHandle {
    let mut fields = fields;
    fields["mode"] = "signed".into();
    FirmwareHandle::in_memory("alpha", &config(fields), Some(pki.root_pem.as_bytes())).unwrap()
}

#[must_use]
pub const fn up(generation: u64) -> StationFacts {
    StationFacts {
        idle: true,
        boot_generation: Some(generation),
    }
}

/// Deliver every pending status as an acknowledged CALL, returning `(status, requestId)`.
pub fn drain(handle: &FirmwareHandle) -> Vec<(String, Option<i32>)> {
    let now = OffsetDateTime::now_utc();
    let mut sent = Vec::new();
    while let Some(status) = handle.next_status(now) {
        handle.status_finished(&status, true, now).unwrap();
        sent.push((status.status, status.request_id));
    }
    sent
}

#[must_use]
pub fn names(sent: &[(String, Option<i32>)]) -> Vec<&str> {
    sent.iter().map(|(status, _)| status.as_str()).collect()
}

#[must_use]
pub fn update(retrieve: OffsetDateTime, extra: Value) -> Value {
    let mut payload = json!({"location": "http://127.0.0.1:9/firmware/legacy.bin", "retrieveDate": timestamp(retrieve)});
    let Value::Object(extra) = extra else {
        panic!("payload fields")
    };
    payload.as_object_mut().unwrap().extend(extra);
    payload
}

#[must_use]
pub fn signed_update(
    pki: &TestPki,
    id: i64,
    retrieve: OffsetDateTime,
    install: Option<OffsetDateTime>,
) -> Value {
    let mut firmware = json!({
        "location": "http://127.0.0.1:9/firmware/signed.bin",
        "retrieveDateTime": timestamp(retrieve),
        "signingCertificate": pki.certificate_pem,
        "signature": pki.sign(IMAGE),
    });
    if let Some(install) = install {
        firmware["installDateTime"] = timestamp(install).into();
    }
    json!({"requestId": id, "retries": 0, "firmware": firmware})
}

/// Run one download attempt to completion and advance past it.
pub fn download(
    handle: &FirmwareHandle,
    facts: StationFacts,
    image: Option<&[u8]>,
) -> FirmwareEffects {
    let now = OffsetDateTime::now_utc();
    let ticket = handle
        .advance(now, facts)
        .unwrap()
        .download
        .expect("download attempt");
    handle.download_finished(&ticket, image, now).unwrap();
    handle.advance(now, facts).unwrap()
}
