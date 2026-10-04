use super::*;
use uob_contracts::{DeviceEvse201, DeviceValue201};

fn component(name: &str) -> DeviceComponent201 {
    DeviceComponent201 {
        name: name.to_owned(),
        instance: None,
        evse: None,
    }
}
fn variable(name: &str) -> DeviceVariable201 {
    DeviceVariable201 {
        name: name.to_owned(),
        instance: None,
    }
}
#[test]
fn only_exact_generation_local_typed_facts_are_safe_to_learn() {
    let c = component("LocalAuthListCtrlr");
    for (name, text) in [
        ("Entries", "0"),
        ("ItemsPerMessage", "3"),
        ("BytesPerMessage", "4096"),
        ("Enabled", "true"),
        ("Available", "false"),
        ("SupportsExpiryDateTime", "true"),
    ] {
        assert!(safe_value(&c, &variable(name), text));
        assert!(!safe_value(&c, &variable(name), "PRIVATE-BAD-VALUE"));
    }
    let v = variable("Entries");
    let mut wrong = c.clone();
    wrong.instance = Some("SendLocalList".to_owned());
    assert!(identity(&wrong, &v).is_none());
    wrong = c.clone();
    wrong.evse = Some(DeviceEvse201 {
        id: 1,
        connector_id: None,
    });
    assert!(identity(&wrong, &v).is_none());
    let mut wrong_variable = v.clone();
    wrong_variable.instance = Some("SendLocalList".to_owned());
    assert!(identity(&c, &wrong_variable).is_none());
    assert!(identity(&component("DeviceDataCtrlr"), &wrong_variable).is_none());
    assert!(!safe_value(&c, &v, "2147483648"));
    assert!(safe_value(
        &component("localauthlistctrlr"),
        &variable("entries"),
        "0"
    ));
}
#[test]
fn entries_actual_zero_is_count_not_full_capacity_and_new_session_starts_unknown() {
    let mut limits = LocalListLimits201::default();
    limits.learn(
        &component("LocalAuthListCtrlr"),
        &variable("Entries"),
        DeviceAttributeType201::Actual,
        &DeviceValue201 {
            present: true,
            redacted: false,
            empty: false,
            value: Some("0".to_owned()),
        },
    );
    assert_eq!(limits.entries_actual, Some(0));
    assert_eq!(limits.capacity, None);
    assert!(limits.allows(256, 256));
    assert_eq!(LocalListLimits201::default().entries_actual, None);
}
