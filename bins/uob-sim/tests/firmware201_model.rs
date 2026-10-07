//! Native OCPP 2.0.1 firmware semantics (L01, L02, Figure 116) on the in-memory station model.
mod firmware201_support;
use firmware201_support::model::{
    IMAGE, download, drain, names, non_secure, secure, secure_with_root, signed_update, up, update,
};
use firmware201_support::{EcdsaPki, TestPki};
use serde_json::json;
use time::{Duration, OffsetDateTime};
use uob_sim::firmware201::StationFacts201;

/// Finish a pending reboot: the old generation requests it, a new accepted Boot completes it.
fn reboot(handle: &uob_sim::firmware201::Firmware201Handle) -> String {
    let now = OffsetDateTime::now_utc();
    let version = handle
        .advance(now, up(1))
        .unwrap()
        .reboot
        .expect("reboot requested");
    assert_eq!(
        handle.advance(now, StationFacts201::default()).unwrap(),
        uob_sim::firmware201::FirmwareEffects201::default()
    );
    handle.advance(now, up(2)).unwrap();
    version
}

#[test]
fn secure_update_verifies_certificate_and_signature_then_reports_every_state_with_request_id() {
    let pki = TestPki::generate();
    let station = secure(&pki, json!({}));
    let now = OffsetDateTime::now_utc();
    assert_eq!(
        station.handle_call(&signed_update(&pki, 77, now), now),
        json!({"status":"Accepted"})
    );
    download(&station, up(1), Some(IMAGE));
    let sent = drain(&station);
    assert_eq!(
        names(&sent),
        [
            "Downloading",
            "Downloaded",
            "SignatureVerified",
            "Installing",
            "InstallRebooting"
        ]
    );
    // L01.FR.10: every report carries the requestId of the request that started it.
    assert!(sent.iter().all(|(_, id)| *id == 77));
    let version = reboot(&station);
    assert_eq!(drain(&station), [("Installed".to_owned(), 77)]);
    let snapshot = station.snapshot();
    assert_eq!(snapshot["installedVersion"], version);
    assert_eq!(
        (snapshot["reboots"].clone(), snapshot["active"].clone()),
        (json!(1), json!(false))
    );
}

#[test]
fn secure_station_refuses_untrusted_or_missing_certificates_before_accepting() {
    let pki = TestPki::generate();
    let stranger = TestPki::generate();
    let station = secure(&pki, json!({}));
    let now = OffsetDateTime::now_utc();
    // L01.FR.21/22: InvalidCertificate, and no process starts.
    for request in [
        signed_update(&stranger, 1, now),
        update(2, now, None, None),
        update(3, now, None, Some(("not a certificate", pki.sign(IMAGE)))),
    ] {
        assert_eq!(
            station.handle_call(&request, now),
            json!({"status":"InvalidCertificate"})
        );
    }
    assert_eq!(station.snapshot()["active"], false);
    assert!(drain(&station).is_empty());
}

#[test]
fn invalid_or_missing_signature_ends_after_download_with_invalid_signature() {
    let pki = TestPki::generate();
    let now = OffsetDateTime::now_utc();
    let tampered = update(
        8,
        now,
        None,
        Some((&pki.certificate_pem, pki.sign(b"another image"))),
    );
    // L01 scenario 5: a missing signature is treated as an invalid one.
    let mut unsigned = update(9, now, None, None);
    unsigned["firmware"]["signingCertificate"] = pki.certificate_pem.clone().into();
    for request in [tampered, unsigned] {
        let station = secure(&pki, json!({}));
        assert_eq!(station.handle_call(&request, now)["status"], "Accepted");
        download(&station, up(1), Some(IMAGE));
        assert_eq!(
            names(&drain(&station)),
            ["Downloading", "Downloaded", "InvalidSignature"]
        );
        assert_eq!(station.snapshot()["active"], false);
    }
}

#[test]
fn errata_ecdsa_signature_over_the_entire_file_is_verified() {
    let pki = EcdsaPki::generate();
    let station = secure_with_root(&pki.root_pem, json!({"reboot_after_install": false}));
    let now = OffsetDateTime::now_utc();
    let request = update(5, now, None, Some((&pki.certificate_pem, pki.sign(IMAGE))));
    assert_eq!(station.handle_call(&request, now)["status"], "Accepted");
    download(&station, up(1), Some(IMAGE));
    assert_eq!(
        names(&drain(&station)),
        [
            "Downloading",
            "Downloaded",
            "SignatureVerified",
            "Installing",
            "Installed"
        ]
    );
}

#[test]
fn non_secure_station_ignores_signing_material_and_skips_signature_states() {
    let pki = TestPki::generate();
    let station = non_secure(json!({}));
    let now = OffsetDateTime::now_utc();
    // L02 has no certificate check; signing material is not interpreted.
    assert_eq!(
        station.handle_call(&signed_update(&pki, -3, now), now)["status"],
        "Accepted"
    );
    download(&station, up(1), Some(IMAGE));
    assert_eq!(
        names(&drain(&station)),
        [
            "Downloading",
            "Downloaded",
            "Installing",
            "InstallRebooting"
        ]
    );
    reboot(&station);
    assert_eq!(drain(&station), [("Installed".to_owned(), -3)]);
}

#[test]
fn future_retrieval_and_installation_are_reported_scheduled() {
    let pki = TestPki::generate();
    let station = secure(&pki, json!({}));
    let now = OffsetDateTime::now_utc();
    let install = now + Duration::hours(1);
    let request = update(
        11,
        now + Duration::seconds(30),
        Some(install),
        Some((&pki.certificate_pem, pki.sign(IMAGE))),
    );
    assert_eq!(station.handle_call(&request, now)["status"], "Accepted");
    // L01.FR.13: the download does not start before retrieveDateTime.
    assert!(station.advance(now, up(1)).unwrap().download.is_none());
    assert_eq!(names(&drain(&station)), ["DownloadScheduled"]);
    let later = now + Duration::seconds(31);
    let ticket = station.advance(later, up(1)).unwrap().download.unwrap();
    station
        .download_finished(&ticket, Some(IMAGE), later)
        .unwrap();
    station.advance(later, up(1)).unwrap();
    // L01.FR.16: installation waits for installDateTime.
    assert_eq!(
        names(&drain(&station)),
        [
            "Downloading",
            "Downloaded",
            "SignatureVerified",
            "InstallScheduled"
        ]
    );
    station.advance(install, up(1)).unwrap();
    assert_eq!(names(&drain(&station)), ["Installing", "InstallRebooting"]);
}

#[test]
fn bounded_retries_end_with_download_failed_after_the_last_attempt() {
    let station = non_secure(json!({"download_failures": 3}));
    let now = OffsetDateTime::now_utc();
    let mut request = update(4, now, None, None);
    request["retries"] = 1.into();
    request["retryInterval"] = 0.into();
    assert_eq!(station.handle_call(&request, now)["status"], "Accepted");
    // The retry is immediate (retryInterval 0), so the next attempt starts at once.
    let retry = download(&station, up(1), None)
        .download
        .expect("second attempt");
    station.download_finished(&retry, None, now).unwrap();
    station.advance(now, up(1)).unwrap();
    // L01.FR.30: one Downloading per attempt, then DownloadFailed.
    assert_eq!(
        names(&drain(&station)),
        ["Downloading", "Downloading", "DownloadFailed"]
    );
    assert_eq!(station.snapshot()["active"], false);
}

#[test]
fn a_new_request_cancels_or_is_rejected_by_the_configured_cancel_policy() {
    let now = OffsetDateTime::now_utc();
    let later = now + Duration::hours(1);
    let station = non_secure(json!({}));
    assert_eq!(
        station.handle_call(&update(1, later, None, None), now)["status"],
        "Accepted"
    );
    // L01.FR.24: the ongoing update is cancelled for the new one.
    assert_eq!(
        station.handle_call(&update(2, later, None, None), now),
        json!({"status":"AcceptedCanceled"})
    );
    let snapshot = station.snapshot();
    assert_eq!(
        (snapshot["requestId"].clone(), snapshot["cancelled"].clone()),
        (json!(2), json!([1]))
    );
    assert_eq!(drain(&station), [("DownloadScheduled".to_owned(), 2)]);

    // L01.FR.27: a station unable to cancel rejects and keeps its update.
    let stubborn = non_secure(json!({"cancel_policy": "reject"}));
    assert_eq!(
        stubborn.handle_call(&update(5, later, None, None), now)["status"],
        "Accepted"
    );
    assert_eq!(
        stubborn.handle_call(&update(6, later, None, None), now),
        json!({"status":"Rejected","statusInfo":{"reasonCode":"UnableToCancel"}})
    );
    assert_eq!(stubborn.snapshot()["requestId"], 5);

    let refusing = non_secure(json!({"reject_updates": true}));
    assert_eq!(
        refusing.handle_call(&update(7, now, None, None), now)["status"],
        "Rejected"
    );
    assert_eq!(refusing.snapshot()["active"], false);
}

#[test]
fn installation_waits_for_transactions_and_reports_configured_failures() {
    let now = OffsetDateTime::now_utc();
    let busy = StationFacts201 {
        idle: false,
        boot_generation: Some(1),
    };
    let station = non_secure(json!({"fail_install": true}));
    let _ = station.handle_call(&update(1, now, None, None), now);
    download(&station, busy, Some(IMAGE));
    // L01.FR.06: no installation while a transaction is ongoing.
    assert_eq!(names(&drain(&station)), ["Downloading", "Downloaded"]);
    station.advance(now, up(1)).unwrap();
    assert_eq!(
        names(&drain(&station)),
        ["Installing", "InstallationFailed"]
    );

    let pki = TestPki::generate();
    let verifying = secure(&pki, json!({"fail_install_verification": true}));
    let _ = verifying.handle_call(&signed_update(&pki, 2, now), now);
    download(&verifying, up(1), Some(IMAGE));
    // L01.FR.29 and Figure 116: verification fails while installing.
    assert_eq!(
        names(&drain(&verifying)),
        [
            "Downloading",
            "Downloaded",
            "SignatureVerified",
            "Installing",
            "InstallVerificationFailed"
        ]
    );
}

#[test]
fn triggered_status_is_idle_after_installed_otherwise_the_last_sent_status() {
    let station = non_secure(json!({"reboot_after_install": false}));
    let now = OffsetDateTime::now_utc();
    assert_eq!(station.trigger_payload(), json!({"status":"Idle"}));
    let _ = station.handle_call(&update(12, now, None, None), now);
    let ticket = station.advance(now, up(1)).unwrap().download.unwrap();
    // Queued but undelivered statuses are not "last sent" yet.
    assert_eq!(station.trigger_payload(), json!({"status":"Idle"}));
    drain(&station);
    // L01.FR.26: the last sent status with its requestId.
    assert_eq!(
        station.trigger_payload(),
        json!({"status":"Downloading","requestId":12})
    );
    station
        .download_finished(&ticket, Some(IMAGE), now)
        .unwrap();
    station.advance(now, up(1)).unwrap();
    drain(&station);
    // L01.FR.25: Idle once Installed was the last status sent.
    assert_eq!(station.snapshot()["lastStatus"], "Installed");
    assert_eq!(station.trigger_payload(), json!({"status":"Idle"}));
}

#[test]
fn undelivered_statuses_are_retried_in_order_and_never_duplicated() {
    let station = non_secure(json!({}));
    let now = OffsetDateTime::now_utc();
    let _ = station.handle_call(&update(3, now, None, None), now);
    station.advance(now, up(1)).unwrap();
    let first = station.next_status(now).unwrap();
    // One CALL at a time.
    assert!(station.next_status(now).is_none());
    station.status_finished(&first, false, now).unwrap();
    assert!(station.next_status(now).is_none());
    let retry = station.next_status(now + Duration::seconds(2)).unwrap();
    assert_eq!(retry, first);
    station.status_finished(&retry, true, now).unwrap();
    station.status_finished(&retry, true, now).unwrap();
    assert_eq!(station.snapshot()["statuses"], json!(["Downloading"]));
}
