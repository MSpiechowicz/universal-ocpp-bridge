use serde_json::{Value, json};
use std::{fs, time::Duration};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};
use tokio::net::TcpListener;
#[allow(dead_code)] // Shared helpers also serve the local-authorization process targets.
#[path = "local_authorization201/process.rs"]
mod common;
use common::{
    PrivateDirectory, Process, accept, boot, closed, native_boot, prefix, receive, send, step,
};

type Socket = tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>;
const MARKERS: [&str; 3] = [
    "private-owner-114",
    "private-member-114",
    "private-group-114",
];

fn assert_private(output: &std::process::Output) {
    for bytes in [&output.stdout, &output.stderr] {
        let public = String::from_utf8_lossy(bytes).to_lowercase();
        for marker in MARKERS {
            assert!(
                !public.contains(marker),
                "private reservation marker leaked"
            );
        }
    }
}
/// Answer one station CALL of the expected action with `reply`.
async fn answer(socket: &mut Socket, action: &str, reply: Value) -> Value {
    let frame = receive(socket).await;
    assert_eq!(
        (frame[0].clone(), frame[2].clone()),
        (json!(2), json!(action))
    );
    send(socket, json!([3, frame[1], reply])).await;
    frame[3].clone()
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // One process narrative; splitting would hide state carried across runs.
async fn killed_binary_recovers_reports_offline_expiry_and_group_start_ends_reservation() {
    let directory = PrivateDirectory::new();
    let state = directory.0.join("reservations.json");
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let config = directory.write(
        "config.toml",
        &format!(
            "schema_version=1\n[[stations]]\nid='alpha'\nendpoint='ws://{}'\nocpp_version='2.0.1'\nrequest_timeout_ms=5000\nreconnect=false\n[[stations.evses]]\nid=1\nconnectors=[1]\n[[stations.evses]]\nid=2\nconnectors=[1]\n[stations.reservation201]\nprivate_state_file='{}'\nenabled=true\n",
            listener.local_addr().unwrap(),
            state.display()
        ),
    );
    let first = directory.write(
        "first.toml",
        &(prefix()
            + &step("connect", "connect", "")
            + &native_boot("boot", "Accepted")
            + &step("accepted", "await_reservation", "expect_response={activeReservations=2}")
            + "\n[[steps]]\nid='park'\nstation='alpha'\naction='wait'\nduration_ms=60000\ntimeout_ms=65000\n"),
    );
    let mut first = Process::start(&config, &first);
    let mut socket = accept(&listener).await;
    boot(&mut socket).await;
    let short = (OffsetDateTime::now_utc().replace_nanosecond(0).unwrap()
        + time::Duration::seconds(3))
    .format(&Rfc3339)
    .unwrap();
    for (id, evse, expiry) in [(-114, 1, "2099-01-01T00:00:00Z"), (115, 2, short.as_str())] {
        send(
            &mut socket,
            json!([2, format!("reserve-{id}"), "ReserveNow", {
            "id":id,"expiryDateTime":expiry,"evseId":evse,
            "idToken":{"idToken":"private-owner-114","type":"ISO14443"},
            "groupIdToken":{"idToken":"private-group-114","type":"Central"}}]),
        )
        .await;
        assert_eq!(receive(&mut socket).await[2]["status"], "Accepted");
        let reserved = answer(&mut socket, "StatusNotification", json!({})).await;
        assert_eq!(
            (
                reserved["evseId"].clone(),
                reserved["connectorStatus"].clone()
            ),
            (json!(evse), json!("Reserved"))
        );
    }
    assert_private(&first.kill());
    drop(socket);
    let before: Value = serde_json::from_slice(&fs::read(&state).unwrap()).unwrap();
    assert_eq!(before["reservations"]["-114"]["evseId"], 1);
    assert_eq!(before["reservations"]["115"]["evseId"], 2);
    // The short reservation expires while no station process runs at all.
    tokio::time::sleep(Duration::from_secs(4)).await;
    let second = directory.write(
        "second.toml",
        &(prefix()
            + &step("connect", "connect", "")
            + &native_boot("boot", "Accepted")
            + &step("reported", "await_reservation", "expect_response={activeReservations=1,pendingUpdates=0}")
            + &step("member", "authorize", "fixture_id='sim.ocpp201.authorize.accepted'\npayload={idToken={idToken='private-member-114',type='ISO14443'}}")
            + &step("start", "start_transaction", "fixture_id='sim.ocpp201.transaction.started'\nuse_current_timestamp=true\npayload={eventType='Started',triggerReason='Authorized',seqNo=0,transactionInfo={transactionId='native-reserved-114',chargingState='EVConnected'},evse={id=1,connectorId=1},idToken={idToken='private-member-114',type='ISO14443'}}")
            + &step("ended", "assert_reservation", "expect_response={activeReservations=0}")
            + &step("disconnect", "disconnect", "")),
    );
    let mut second = Process::start(&config, &second);
    let mut socket = accept(&listener).await;
    boot(&mut socket).await;
    // H04.FR.01–03 applied at restart: Available, then Expired, only once registered.
    let available = answer(&mut socket, "StatusNotification", json!({})).await;
    assert_eq!(
        (
            available["evseId"].clone(),
            available["connectorStatus"].clone()
        ),
        (json!(2), json!("Available"))
    );
    assert_eq!(
        answer(&mut socket, "ReservationStatusUpdate", json!({})).await,
        json!({"reservationId":115,"reservationUpdateStatus":"Expired"})
    );
    let member = json!({"idTokenInfo":{"status":"Accepted","groupIdToken":{"idToken":"PRIVATE-GROUP-114","type":"Central"}}});
    answer(&mut socket, "Authorize", member.clone()).await;
    // Without a local list or cache, H03.FR.08 resolves the group through the CSMS again.
    let lookup = answer(&mut socket, "Authorize", member).await;
    assert_eq!(lookup["idToken"]["idToken"], "private-member-114");
    let started = answer(
        &mut socket,
        "TransactionEvent",
        json!({"idTokenInfo":{"status":"Accepted"}}),
    )
    .await;
    assert_eq!(started["eventType"], "Started");
    assert_eq!(
        started["reservationId"], -114,
        "H01.FR.15 the ending start carries reservationId"
    );
    closed(&mut socket).await;
    let output = second.finish().await;
    assert!(
        output.status.success(),
        "native reservation process scenario failed"
    );
    assert_private(&output);
    let after: Value = serde_json::from_slice(&fs::read(&state).unwrap()).unwrap();
    assert_eq!(after["reservations"], json!({}));
    assert_eq!(after["updates"], json!([]));
    assert_eq!(after["evses"]["1"]["1"], "Occupied");
}
