pub use crate::host::{
    CONTROL, Fixture, PRIVILEGED, READ, Socket, begin, boot, client, completed, no_call, receive,
    result, send, station_call,
};
use serde_json::{Value, json};
use std::{fs, os::unix::fs::PermissionsExt};
pub const TOKEN: &str = "PRIVATE-RESERVE-113";
pub const PARENT: &str = "PRIVATE-PARENT-113";
pub const SIBLING: &str = "PRIVATE-SIBLING-113";
pub const UNGRANTED: &str = "PRIVATE-NOGRT-113";
pub fn reference(index: usize) -> String {
    format!("reserve16:{index:064x}")
}
pub fn native(index: usize) -> Value {
    let id = match index {
        1 => i32::MIN,
        2 => 0,
        3 => i32::MAX,
        _ => -i32::try_from(index).unwrap(),
    };
    let connector = i32::from(index != 6);
    json!({"connectorId":connector,"expiryDate":"2099-01-01T00:00:00Z","reservationId":id,"idTag":TOKEN,"parentIdTag":PARENT})
}
pub fn provisioning(revoked: bool) -> Value {
    let reservations: Vec<_> = (1..=9).map(|index| json!({"reference":reference(index),"request":native(index),"expires_at":"2099-01-01T00:00:00Z","revoked":false})).collect();
    json!({"reservations":reservations,"identities":[
        {"idTag":TOKEN,"parentIdTag":PARENT,"authorize":!revoked,"policy_revision":if revoked {2} else {1}},
        {"idTag":SIBLING,"parentIdTag":"private-parent-113","authorize":true},
        {"idTag":UNGRANTED,"parentIdTag":PARENT,"authorize":false}
    ]})
}
pub fn save(fixture: &Fixture, value: &Value) {
    let path = fixture.root.join("reservations.json");
    fs::write(&path, value.to_string()).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
}
pub fn fixture(zero: bool) -> Fixture {
    let fixture = Fixture::new();
    let path = fixture.root.join("bridge.toml");
    let config = fs::read_to_string(&path).unwrap().replace("get_composite_schedule=true", &format!("reserve_now=true\ncancel_reservation=true\nreserve_connector_zero_supported={zero}\nreservation16_file='{}'", fixture.root.join("reservations.json").display()));
    fs::write(path, config).unwrap();
    save(&fixture, &provisioning(false));
    fixture
}
pub fn command(id: &str, index: usize) -> Value {
    let native = native(index);
    let connector = native["connectorId"].as_u64().unwrap();
    let mut resource = json!({"bridge_id":"bridge-1","station_id":"station-a"});
    if connector > 0 {
        resource["resource"] = json!({"kind":"connector","connector_id":"one"});
        resource["native_protocol_reference"] =
            json!({"protocol":"ocpp16","connector_id":connector});
    }
    json!({"request_id":id,"resource":resource,"operation":{"kind":"ocpp","parameters":{"protocol":"ocpp16j","action":"ReserveNow","payload_schema":"urn:uob:ocpp16:ReserveNowReference:1","payload":{"connectorId":connector,"expiryDate":native["expiryDate"],"reservationId":native["reservationId"],"reservationReference":reference(index)}}},"expires_at":"2099-01-01T00:00:00Z"})
}
pub fn cancel(id: &str, reservation_id: &Value) -> Value {
    json!({"request_id":id,"resource":{"bridge_id":"bridge-1","station_id":"station-a"},"operation":{"kind":"ocpp","parameters":{"protocol":"ocpp16j","action":"CancelReservation","payload_schema":"urn:OCPP:1.6:2019:12:CancelReservationRequest","payload":{"reservationId":reservation_id}}},"expires_at":"2099-01-01T00:00:00Z"})
}
pub async fn create(
    client: &reqwest::Client,
    fixture: &Fixture,
    socket: &mut Socket,
    id: &str,
    index: usize,
    status: &str,
) -> Value {
    let submission = begin(client, fixture, command(id, index));
    let call = receive(socket).await;
    assert_eq!(call, json!([2, id, "ReserveNow", native(index)]));
    send(socket, json!([3,call[1],{"status":status}])).await;
    let value = completed(submission).await;
    assert_eq!(value["reservation_16"]["status"], status);
    assert_private(fixture, &value);
    value
}
pub fn assert_private(fixture: &Fixture, public: &Value) {
    for marker in [
        TOKEN,
        PARENT,
        SIBLING,
        UNGRANTED,
        "private-parent-113",
        "private-reserve-113",
        "reserve16:",
        "token_key",
        "group_key",
    ] {
        assert!(
            !public.to_string().contains(marker),
            "secret marker on public boundary"
        );
    }
    for file in ["charging.sqlite3", "charging.sqlite3-wal"] {
        if let Ok(bytes) = fs::read(fixture.root.join("state").join(file)) {
            for marker in [
                TOKEN,
                PARENT,
                SIBLING,
                UNGRANTED,
                "private-parent-113",
                "private-reserve-113",
            ] {
                assert!(
                    !bytes
                        .windows(marker.len())
                        .any(|part| part == marker.as_bytes()),
                    "raw identity persisted"
                );
            }
        }
    }
}
pub fn now() -> Value {
    json!(uob_contracts::UtcTimestamp::new(
        time::OffsetDateTime::now_utc()
    ))
}
