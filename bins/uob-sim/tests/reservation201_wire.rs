use futures::StreamExt;
use serde_json::{Value, json};
use std::{fs, os::unix::fs::PermissionsExt, path::PathBuf, time::Duration};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};
use tokio::{net::TcpListener, sync::oneshot, time::timeout};
use uob_sim::local_authorization::transport::NativeReplyFault;
use uob_sim::reservation201::{Reservation201Config, Reservation201Handle};
use uob_sim::scenario::parse_configuration;
use uob_sim::{ProtocolClient, SimulatorAction, SimulatorCall, SimulatorProtocolClient};
#[allow(dead_code)]
#[path = "local_authorization201/socket.rs"]
mod socket;
use socket::{accept, boot, receive, send};

type Socket = tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>;

fn private_directory() -> PathBuf {
    let directory = std::env::temp_dir().join(format!("uob-reserve201-{}", uuid::Uuid::new_v4()));
    fs::create_dir(&directory).unwrap();
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).unwrap();
    fs::canonicalize(directory).unwrap()
}
fn source(address: std::net::SocketAddr, state: &std::path::Path) -> String {
    format!(
        "schema_version=1\n[[stations]]\nid='alpha'\nendpoint='ws://{address}'\nocpp_version='2.0.1'\n[[stations.evses]]\nid=1\nconnectors=[1]\n[[stations.evses]]\nid=2\nconnectors=[1]\n[stations.reservation201]\nprivate_state_file='{}'\nenabled=true\nnon_evse_specific=true\n",
        state.display()
    )
}
fn reserve(id: i64, evse: u16, expiry: &str) -> Value {
    json!({"id":id,"expiryDateTime":expiry,"evseId":evse,
        "idToken":{"idToken":"private-owner-114","type":"ISO14443"},
        "groupIdToken":{"idToken":"private-group-114","type":"Central"}})
}
fn soon(seconds: i64) -> String {
    (OffsetDateTime::now_utc().replace_nanosecond(0).unwrap() + time::Duration::seconds(seconds))
        .format(&Rfc3339)
        .unwrap()
}
/// Next station CALL, answered with an empty native confirmation.
async fn station_call(socket: &mut Socket, action: &str) -> Value {
    let frame = receive(socket).await;
    assert_eq!(frame[0], 2, "station sent an unexpected frame");
    assert_eq!(frame[2], action);
    send(socket, json!([3, frame[1], {}])).await;
    frame[3].clone()
}
async fn connected(source: &str) -> SimulatorProtocolClient {
    let client = SimulatorProtocolClient::connect(
        parse_configuration(source).unwrap().stations[0].client_config(),
    )
    .await
    .unwrap();
    client
        .call(SimulatorCall {
            action: SimulatorAction::BootNotification,
            payload: json!({"reason":"PowerUp","chargingStation":{"model":"Independent","vendorName":"UOB"}}),
        })
        .await
        .unwrap();
    client
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // One live socket narrative; splitting would hide state carried across steps.
async fn real_socket_reply_view_cancel_invalid_and_expiry_report_follow_native_order() {
    let directory = private_directory();
    let state = directory.join("reservation.json");
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let source = source(listener.local_addr().unwrap(), &state);
    let peer = tokio::spawn(async move {
        let mut socket = accept(&listener).await;
        boot(&mut socket).await;
        let request = reserve(-114, 1, "2099-01-01T00:00:00Z");
        send(&mut socket, json!([2, "reserve", "ReserveNow", request])).await;
        assert_eq!(
            receive(&mut socket).await,
            json!([3,"reserve",{"status":"Accepted"}])
        );
        let reserved = station_call(&mut socket, "StatusNotification").await;
        assert_eq!(
            (
                reserved["evseId"].clone(),
                reserved["connectorStatus"].clone()
            ),
            (json!(1), json!("Reserved"))
        );
        for (index, fields) in [
            json!({"id":2_147_483_648_i64}),
            json!({"evseId":"1"}),
            json!({"connectorType":"Type2"}),
            json!({"expiryDateTime":"not-a-date"}),
            json!({"expiryDateTime":"2099-01-01T00:00:00.1234Z"}),
            json!({"idToken":{"idToken":"X".repeat(37),"type":"ISO14443"}}),
            json!({"idToken":{"idToken":"token","type":"Unknown"}}),
            json!({"groupIdToken":null}),
            json!({"unexpected":true}),
        ]
        .into_iter()
        .enumerate()
        {
            let mut invalid = request.clone();
            invalid
                .as_object_mut()
                .unwrap()
                .extend(fields.as_object().unwrap().clone());
            let id = format!("invalid-reserve-{index}");
            send(&mut socket, json!([2, id, "ReserveNow", invalid])).await;
            let reply = receive(&mut socket).await;
            assert_eq!(
                (reply[0].clone(), reply[2].clone()),
                (json!(4), json!("FormationViolation"))
            );
        }
        send(
            &mut socket,
            json!([2,"invalid-cancel","CancelReservation",{"reservationId":-114,"evseId":1}]),
        )
        .await;
        assert_eq!(receive(&mut socket).await[0], 4);
        send(
            &mut socket,
            json!([2,"cancel","CancelReservation",{"reservationId":-114}]),
        )
        .await;
        assert_eq!(
            receive(&mut socket).await,
            json!([3,"cancel",{"status":"Accepted"}])
        );
        let available = station_call(&mut socket, "StatusNotification").await;
        assert_eq!(available["connectorStatus"], "Available");
        send(
            &mut socket,
            json!([2,"absent","CancelReservation",{"reservationId":-114}]),
        )
        .await;
        assert_eq!(
            receive(&mut socket).await,
            json!([3,"absent",{"status":"Rejected"}])
        );
        send(
            &mut socket,
            json!([2, "short", "ReserveNow", reserve(115, 2, &soon(2))]),
        )
        .await;
        assert_eq!(receive(&mut socket).await[2]["status"], "Accepted");
        assert_eq!(
            station_call(&mut socket, "StatusNotification").await["connectorStatus"],
            "Reserved"
        );
        // H04: the EVSE reports Available first, then the reservation's Expired update.
        let available = station_call(&mut socket, "StatusNotification").await;
        assert_eq!(
            (
                available["evseId"].clone(),
                available["connectorStatus"].clone()
            ),
            (json!(2), json!("Available"))
        );
        assert_eq!(
            station_call(&mut socket, "ReservationStatusUpdate").await,
            json!({"reservationId":115,"reservationUpdateStatus":"Expired"})
        );
        socket
    });
    let client = connected(&source).await;
    let mut socket = timeout(Duration::from_secs(15), peer)
        .await
        .unwrap()
        .unwrap();
    let handle = client.reservation201().unwrap();
    timeout(Duration::from_secs(5), async {
        while !handle.updates().is_empty() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(handle.snapshot()["activeReservations"], 0);
    let public = format!("{:?} {}", client.traces(), handle.snapshot());
    for marker in ["private-owner-114", "private-group-114"] {
        assert!(!public.contains(marker));
    }
    client.abort();
    drop(client);
    drop(handle);
    while let Ok(Some(_)) = timeout(Duration::from_millis(200), socket.next()).await {}
    fs::remove_dir_all(directory).unwrap();
}

#[tokio::test]
async fn post_commit_dropped_ack_and_faulted_target_survive_reopen_with_removed_report() {
    let directory = private_directory();
    let state = directory.join("reservation.json");
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let source = source(listener.local_addr().unwrap(), &state);
    let (armed, ready) = oneshot::channel();
    let (faulted, fault_seen) = oneshot::channel();
    let peer = tokio::spawn(async move {
        let mut socket = accept(&listener).await;
        boot(&mut socket).await;
        send(
            &mut socket,
            json!([
                2,
                "first",
                "ReserveNow",
                reserve(7, 1, "2099-01-01T00:00:00Z")
            ]),
        )
        .await;
        assert_eq!(receive(&mut socket).await[2]["status"], "Accepted");
        station_call(&mut socket, "StatusNotification").await;
        faulted.send(()).unwrap();
        let fault = station_call(&mut socket, "StatusNotification").await;
        assert_eq!(fault["connectorStatus"], "Faulted");
        // H01.FR.16: the station cancels and reports Removed.
        assert_eq!(
            station_call(&mut socket, "ReservationStatusUpdate").await,
            json!({"reservationId":7,"reservationUpdateStatus":"Removed"})
        );
        ready.await.unwrap();
        send(
            &mut socket,
            json!([
                2,
                "lost",
                "ReserveNow",
                reserve(0, 2, "2099-01-01T00:00:00Z")
            ]),
        )
        .await;
        timeout(Duration::from_secs(5), async {
            while let Some(message) = socket.next().await {
                if let tokio_tungstenite::tungstenite::Message::Close(_) = message.unwrap() {
                    return;
                }
            }
        })
        .await
        .unwrap();
    });
    let client = connected(&source).await;
    timeout(Duration::from_secs(5), fault_seen)
        .await
        .unwrap()
        .unwrap();
    client
        .call(SimulatorCall {
            action: SimulatorAction::StatusNotification,
            payload: json!({"timestamp":"2026-10-06T00:00:00Z","connectorStatus":"Faulted","evseId":1,"connectorId":1}),
        })
        .await
        .unwrap();
    let handle = client.reservation201().unwrap();
    timeout(Duration::from_secs(5), async {
        while !handle.updates().is_empty() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    client
        .arm_local_reply_fault(NativeReplyFault::DropConnection)
        .unwrap();
    armed.send(()).unwrap();
    timeout(Duration::from_secs(5), peer)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(handle.snapshot()["reservations"][0]["id"], 0);
    client.abort();
    drop(client);
    drop(handle);
    let config = Reservation201Config {
        private_state_file: state.to_str().unwrap().to_owned(),
        enabled: true,
        non_evse_specific: true,
        connector_types: vec![],
    };
    let recovered = Reservation201Handle::open("alpha", &[(1, 1), (2, 1)], &config).unwrap();
    assert_eq!(recovered.snapshot()["activeReservations"], 1);
    assert_eq!(recovered.snapshot()["reservations"][0]["evseId"], 2);
    drop(recovered);
    fs::remove_dir_all(directory).unwrap();
}
