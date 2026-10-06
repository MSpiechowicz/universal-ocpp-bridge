use super::{
    FirmwareJobRecord16, FirmwareTransition16, FirmwareVariant16, apply_firmware_status_16,
};
use uob_contracts::{
    BridgeId, FirmwareJobState16, FirmwareStatus16, RequestId, ResourceRef, StationId, UtcTimestamp,
};

fn at(seconds: i64) -> UtcTimestamp {
    UtcTimestamp::new(time::OffsetDateTime::from_unix_timestamp(1_800_000_000 + seconds).unwrap())
}

fn job(variant: FirmwareVariant16) -> FirmwareJobRecord16 {
    FirmwareJobRecord16 {
        station: ResourceRef {
            bridge_id: BridgeId::new("bridge").unwrap(),
            station_id: StationId::new("alpha").unwrap(),
            resource: None,
            native_protocol_reference: None,
        },
        request_id: RequestId::new("firmware-1").unwrap(),
        variant,
        artifact_reference: "image-1".to_owned(),
        revision: 1,
        state: FirmwareJobState16::Accepted,
        admitted_at: at(0),
        changed_at: at(0),
        deadline: at(3600),
        started: true,
        last_status: None,
        last_status_at: None,
        notifications: 0,
        rejected_transitions: 0,
    }
}

fn run(
    record: &mut FirmwareJobRecord16,
    statuses: &[FirmwareStatus16],
) -> Vec<FirmwareTransition16> {
    statuses
        .iter()
        .enumerate()
        .map(|(index, status)| {
            apply_firmware_status_16(record, *status, at(i64::try_from(index).unwrap() + 1))
        })
        .collect()
}

#[test]
fn legacy_sequence_with_retries_and_reboot_reaches_installed() {
    use FirmwareStatus16::{Downloaded, Downloading, Installed, Installing};
    let mut record = job(FirmwareVariant16::Legacy);
    let outcomes = run(
        &mut record,
        &[Downloading, Downloading, Downloaded, Installing, Installed],
    );
    assert!(
        outcomes
            .iter()
            .all(|o| *o == FirmwareTransition16::Advanced)
    );
    assert_eq!(record.state, FirmwareJobState16::Installed);
    assert!(record.state.resolved());
    assert_eq!(record.notifications, 5);
    assert_eq!(record.last_status_at, Some(at(5)));
    assert_eq!(
        apply_firmware_status_16(&mut record, Downloading, at(9)),
        FirmwareTransition16::Late
    );
    assert_eq!(record.state, FirmwareJobState16::Installed);
}

#[test]
fn signed_sequence_alternates_within_phase_but_never_regresses() {
    use FirmwareStatus16::{
        DownloadPaused, DownloadScheduled, Downloaded, Downloading, InstallRebooting,
        InstallScheduled, Installed, Installing, SignatureVerified,
    };
    let mut record = job(FirmwareVariant16::Signed { request_id: 7 });
    let outcomes = run(
        &mut record,
        &[
            DownloadScheduled,
            Downloading,
            DownloadPaused,
            Downloading,
            Downloaded,
            SignatureVerified,
            InstallScheduled,
            InstallRebooting,
            Installing,
        ],
    );
    assert!(
        outcomes
            .iter()
            .all(|o| *o == FirmwareTransition16::Advanced)
    );
    assert_eq!(record.state, FirmwareJobState16::Installing);
    for regression in [Downloading, DownloadScheduled, Downloaded, InstallScheduled] {
        assert_eq!(
            apply_firmware_status_16(&mut record, regression, at(20)),
            FirmwareTransition16::Rejected
        );
    }
    assert_eq!(record.state, FirmwareJobState16::Installing);
    assert_eq!(record.rejected_transitions, 4);
    assert_eq!(
        apply_firmware_status_16(&mut record, Installed, at(21)),
        FirmwareTransition16::Advanced
    );
    assert_eq!(record.state, FirmwareJobState16::Installed);
}

#[test]
fn failure_end_states_must_match_reported_progress() {
    use FirmwareStatus16::{
        DownloadFailed, Downloaded, Downloading, InstallVerificationFailed, InvalidSignature,
        SignatureVerified,
    };
    let mut record = job(FirmwareVariant16::Signed { request_id: 1 });
    run(&mut record, &[Downloading, Downloaded, SignatureVerified]);
    assert_eq!(
        apply_firmware_status_16(&mut record, DownloadFailed, at(10)),
        FirmwareTransition16::Rejected
    );
    assert_eq!(
        apply_firmware_status_16(&mut record, InvalidSignature, at(11)),
        FirmwareTransition16::Rejected
    );
    assert_eq!(
        apply_firmware_status_16(&mut record, InstallVerificationFailed, at(12)),
        FirmwareTransition16::Advanced
    );
    assert_eq!(record.state, FirmwareJobState16::InstallVerificationFailed);

    let mut download = job(FirmwareVariant16::Legacy);
    run(&mut download, &[Downloading, DownloadFailed]);
    assert_eq!(download.state, FirmwareJobState16::DownloadFailed);
    assert!(download.state.resolved());
}

#[test]
fn legacy_jobs_refuse_signed_only_statuses_and_idle_resolves_without_success() {
    let mut record = job(FirmwareVariant16::Legacy);
    for status in [
        FirmwareStatus16::DownloadScheduled,
        FirmwareStatus16::SignatureVerified,
        FirmwareStatus16::InstallRebooting,
        FirmwareStatus16::InvalidSignature,
    ] {
        assert_eq!(
            apply_firmware_status_16(&mut record, status, at(1)),
            FirmwareTransition16::Rejected
        );
    }
    assert_eq!(record.state, FirmwareJobState16::Accepted);
    record.state = FirmwareJobState16::TimedOut;
    assert_eq!(
        apply_firmware_status_16(&mut record, FirmwareStatus16::Idle, at(2)),
        FirmwareTransition16::Advanced
    );
    assert_eq!(record.state, FirmwareJobState16::StationIdle);
    assert_eq!(record.last_status, None);
    assert!(record.state.resolved());
}

#[test]
fn timed_out_and_uncertain_jobs_still_accept_later_native_progress() {
    for state in [FirmwareJobState16::TimedOut, FirmwareJobState16::Uncertain] {
        let mut record = job(FirmwareVariant16::Legacy);
        record.state = state;
        assert!(!state.resolved());
        assert_eq!(
            apply_firmware_status_16(&mut record, FirmwareStatus16::Installed, at(5)),
            FirmwareTransition16::Advanced
        );
        assert_eq!(record.state, FirmwareJobState16::Installed);
    }
}
