pub use crate::host::{
    CONTROL, Fixture, PRIVILEGED, READ, Socket, begin, client, completed, no_call, receive, result,
    send, station_call,
};
use serde_json::{Value, json};
use std::{fs, os::unix::fs::PermissionsExt, time::Duration};
pub const TOKEN: &str = "PRIVATE-RESERVE-114";
pub const GROUP: &str = "PRIVATE-GROUP-114";
pub const SIBLING: &str = "PRIVATE-SIBLING-114";
pub const STRANGER: &str = "PRIVATE-STRANGER-114";
pub fn reference(index: usize) -> String {
    format!("reserve201:{index:064x}")
}
pub fn reservation_id(index: usize) -> i32 {
    match index {
        1 => i32::MIN,
        2 => 0,
        3 => i32::MAX,
        _ => -i32::try_from(index).unwrap(),
    }
}
/// Index 6 is the unspecified-EVSE (station-scope) reservation.
pub fn native(index: usize) -> Value {
    let mut value = json!({"id":reservation_id(index),"expiryDateTime":"2099-01-01T00:00:00Z","idToken":{"idToken":TOKEN,"type":"ISO14443"},"groupIdToken":{"idToken":GROUP,"type":"Central"}});
    if index != 6 {
        value["evseId"] = json!(1);
    }
    value
}
pub fn provisioning() -> Value {
    let reservations: Vec<_> = (1..=9).map(|index| json!({"reference":reference(index),"request":native(index),"expires_at":"2099-01-01T00:00:00Z","revoked":false})).collect();
    json!({"reservations":reservations,"identities":[
        {"idToken":{"idToken":TOKEN,"type":"ISO14443"},"groupIdToken":{"idToken":GROUP,"type":"Central"},"authorize":true},
        {"idToken":{"idToken":SIBLING,"type":"Local"},"groupIdToken":{"idToken":"private-group-114","type":"Central"},"authorize":true}
    ]})
}
pub fn save(fixture: &Fixture, value: &Value) {
    let temporary = fixture.root.join("reservations-next.json");
    fs::write(&temporary, value.to_string()).unwrap();
    fs::set_permissions(&temporary, fs::Permissions::from_mode(0o600)).unwrap();
    fs::rename(temporary, fixture.root.join("reservations.json")).unwrap();
}
pub fn fixture(non_evse_specific: bool) -> Fixture {
    let fixture = Fixture::new();
    let path = fixture.root.join("bridge.toml");
    let config = fs::read_to_string(&path)
        .unwrap()
        .replace(
            "get_composite_schedule=true",
            &format!(
                "reserve_now=true\ncancel_reservation=true\nreserve_non_evse_specific_supported={non_evse_specific}\nreservation201_file='{}'",
                fixture.root.join("reservations.json").display()
            ),
        )
        .replacen("protocol='ocpp16j'", "protocol='ocpp201'", 1)
        .replacen(
            "connector_id='one'\nnative_connector_id=1",
            "evse_id='one'\nnative_evse_id=1\n[[charging.stations.resources]]\nevse_id='one'\nnative_evse_id=1\nconnector_id='one'\nnative_connector_id=1",
            1,
        )
        .replacen(
            "connector_id='two'\nnative_connector_id=2",
            "evse_id='two'\nnative_evse_id=2",
            1,
        );
    fs::write(path, config).unwrap();
    save(&fixture, &provisioning());
    fixture
}
pub fn command(id: &str, index: usize) -> Value {
    let native = native(index);
    let mut resource = json!({"bridge_id":"bridge-1","station_id":"station-a"});
    let mut payload = json!({"id":native["id"],"expiryDateTime":native["expiryDateTime"],"reservationReference":reference(index)});
    if let Some(evse) = native.get("evseId") {
        resource["resource"] = json!({"kind":"evse","evse_id":"one"});
        resource["native_protocol_reference"] = json!({"protocol":"ocpp201","evse_id":evse});
        payload["evseId"] = evse.clone();
    }
    json!({"request_id":id,"resource":resource,"operation":{"kind":"ocpp","parameters":{"protocol":"ocpp201","action":"ReserveNow","payload_schema":"urn:uob:ocpp201:ReserveNowReference:1","payload":payload}},"expires_at":"2099-01-01T00:00:00Z"})
}
pub fn cancel(id: &str, reservation_id: &Value) -> Value {
    json!({"request_id":id,"resource":{"bridge_id":"bridge-1","station_id":"station-a"},"operation":{"kind":"ocpp","parameters":{"protocol":"ocpp201","action":"CancelReservation","payload_schema":"urn:OCPP:Cp:2:2020:3:CancelReservationRequest","payload":{"reservationId":reservation_id}}},"expires_at":"2099-01-01T00:00:00Z"})
}
pub async fn boot(socket: &mut Socket) {
    send(socket, json!([2,"boot","BootNotification",{"chargingStation":{"model":"model","vendorName":"vendor"},"reason":"PowerUp"}])).await;
    assert_eq!(receive(socket).await[2]["status"], "Accepted");
}
pub async fn connected(fixture: &Fixture, client: &reqwest::Client) -> Socket {
    let mut socket = fixture.station_protocol("station-a", "ocpp2.0.1").await;
    boot(&mut socket).await;
    fixture.connected(client, "station-a").await;
    socket
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
    assert_eq!(value["reservation_201"]["status"], status);
    assert_private(fixture, &value);
    value
}
pub async fn rejected(client: &reqwest::Client, fixture: &Fixture, credential: &str, body: &Value) {
    let response = client
        .post(fixture.url("/api/v1/commands"))
        .bearer_auth(credential)
        .json(body)
        .send()
        .await
        .unwrap();
    let status = response.status();
    let value: Value = response.json().await.unwrap();
    assert!(
        !status.is_success() || value["result"]["lifecycle"]["stage"] == "rejected",
        "invalid reservation admitted: {value}"
    );
    assert_private(fixture, &value);
}
pub fn transaction(id: &str, seq: u32, token: Option<(&str, &str)>, reservation: i32) -> Value {
    let mut payload = json!({"eventType":"Started","timestamp":now(),"triggerReason":"Authorized","seqNo":seq,"transactionInfo":{"transactionId":id},"evse":{"id":1,"connectorId":1},"reservationId":reservation});
    if let Some((token, kind)) = token {
        payload["idToken"] = json!({"idToken":token,"type":kind});
    }
    payload
}
pub async fn state(client: &reqwest::Client, fixture: &Fixture, id: &str, expected: &str) -> Value {
    tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            let value = result(client, fixture, id).await;
            if value["reservation_201"]["reconciliation"]["state"] == expected {
                break value;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("{id} never reached {expected}"))
}
pub fn assert_private(fixture: &Fixture, public: &Value) {
    let markers = [
        TOKEN,
        GROUP,
        SIBLING,
        STRANGER,
        "private-group-114",
        "private-reserve-114",
    ];
    for marker in markers
        .iter()
        .copied()
        .chain(["reserve201:", "token_key", "group_key"])
    {
        assert!(
            !public.to_string().contains(marker),
            "secret marker on public boundary"
        );
    }
    for file in ["charging.sqlite3", "charging.sqlite3-wal"] {
        if let Ok(bytes) = fs::read(fixture.root.join("state").join(file)) {
            for marker in markers {
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
