//! Expectations originate in the hand-authored OCA corpus, not bridge types or encoders.
mod firmware201_support;
use firmware201_support::TestPki;
use firmware201_support::model::{non_secure, secure};
use serde_json::{Value, json};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

fn wire(name: &str) -> Value {
    let path = format!(
        "{}/../../tests/ocpp-fixtures/corpus/wire/2.0.1/{name}.json",
        env!("CARGO_MANIFEST_DIR")
    );
    serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
}

#[test]
fn corpus_requests_drive_the_real_station_model_to_the_corpus_replies() {
    let now = OffsetDateTime::parse("2026-10-07T12:00:00Z", &Rfc3339).unwrap();
    for request in [
        "firmware-update-non-secure",
        "firmware-update-minimal",
        "firmware-update-secure",
    ] {
        // A non-secure station accepts any well-formed request, ignoring signing material.
        let station = non_secure(json!({}));
        assert_eq!(
            station.handle_call(&wire(request)[3], now),
            wire("firmware-update-result-accepted")[2],
            "{request}"
        );
        assert_eq!(station.snapshot()["active"], true);
    }
    // The corpus certificate is not issued by this station's manufacturer root (L01.FR.22).
    let pki = TestPki::generate();
    for request in ["firmware-update-secure", "firmware-update-secure-minimal"] {
        let station = secure(&pki, json!({}));
        assert_eq!(
            station.handle_call(&wire(request)[3], now),
            wire("firmware-update-result-invalid-certificate")[2],
            "{request}"
        );
        assert_eq!(station.snapshot()["active"], false);
    }
    // L01.FR.24: a second corpus request cancels the first.
    let station = non_secure(json!({}));
    let _ = station.handle_call(&wire("firmware-update-non-secure")[3], now);
    assert_eq!(
        station.handle_call(&wire("firmware-update-minimal")[3], now),
        wire("firmware-update-result-accepted-canceled")[2]
    );
    // Every reported status uses the corpus vocabulary and requestId shape.
    let statuses: Vec<String> = [
        "downloaded",
        "download-failed",
        "downloading",
        "download-scheduled",
        "download-paused",
        "idle",
        "installation-failed",
        "installing",
        "installed",
        "install-rebooting",
        "install-scheduled",
        "install-verification-failed",
        "invalid-signature",
        "signature-verified",
    ]
    .iter()
    .map(|name| {
        let frame = wire(&format!("firmware-status-{name}"));
        assert_eq!(frame[3]["requestId"], 2010);
        frame[3]["status"].as_str().unwrap().to_owned()
    })
    .collect();
    assert_eq!(statuses.len(), 14);
    assert_eq!(
        wire("firmware-status-idle-unattributed")[3],
        json!({"status":"Idle"})
    );
    assert_eq!(
        non_secure(json!({})).trigger_payload(),
        json!({"status":"Idle"})
    );
}

#[test]
fn corpus_negative_requests_never_start_a_native_process() {
    let now = OffsetDateTime::parse("2026-10-07T12:00:00Z", &Rfc3339).unwrap();
    let cases = wire("firmware-negative-cases");
    let pki = TestPki::generate();
    for case in cases["cases"].as_array().unwrap() {
        if case["wire"][2] != "UpdateFirmware" {
            continue;
        }
        let payload = &case["wire"][3];
        // Schema-invalid requests fail in both modes; schema-valid ones fail native semantics.
        let station = if case["schema_valid"] == true {
            secure(&pki, json!({}))
        } else {
            non_secure(json!({}))
        };
        let reply = station.handle_call(payload, now);
        let refused = reply.get("callError").is_some()
            || reply == json!({"status":"InvalidCertificate"})
            || reply == json!({"status":"Rejected"});
        assert!(refused, "{}: {reply}", case["id"]);
        assert_eq!(station.snapshot()["active"], false, "{}", case["id"]);
    }
}
