//! Native firmware station semantics: OCPP 1.6 Edition 2 §5.19 and Security Whitepaper Ed4 L01.
mod firmware16_support;
use firmware16_support::model::{
    IMAGE, config, download, drain, legacy, names, signed, signed_update, up, update,
};
use firmware16_support::{TestPki, private_directory, timestamp};
use serde_json::json;
use time::{Duration, OffsetDateTime};
use uob_sim::firmware16::{FirmwareConfig, FirmwareEffects, FirmwareHandle, StationFacts};

#[test]
fn legacy_download_install_reboot_reports_every_native_step() {
    let handle = legacy(json!({}));
    let now = OffsetDateTime::now_utc();
    assert_eq!(
        handle.handle_call(
            "UpdateFirmware",
            &update(now - Duration::minutes(1), json!({})),
            now
        ),
        json!({})
    );
    download(&handle, up(1), Some(IMAGE));
    assert_eq!(
        names(&drain(&handle)),
        ["Downloading", "Downloaded", "Installing"]
    );
    let reboot = handle
        .advance(now, up(1))
        .unwrap()
        .reboot
        .expect("reboot after Installing");
    assert!(reboot.starts_with("sha256-"));
    assert_eq!(
        handle.advance(now, up(1)).unwrap(),
        FirmwareEffects::default()
    );
    handle.advance(now, up(2)).unwrap();
    assert_eq!(names(&drain(&handle)), ["Installed"]);
    let snapshot = handle.snapshot();
    assert_eq!(snapshot["active"], false);
    assert_eq!(snapshot["reboots"], 1);
    assert_eq!(snapshot["installedVersion"], reboot);
    assert_eq!(
        snapshot["statuses"],
        json!(["Downloading", "Downloaded", "Installing", "Installed"])
    );
    assert_eq!(handle.trigger_status().as_deref(), Some("Idle"));
}

#[test]
fn legacy_waits_for_retrieve_date_and_ending_transactions() {
    let handle = legacy(json!({"reboot_after_install": false}));
    let now = OffsetDateTime::now_utc();
    let later = now + Duration::minutes(5);
    assert_eq!(
        handle.handle_call("UpdateFirmware", &update(later, json!({})), now),
        json!({})
    );
    assert!(handle.advance(now, up(1)).unwrap().download.is_none());
    assert!(
        drain(&handle).is_empty(),
        "legacy 1.6 has no scheduled status"
    );
    assert_eq!(
        handle.trigger_status(),
        None,
        "busy before any reportable status"
    );
    let busy = StationFacts {
        idle: false,
        boot_generation: Some(1),
    };
    let ticket = handle.advance(later, busy).unwrap().download.unwrap();
    handle
        .download_finished(&ticket, Some(IMAGE), later)
        .unwrap();
    handle.advance(later, busy).unwrap();
    assert_eq!(names(&drain(&handle)), ["Downloading", "Downloaded"]);
    assert_eq!(handle.trigger_status().as_deref(), Some("Downloaded"));
    handle.advance(later, up(1)).unwrap();
    assert_eq!(names(&drain(&handle)), ["Installing", "Installed"]);
}

#[test]
fn retries_send_downloading_per_attempt_then_download_failed() {
    let handle = legacy(json!({"download_failures": 1}));
    let now = OffsetDateTime::now_utc();
    let payload = update(now, json!({"retries": 2, "retryInterval": 0}));
    assert_eq!(
        handle.handle_call("UpdateFirmware", &payload, now),
        json!({})
    );
    for attempt in 1..=3 {
        let ticket = handle.advance(now, up(1)).unwrap().download.unwrap();
        assert_eq!(ticket.attempt, attempt);
        assert_eq!(ticket.simulated_failure, attempt == 1);
        handle.download_finished(&ticket, None, now).unwrap();
    }
    assert_eq!(
        names(&drain(&handle)),
        [
            "Downloading",
            "Downloading",
            "Downloading",
            "DownloadFailed"
        ]
    );
    assert_eq!(handle.snapshot()["active"], false);
}

#[test]
fn signed_flow_carries_request_id_and_reports_reboot() {
    let pki = TestPki::generate();
    let handle = signed(&pki, json!({}));
    let now = OffsetDateTime::now_utc();
    let reply = handle.handle_call(
        "SignedUpdateFirmware",
        &signed_update(&pki, 77, now, None),
        now,
    );
    assert_eq!(reply, json!({"status": "Accepted"}));
    download(&handle, up(1), Some(IMAGE));
    let sent = drain(&handle);
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
    assert!(sent.iter().all(|(_, id)| *id == Some(77)));
    assert!(handle.advance(now, up(1)).unwrap().reboot.is_some());
    handle.advance(now, up(2)).unwrap();
    assert_eq!(drain(&handle), [("Installed".to_owned(), Some(77))]);
    assert_eq!(handle.snapshot()["requestId"], 77);
}

#[test]
fn certificate_signature_and_policy_failures_follow_l01() {
    let pki = TestPki::generate();
    let stranger = TestPki::generate();
    let now = OffsetDateTime::now_utc();
    let handle = signed(&pki, json!({}));
    let untrusted = signed_update(&stranger, 1, now, None);
    assert_eq!(
        handle.handle_call("SignedUpdateFirmware", &untrusted, now),
        json!({"status":"InvalidCertificate"})
    );
    assert_eq!(handle.snapshot()["active"], false);
    assert_eq!(
        handle.handle_call(
            "SignedUpdateFirmware",
            &signed_update(&pki, 2, now, None),
            now
        )["status"],
        "Accepted"
    );
    download(&handle, up(1), Some(b"tampered image"));
    assert_eq!(
        names(&drain(&handle)),
        ["Downloading", "Downloaded", "InvalidSignature"]
    );

    let rejecting = signed(&pki, json!({"reject_signed": true}));
    assert_eq!(
        rejecting.handle_call(
            "SignedUpdateFirmware",
            &signed_update(&pki, 3, now, None),
            now
        )["status"],
        "Rejected"
    );
    let verifying = signed(&pki, json!({"fail_install_verification": true}));
    assert_eq!(
        verifying.handle_call(
            "SignedUpdateFirmware",
            &signed_update(&pki, 4, now, None),
            now
        )["status"],
        "Accepted"
    );
    download(&verifying, up(1), Some(IMAGE));
    assert_eq!(
        names(&drain(&verifying)),
        [
            "Downloading",
            "Downloaded",
            "SignatureVerified",
            "InstallVerificationFailed"
        ]
    );
    let failing = legacy(json!({"fail_install": true}));
    assert_eq!(
        failing.handle_call("UpdateFirmware", &update(now, json!({})), now),
        json!({})
    );
    download(&failing, up(1), Some(IMAGE));
    assert_eq!(
        names(&drain(&failing)),
        [
            "Downloading",
            "Downloaded",
            "Installing",
            "InstallationFailed"
        ]
    );
}

#[test]
fn scheduled_signed_download_and_install_are_reported_before_waiting() {
    let pki = TestPki::generate();
    let handle = signed(&pki, json!({"reboot_after_install": false}));
    let now = OffsetDateTime::now_utc();
    let retrieve = now + Duration::minutes(1);
    let install = now + Duration::minutes(2);
    assert_eq!(
        handle.handle_call(
            "SignedUpdateFirmware",
            &signed_update(&pki, 9, retrieve, Some(install)),
            now
        )["status"],
        "Accepted"
    );
    assert!(handle.advance(now, up(1)).unwrap().download.is_none());
    assert_eq!(names(&drain(&handle)), ["DownloadScheduled"]);
    let ticket = handle.advance(retrieve, up(1)).unwrap().download.unwrap();
    handle
        .download_finished(&ticket, Some(IMAGE), retrieve)
        .unwrap();
    handle.advance(retrieve, up(1)).unwrap();
    assert_eq!(
        names(&drain(&handle)),
        [
            "Downloading",
            "Downloaded",
            "SignatureVerified",
            "InstallScheduled"
        ]
    );
    handle.advance(install, up(1)).unwrap();
    assert_eq!(names(&drain(&handle)), ["Installing", "Installed"]);
}

#[test]
fn ongoing_signed_update_is_cancelled_or_kept_by_policy() {
    let pki = TestPki::generate();
    let now = OffsetDateTime::now_utc();
    let later = now + Duration::hours(1);
    let cancelling = signed(&pki, json!({}));
    assert_eq!(
        cancelling.handle_call(
            "SignedUpdateFirmware",
            &signed_update(&pki, 10, later, None),
            now
        )["status"],
        "Accepted"
    );
    let reply = cancelling.handle_call(
        "SignedUpdateFirmware",
        &signed_update(&pki, 11, later, None),
        now,
    );
    assert_eq!(reply, json!({"status":"AcceptedCanceled"}));
    assert_eq!(cancelling.snapshot()["cancelled"], json!([10]));
    assert_eq!(cancelling.snapshot()["requestId"], 11);
    let keeping = signed(&pki, json!({"cancel_policy": "reject"}));
    assert_eq!(
        keeping.handle_call(
            "SignedUpdateFirmware",
            &signed_update(&pki, 12, later, None),
            now
        )["status"],
        "Accepted"
    );
    assert_eq!(
        keeping.handle_call(
            "SignedUpdateFirmware",
            &signed_update(&pki, 13, later, None),
            now
        )["status"],
        "Rejected"
    );
    assert_eq!(keeping.snapshot()["requestId"], 12);
    assert_eq!(keeping.snapshot()["active"], true);
}

#[test]
fn editions_and_payloads_are_strict() {
    let pki = TestPki::generate();
    let now = OffsetDateTime::now_utc();
    let legacy_station = legacy(json!({}));
    let signed_station = signed(&pki, json!({}));
    assert_eq!(
        legacy_station.handle_call(
            "SignedUpdateFirmware",
            &signed_update(&pki, 1, now, None),
            now
        ),
        json!({"callError":"NotImplemented"})
    );
    assert_eq!(
        signed_station.handle_call("UpdateFirmware", &update(now, json!({})), now),
        json!({"callError":"NotSupported"})
    );
    assert_eq!(
        signed_station.snapshot()["active"],
        false,
        "L01.FR.20 never starts"
    );
    for (payload, code) in [
        (update(now, json!({"unknown": 1})), "FormationViolation"),
        (update(now, json!({"retries": null})), "FormationViolation"),
        (
            update(now, json!({"retries": -1})),
            "PropertyConstraintViolation",
        ),
        (
            update(now, json!({"retryInterval": 2_147_483_648_i64})),
            "PropertyConstraintViolation",
        ),
        (
            update(now, json!({"retrieveDate": "tomorrow"})),
            "PropertyConstraintViolation",
        ),
        (
            json!({"retrieveDate": timestamp(now)}),
            "FormationViolation",
        ),
    ] {
        assert_eq!(
            legacy_station.handle_call("UpdateFirmware", &payload, now)["callError"],
            code
        );
    }
    let mut oversized = signed_update(&pki, 1, now, None);
    oversized["firmware"]["location"] = "x".repeat(513).into();
    let mut overflow = signed_update(&pki, 1, now, None);
    overflow["requestId"] = 2_147_483_648_i64.into();
    let mut nested = signed_update(&pki, 1, now, None);
    nested["firmware"]["extra"] = true.into();
    for (payload, code) in [
        (oversized, "PropertyConstraintViolation"),
        (overflow, "PropertyConstraintViolation"),
        (nested, "FormationViolation"),
    ] {
        assert_eq!(
            signed_station.handle_call("SignedUpdateFirmware", &payload, now)["callError"],
            code
        );
    }
    assert_eq!(signed_station.snapshot()["active"], false);
}

#[test]
fn durable_state_resumes_downloads_and_completes_reboot_after_restart() {
    let directory = private_directory("firmware-model");
    let file = directory.join("firmware.json");
    let options = config(json!({"private_state_file": file.to_str().unwrap()}));
    let now = OffsetDateTime::now_utc();
    let handle = FirmwareHandle::open("alpha", &options).unwrap();
    assert!(
        FirmwareHandle::open("alpha", &options).is_err(),
        "exclusive owner"
    );
    assert_eq!(
        handle.handle_call("UpdateFirmware", &update(now, json!({})), now),
        json!({})
    );
    assert!(handle.advance(now, up(1)).unwrap().download.is_some());
    drop(handle);
    let resumed = FirmwareHandle::open("alpha", &options).unwrap();
    assert_eq!(
        resumed.snapshot()["pendingStatuses"],
        1,
        "undelivered status is durable"
    );
    let ticket = resumed
        .advance(now, up(1))
        .unwrap()
        .download
        .expect("restarted attempt");
    assert_eq!(ticket.attempt, 1);
    resumed
        .download_finished(&ticket, Some(IMAGE), now)
        .unwrap();
    resumed.advance(now, up(1)).unwrap();
    assert_eq!(
        names(&drain(&resumed)),
        ["Downloading", "Downloading", "Downloaded", "Installing"]
    );
    assert!(resumed.advance(now, up(1)).unwrap().reboot.is_some());
    drop(resumed);
    let rebooted = FirmwareHandle::open("alpha", &options).unwrap();
    rebooted.advance(now, up(1)).unwrap();
    assert_eq!(names(&drain(&rebooted)), ["Installed"]);
    assert_eq!(rebooted.snapshot()["reboots"], 1);
    drop(rebooted);
    let mut signed_options = options.clone();
    signed_options.mode = uob_sim::firmware16::FirmwareMode::Signed;
    signed_options.manufacturer_root_file =
        Some(directory.join("missing.pem").to_str().unwrap().to_owned());
    assert!(FirmwareHandle::open("alpha", &signed_options).is_err());
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn snapshots_and_debug_output_never_reveal_private_request_material() {
    let pki = TestPki::generate();
    let handle = signed(&pki, json!({}));
    let now = OffsetDateTime::now_utc();
    let payload = signed_update(&pki, 5, now, None);
    assert_eq!(
        handle.handle_call("SignedUpdateFirmware", &payload, now)["status"],
        "Accepted"
    );
    let ticket = handle.advance(now, up(1)).unwrap().download.unwrap();
    let rendered = format!("{} {handle:?} {ticket:?}", handle.snapshot());
    for private in [
        "signed.bin",
        "BEGIN CERTIFICATE",
        payload["firmware"]["signature"].as_str().unwrap(),
    ] {
        assert!(!rendered.contains(private), "leaked {private}");
    }
}

#[test]
fn configuration_bounds_and_edition_requirements_are_checked() {
    for invalid in [
        json!({"mode": "signed"}),
        json!({"manufacturer_root_file": "/tmp/root.pem"}),
        json!({"maximum_bytes": 0}),
        json!({"download_timeout_ms": 300_001}),
        json!({"status_delay_ms": 30_001}),
        json!({"download_failures": 17}),
        json!({"firmware_version_prefix": "x".repeat(21)}),
        json!({"fail_install_verification": true}),
        json!({"reject_signed": true}),
    ] {
        assert!(config(invalid).validate().is_err());
    }
    assert!(
        serde_json::from_value::<FirmwareConfig>(
            json!({"private_state_file":"/x","mode":"legacy","unknown":true})
        )
        .is_err()
    );
}
