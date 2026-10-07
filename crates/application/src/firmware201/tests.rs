use super::{FirmwareJobRecord201, FirmwareTransition201, apply_firmware_status_201};
use uob_contracts::{
    BridgeId, FirmwareJobState201, FirmwareStatus201, RequestId, ResourceRef, StationId,
    UtcTimestamp,
};

fn at(seconds: i64) -> UtcTimestamp {
    UtcTimestamp::new(time::OffsetDateTime::from_unix_timestamp(1_800_000_000 + seconds).unwrap())
}

fn job(secure: bool) -> FirmwareJobRecord201 {
    FirmwareJobRecord201 {
        station: ResourceRef {
            bridge_id: BridgeId::new("bridge").unwrap(),
            station_id: StationId::new("alpha").unwrap(),
            resource: None,
            native_protocol_reference: None,
        },
        request_id: RequestId::new("firmware-1").unwrap(),
        native_request_id: 7,
        secure,
        artifact_reference: "image-1".to_owned(),
        revision: 1,
        state: FirmwareJobState201::Accepted,
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
    record: &mut FirmwareJobRecord201,
    statuses: &[FirmwareStatus201],
) -> Vec<FirmwareTransition201> {
    statuses
        .iter()
        .enumerate()
        .map(|(index, status)| {
            apply_firmware_status_201(record, *status, at(i64::try_from(index).unwrap() + 1))
        })
        .collect()
}

#[test]
fn secure_sequence_alternates_within_phase_but_never_regresses() {
    use FirmwareStatus201::{
        DownloadPaused, DownloadScheduled, Downloaded, Downloading, InstallRebooting,
        InstallScheduled, Installed, Installing, SignatureVerified,
    };
    let mut record = job(true);
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
            Installing,
            InstallRebooting,
        ],
    );
    assert!(
        outcomes
            .iter()
            .all(|o| *o == FirmwareTransition201::Advanced)
    );
    assert_eq!(record.state, FirmwareJobState201::InstallRebooting);
    for regression in [Downloading, DownloadScheduled, Downloaded, InstallScheduled] {
        assert_eq!(
            apply_firmware_status_201(&mut record, regression, at(20)),
            FirmwareTransition201::Rejected
        );
    }
    assert_eq!(record.rejected_transitions, 4);
    assert_eq!(record.state, FirmwareJobState201::InstallRebooting);
    assert_eq!(
        apply_firmware_status_201(&mut record, Installed, at(21)),
        FirmwareTransition201::Advanced
    );
    assert!(record.state.resolved());
    assert_eq!(record.notifications, 14);
    assert_eq!(
        apply_firmware_status_201(&mut record, Downloading, at(22)),
        FirmwareTransition201::Late
    );
    assert_eq!(record.state, FirmwareJobState201::Installed);
    assert_eq!(record.last_status_at, Some(at(21)));
}

#[test]
fn non_secure_update_refuses_signature_statuses() {
    use FirmwareStatus201::{Downloaded, Downloading, Installed, Installing, SignatureVerified};
    let mut record = job(false);
    run(&mut record, &[Downloading, Downloaded]);
    assert_eq!(
        apply_firmware_status_201(&mut record, SignatureVerified, at(5)),
        FirmwareTransition201::Rejected
    );
    assert_eq!(
        apply_firmware_status_201(&mut record, FirmwareStatus201::InvalidSignature, at(6)),
        FirmwareTransition201::Rejected
    );
    assert_eq!(record.state, FirmwareJobState201::Downloaded);
    run(&mut record, &[Installing, Installed]);
    assert_eq!(record.state, FirmwareJobState201::Installed);
    assert_eq!(record.rejected_transitions, 2);
}

#[test]
fn failure_end_states_must_match_reported_progress() {
    use FirmwareStatus201::{
        DownloadFailed, Downloaded, Downloading, InstallVerificationFailed, Installing,
        InvalidSignature, SignatureVerified,
    };
    let mut record = job(true);
    run(&mut record, &[Downloading, Downloaded]);
    assert_eq!(
        apply_firmware_status_201(&mut record, DownloadFailed, at(9)),
        FirmwareTransition201::Rejected
    );
    run(&mut record, &[SignatureVerified]);
    assert_eq!(
        apply_firmware_status_201(&mut record, InvalidSignature, at(10)),
        FirmwareTransition201::Rejected
    );
    run(&mut record, &[Installing]);
    assert_eq!(
        apply_firmware_status_201(&mut record, InstallVerificationFailed, at(11)),
        FirmwareTransition201::Advanced
    );
    assert_eq!(record.state, FirmwareJobState201::InstallVerificationFailed);
    assert!(record.state.resolved());

    let mut failed = job(true);
    run(&mut failed, &[Downloading, DownloadFailed]);
    assert_eq!(failed.state, FirmwareJobState201::DownloadFailed);
    let mut invalid = job(true);
    run(&mut invalid, &[Downloading, Downloaded, InvalidSignature]);
    assert_eq!(invalid.state, FirmwareJobState201::InvalidSignature);
}

#[test]
fn idle_resolves_without_claiming_success_or_overwriting_progress() {
    let mut record = job(true);
    run(&mut record, &[FirmwareStatus201::Downloading]);
    assert_eq!(
        apply_firmware_status_201(&mut record, FirmwareStatus201::Idle, at(30)),
        FirmwareTransition201::Advanced
    );
    assert_eq!(record.state, FirmwareJobState201::StationIdle);
    assert!(record.state.resolved());
    assert_eq!(record.last_status, Some(FirmwareStatus201::Downloading));
    assert_eq!(record.changed_at, at(30));
}
