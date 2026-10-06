//! Expectations originate in the hand-authored OCA corpus, not bridge types or encoders.
mod firmware16_support;
use firmware16_support::TestPki;
use firmware16_support::model::{legacy, signed};
use serde_json::{Value, json};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

fn wire(name: &str) -> Value {
    let path = format!(
        "{}/../../tests/ocpp-fixtures/corpus/wire/1.6/{name}.json",
        env!("CARGO_MANIFEST_DIR")
    );
    serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
}

#[test]
fn corpus_requests_drive_the_real_station_model_to_the_corpus_replies() {
    let now = OffsetDateTime::parse("2026-10-06T12:00:00Z", &Rfc3339).unwrap();
    let station = legacy(json!({}));
    for request in ["firmware-update", "firmware-update-minimal"] {
        let call = wire(request);
        assert_eq!(
            station.handle_call(call[2].as_str().unwrap(), &call[3], now),
            wire("firmware-update-acknowledged")[2],
            "{request}"
        );
    }
    // A legacy station implements no Security Whitepaper message.
    let signed_call = wire("signed-firmware-update");
    assert_eq!(
        station.handle_call("SignedUpdateFirmware", &signed_call[3], now),
        json!({"callError":"NotImplemented"})
    );
    // The corpus certificate is not issued by this station's manufacturer root (L01.FR.24).
    let pki = TestPki::generate();
    let secure = signed(&pki, json!({}));
    assert_eq!(
        secure.handle_call("SignedUpdateFirmware", &signed_call[3], now),
        wire("signed-firmware-update-invalid-certificate")[2]
    );
    // L01.FR.20: the original message is refused without starting a process.
    assert_eq!(
        secure.handle_call("UpdateFirmware", &wire("firmware-update")[3], now),
        json!({"callError":"NotSupported"})
    );
    assert_eq!(secure.snapshot()["active"], false);
}

#[test]
fn corpus_negative_requests_never_start_a_native_process() {
    let now = OffsetDateTime::parse("2026-10-06T12:00:00Z", &Rfc3339).unwrap();
    let cases = wire("firmware-negative-cases");
    let pki = TestPki::generate();
    for case in cases["cases"].as_array().unwrap() {
        let action = case["wire"][2].as_str().unwrap();
        if action.ends_with("StatusNotification") {
            continue;
        }
        // Schema-valid cases are refused by native semantics (for example negative retries).
        let station = if action == "UpdateFirmware" {
            legacy(json!({}))
        } else {
            signed(&pki, json!({}))
        };
        let reply = station.handle_call(action, &case["wire"][3], now);
        assert!(
            reply.get("callError").is_some()
                || reply == json!({"status":"InvalidCertificate"})
                || reply == json!({"status":"Rejected"}),
            "{}: {reply}",
            case["id"]
        );
        assert_eq!(station.snapshot()["active"], false, "{}", case["id"]);
    }
}
