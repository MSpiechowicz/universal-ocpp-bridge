use super::{Reservation201Config, Reservation201Handle, ReserveNowRequest};
use std::{fs, os::unix::fs::PermissionsExt};

#[test]
fn uncertain_directory_commit_never_serves_previous_ram_or_native_success() {
    let directory = std::env::temp_dir().join(format!(
        "uob-reservation201-uncertain-{}",
        uuid::Uuid::new_v4()
    ));
    fs::create_dir(&directory).unwrap();
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).unwrap();
    let directory = fs::canonicalize(directory).unwrap();
    let config = Reservation201Config {
        private_state_file: directory.join("state.json").to_str().unwrap().to_owned(),
        enabled: true,
        non_evse_specific: true,
        connector_types: vec![],
    };
    let handle = Reservation201Handle::open("alpha", &[(1, 1)], &config).unwrap();
    handle
        .0
        .lock()
        .storage
        .as_mut()
        .unwrap()
        .fail_directory_sync = true;
    let request: ReserveNowRequest = serde_json::from_value(serde_json::json!({
        "id":-114,"expiryDateTime":"2099-01-01T00:00:00Z","evseId":1,
        "idToken":{"idToken":"native-private","type":"Central"}
    }))
    .unwrap();
    assert_eq!(
        handle.reserve(request, time::OffsetDateTime::now_utc()),
        Err("reservation_commit_uncertain")
    );
    assert_eq!(handle.snapshot()["stateAvailable"], false);
    assert!(handle.take_statuses().is_empty());
    assert!(
        handle
            .cancel(-114, time::OffsetDateTime::now_utc())
            .is_err()
    );
    drop(handle);
    let recovered = Reservation201Handle::open("alpha", &[(1, 1)], &config).unwrap();
    assert_eq!(recovered.snapshot()["activeReservations"], 1);
    assert!(
        recovered
            .cancel(-114, time::OffsetDateTime::now_utc())
            .unwrap()
    );
    assert!(
        recovered.updates().is_empty(),
        "CSMS cancel never queues a status update"
    );
    drop(recovered);
    fs::remove_dir_all(directory).unwrap();
}
