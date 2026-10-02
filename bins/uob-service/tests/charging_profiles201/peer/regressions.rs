use super::{Peer, Reply};
use serde_json::{Value, json};
use std::{fs, os::unix::fs::PermissionsExt, path::PathBuf};

struct StateDirectory(PathBuf);
impl StateDirectory {
    fn new() -> Self {
        let path =
            std::env::temp_dir().join(format!("uob-profile201-peer-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&path).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        Self(path)
    }
    fn open(&self) -> Peer {
        Peer::open(self.0.join("profiles.json")).unwrap()
    }
}
impl Drop for StateDirectory {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

fn fixture(name: &str) -> Value {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/ocpp-fixtures/corpus/wire/2.0.1")
        .join(format!("profile-{name}.json"));
    serde_json::from_slice::<Value>(&fs::read(path).unwrap()).unwrap()[3].clone()
}

fn accepted(peer: &mut Peer, action: &str, value: &Value) {
    assert_eq!(
        peer.apply(action, value).unwrap(),
        Reply::Result(json!({"status":"Accepted"}))
    );
}

#[test]
fn native_id_replacement_and_nested_and_clear_survive_independent_reopen() {
    let directory = StateDirectory::new();
    let mut peer = directory.open();
    let mut first = fixture("weekly-evse");
    first["chargingProfile"]["validTo"] = json!("2026-01-02T00:00:00Z");
    accepted(&mut peer, "SetChargingProfile", &first);
    let mut second = first.clone();
    second["chargingProfile"]["id"] = json!(31);
    second["chargingProfile"]["validFrom"] = json!("2026-01-02T00:00:00Z");
    second["chargingProfile"]["validTo"] = json!("2026-01-03T00:00:00Z");
    accepted(&mut peer, "SetChargingProfile", &second);
    assert_eq!(peer.profiles(), &[first.clone(), second.clone()]);
    first["chargingProfile"]["chargingSchedule"][0]["chargingSchedulePeriod"][0]["limit"] =
        json!(0);
    accepted(&mut peer, "SetChargingProfile", &first);
    assert_eq!(
        directory.open().profiles(),
        &[second.clone(), first.clone()]
    );
    let nonmatch = json!({"chargingProfileCriteria":{"evseId":1,"stackLevel":2}});
    assert_eq!(
        peer.apply("ClearChargingProfile", &nonmatch).unwrap(),
        Reply::Result(json!({"status":"Unknown"}))
    );
    assert_eq!(directory.open().profiles(), &[second, first.clone()]);
    accepted(
        &mut peer,
        "ClearChargingProfile",
        &json!({"chargingProfileId":31}),
    );
    assert_eq!(directory.open().profiles(), &[first]);
    accepted(&mut peer, "ClearChargingProfile", &fixture("clear-filter"));
    assert_eq!(directory.open().profiles(), &[] as &[Value]);
}

#[test]
fn transaction_and_phase_are_exact_evse_associations_and_denials_do_not_mutate() {
    let directory = StateDirectory::new();
    let mut peer = directory.open();
    let phase = fixture("phase-evse");
    peer.set_transaction(2, Some("12345678-1234-1234-1234-123456789012"));
    peer.set_phase(1, true);
    assert_eq!(
        peer.apply("SetChargingProfile", &phase).unwrap(),
        Reply::Result(json!({"status":"Rejected"}))
    );
    peer.set_transaction(1, Some("12345678-1234-1234-1234-123456789012"));
    peer.set_phase(1, false);
    assert_eq!(
        peer.apply("SetChargingProfile", &phase).unwrap(),
        Reply::Result(json!({"status":"Rejected"}))
    );
    peer.set_phase(1, true);
    accepted(&mut peer, "SetChargingProfile", &phase);
    let mut invalid = phase.clone();
    invalid["chargingProfile"]["chargingSchedule"][0]["chargingSchedulePeriod"][0]["startPeriod"] =
        json!(1);
    assert_eq!(
        peer.apply("SetChargingProfile", &invalid).unwrap(),
        Reply::Result(json!({"status":"Rejected"}))
    );
    peer.controls.reject_next = true;
    assert_eq!(
        peer.apply("SetChargingProfile", &phase).unwrap(),
        Reply::Result(json!({"status":"Rejected"}))
    );
    assert_eq!(directory.open().profiles(), std::slice::from_ref(&phase));
    let mixed = json!({"chargingProfileId":23,"chargingProfileCriteria":{"evseId":1}});
    assert_eq!(
        peer.apply("ClearChargingProfile", &mixed).unwrap(),
        Reply::Error("FormationViolation")
    );
    assert_eq!(directory.open().profiles(), &[phase]);
}

#[test]
fn external_profiles_are_protected_and_capacity_never_evicts() {
    let directory = StateDirectory::new();
    let mut peer = directory.open();
    let external = fixture("external-protected");
    assert_eq!(
        peer.apply("SetChargingProfile", &external).unwrap(),
        Reply::Result(json!({"status":"Rejected"}))
    );
    peer.seed_external(external.clone()).unwrap();
    let mut default = fixture("weekly-evse");
    default["chargingProfile"]["id"] = external["chargingProfile"]["id"].clone();
    assert_eq!(
        peer.apply("SetChargingProfile", &default).unwrap(),
        Reply::Result(json!({"status":"Rejected"}))
    );
    assert_eq!(
        peer.apply("ClearChargingProfile", &fixture("clear-external"))
            .unwrap(),
        Reply::Result(json!({"status":"Unknown"}))
    );
    for index in 1..super::CAPACITY {
        default["chargingProfile"]["id"] = json!(1000 + index);
        default["chargingProfile"]["stackLevel"] = json!(index);
        accepted(&mut peer, "SetChargingProfile", &default);
    }
    let before = peer.profiles().to_vec();
    default["chargingProfile"]["id"] = json!(9999);
    default["chargingProfile"]["stackLevel"] = json!(9999);
    assert_eq!(
        peer.apply("SetChargingProfile", &default).unwrap(),
        Reply::Result(json!({"status":"Rejected"}))
    );
    assert_eq!(directory.open().profiles(), before);
    accepted(&mut peer, "ClearChargingProfile", &fixture("clear-default"));
    assert_eq!(directory.open().profiles(), &[external]);
}

#[test]
fn phase_query_reports_native_boolean_and_revokes_false_without_cross_evse_echo() {
    let directory = StateDirectory::new();
    let mut peer = directory.open();
    let query = json!({"getVariableData":[{"attributeType":"Actual",
        "component":{"name":"SmartChargingCtrlr","evse":{"id":1}},
        "variable":{"name":"ACPhaseSwitchingSupported"}}]});
    peer.set_phase(1, true);
    let Reply::Result(positive) = peer.apply("GetVariables", &query).unwrap() else {
        panic!()
    };
    assert_eq!(positive["getVariableResult"][0]["attributeValue"], "true");
    peer.set_phase(1, false);
    let Reply::Result(negative) = peer.apply("GetVariables", &query).unwrap() else {
        panic!()
    };
    assert_eq!(negative["getVariableResult"][0]["attributeValue"], "false");
    let mut other = query.clone();
    other["getVariableData"][0]["component"]["evse"]["id"] = json!(2);
    let Reply::Result(unknown) = peer.apply("GetVariables", &other).unwrap() else {
        panic!()
    };
    assert_eq!(
        unknown["getVariableResult"][0]["attributeStatus"],
        "UnknownVariable"
    );
    assert!(
        unknown["getVariableResult"][0]
            .get("attributeValue")
            .is_none()
    );
}

#[test]
fn exact_high_tenth_survives_peer_reopen_and_extra_precision_is_denied() {
    let directory = StateDirectory::new();
    let mut peer = directory.open();
    let mut profile = fixture("weekly-evse");
    let rate = "900719925474099.1";
    profile["chargingProfile"]["chargingSchedule"][0]["chargingSchedulePeriod"][0]["limit"] =
        serde_json::from_str(rate).unwrap();
    accepted(&mut peer, "SetChargingProfile", &profile);
    assert_eq!(directory.open().profiles()[0]["chargingProfile"]["chargingSchedule"][0]
        ["chargingSchedulePeriod"][0]["limit"].to_string(), rate);
    let saved = peer.profiles().to_vec();
    profile["chargingProfile"]["chargingSchedule"][0]["chargingSchedulePeriod"][0]["limit"] =
        serde_json::from_str("8.11").unwrap();
    assert_eq!(
        peer.apply("SetChargingProfile", &profile).unwrap(),
        Reply::Result(json!({"status":"Rejected"}))
    );
    assert_eq!(directory.open().profiles(), saved.as_slice());
}
