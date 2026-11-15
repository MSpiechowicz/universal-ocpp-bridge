mod gate;
mod redaction;

use super::{
    STANDARD_SECURITY_EVENTS_16, SecurityEventError16, SecurityEventObservation16,
    is_standard_security_event_16,
};
use uob_contracts::{ResourceRef, UtcTimestamp};

fn at(seconds: i64) -> UtcTimestamp {
    UtcTimestamp::new(time::OffsetDateTime::from_unix_timestamp(1_800_000_000 + seconds).unwrap())
}

#[test]
fn every_section_8_name_is_standard_and_matching_is_exact() {
    assert_eq!(STANDARD_SECURITY_EVENTS_16.len(), 21);
    for name in STANDARD_SECURITY_EVENTS_16 {
        assert!(is_standard_security_event_16(name), "{name}");
    }

    for other in [
        "firmwareupdated",
        "FirmwareUpdated ",
        "VendorSpecificEvent",
        "",
    ] {
        assert!(!is_standard_security_event_16(other), "{other:?}");
    }
}

#[test]
fn a_vendor_type_is_kept_verbatim_and_not_standard() {
    let observation = SecurityEventObservation16::new("Acme.DoorForced", at(0), None).unwrap();

    assert_eq!(observation.event_type(), "Acme.DoorForced");
    assert!(!observation.standard());
    assert_eq!(observation.tech_info(), None);
    assert_eq!(observation.tech_info_redactions(), 0);

    let standard = SecurityEventObservation16::new("ResetOrReboot", at(0), None).unwrap();
    assert!(standard.standard());
    assert!(standard.into_record(0).standard);
}

#[test]
fn native_bounds_are_enforced_in_characters_not_bytes() {
    let longest_type = "é".repeat(50);
    assert!(SecurityEventObservation16::new(&longest_type, at(0), None).is_ok());
    assert!(SecurityEventObservation16::new("", at(0), None).is_ok());
    assert_eq!(
        SecurityEventObservation16::new(&"é".repeat(51), at(0), None),
        Err(SecurityEventError16::TypeTooLong)
    );

    let longest_info = "ü".repeat(255);
    assert!(SecurityEventObservation16::new("X", at(0), Some(&longest_info)).is_ok());
    assert_eq!(
        SecurityEventObservation16::new("X", at(0), Some(&"ü".repeat(256))),
        Err(SecurityEventError16::TechInfoTooLong)
    );
}

#[test]
fn the_observation_holds_only_redacted_text_even_in_debug_output() {
    let observation = SecurityEventObservation16::new(
        "InvalidMessages",
        at(0),
        Some("login password=not-a-real-password-0000 failed"),
    )
    .unwrap();

    assert_eq!(
        observation.tech_info(),
        Some("login password=[redacted] failed")
    );
    assert_eq!(observation.tech_info_redactions(), 1);
    assert!(!format!("{observation:?}").contains("not-a-real-password"));
}

#[test]
fn the_record_carries_the_observation_and_the_reported_shed_count() {
    let observation =
        SecurityEventObservation16::new("ResetOrReboot", at(3), Some("token=abc")).unwrap();
    let record = observation.into_record(4);

    assert_eq!(record.event_type, "ResetOrReboot");
    assert!(record.standard);
    assert_eq!(record.timestamp, at(3));
    assert_eq!(record.tech_info.as_deref(), Some("token=[redacted]"));
    assert_eq!(record.tech_info_redactions, 1);
    assert_eq!(record.shed_calls, 4);
}

#[test]
fn the_dedup_key_is_station_type_timestamp_and_redacted_text() {
    let station: ResourceRef = serde_json::from_value(serde_json::json!({
        "bridge_id": "bridge-berlin-1",
        "station_id": "station-1"
    }))
    .unwrap();

    let first = SecurityEventObservation16::new("X", at(0), Some("password=one1")).unwrap();
    let second = SecurityEventObservation16::new("X", at(0), Some("password=two2")).unwrap();
    let later = SecurityEventObservation16::new("X", at(1), Some("password=one1")).unwrap();

    assert_eq!(first.key(station.clone()), second.key(station.clone()));
    assert_ne!(first.key(station.clone()), later.key(station));
}
