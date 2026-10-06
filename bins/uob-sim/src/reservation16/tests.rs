use super::{ReservationConfig, ReservationHandle, ReserveRequest};
use std::{fs, os::unix::fs::PermissionsExt};

#[test]
fn uncertain_directory_commit_never_serves_previous_ram_or_native_success() {
    let directory = std::env::temp_dir().join(format!(
        "uob-reservation-uncertain-{}",
        uuid::Uuid::new_v4()
    ));
    fs::create_dir(&directory).unwrap();
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).unwrap();
    let directory = fs::canonicalize(directory).unwrap();
    let config = ReservationConfig {
        private_state_file: directory.join("state.json").to_str().unwrap().to_owned(),
        enabled: true,
        reserve_connector_zero_supported: true,
    };
    let handle = ReservationHandle::open("alpha", &[1], &config).unwrap();
    handle
        .0
        .lock()
        .storage
        .as_mut()
        .unwrap()
        .fail_directory_sync = true;
    let request: ReserveRequest = serde_json::from_value(serde_json::json!({"connectorId":1,"expiryDate":"2099-01-01T00:00:00Z","idTag":"native-private","reservationId":-113})).unwrap();
    assert_eq!(
        handle.reserve(request, time::OffsetDateTime::now_utc()),
        Err("reservation_commit_uncertain")
    );
    assert_eq!(handle.snapshot()["stateAvailable"], false);
    assert!(
        handle
            .cancel(-113, time::OffsetDateTime::now_utc())
            .is_err()
    );
    drop(handle);
    let recovered = ReservationHandle::open("alpha", &[1], &config).unwrap();
    assert_eq!(recovered.snapshot()["activeReservations"], 1);
    assert!(
        recovered
            .cancel(-113, time::OffsetDateTime::now_utc())
            .unwrap()
    );
    drop(recovered);
    fs::remove_dir_all(directory).unwrap();
}
