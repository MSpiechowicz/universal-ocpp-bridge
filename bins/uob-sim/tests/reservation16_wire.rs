use futures::StreamExt;
use serde_json::{Value, json};
use std::{fs, os::unix::fs::PermissionsExt, time::Duration};
use tokio::{net::TcpListener, sync::oneshot, time::timeout};
use uob_sim::local_authorization::transport::NativeReplyFault;
use uob_sim::scenario::parse_configuration;
use uob_sim::{ProtocolClient, SimulatorAction, SimulatorCall, SimulatorProtocolClient};
#[allow(dead_code)]
#[path = "local_authorization16/socket.rs"]
mod socket;
use socket::{accept, boot, receive, send};

async fn reply(
    socket: &mut tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>,
    id: &str,
) -> Value {
    loop {
        let frame = receive(socket).await;
        if frame[0] == 2 && frame[2] == "StatusNotification" {
            send(socket, json!([3, frame[1], {}])).await;
        } else {
            assert_eq!(frame[1], id);
            return frame;
        }
    }
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // One live socket narrative; splitting would hide state carried across steps.
async fn real_socket_signed_replace_cancel_invalid_and_post_commit_drop_survive_reopen() {
    let directory = std::env::temp_dir().join(format!("uob-reserve-wire-{}", uuid::Uuid::new_v4()));
    fs::create_dir(&directory).unwrap();
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).unwrap();
    let directory = fs::canonicalize(directory).unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let source = format!(
        "schema_version=1\n[[stations]]\nid='alpha'\nendpoint='ws://{}'\nocpp_version='1.6'\nconnectors=[1,2]\n[stations.reservation16]\nprivate_state_file='{}'\nenabled=true\nreserve_connector_zero_supported=true\n",
        listener.local_addr().unwrap(),
        directory.join("reservation.json").display()
    );
    let (armed, ready) = oneshot::channel();
    let (finished, observed) = oneshot::channel();
    let peer = tokio::spawn(async move {
        let mut socket = accept(&listener).await;
        boot(&mut socket).await;
        let request = json!({"connectorId":1,"expiryDate":"2099-01-01T00:00:00Z","idTag":"private-token-113","parentIdTag":"private-parent-113","reservationId":-113});
        send(&mut socket, json!([2, "reserve", "ReserveNow", request])).await;
        assert_eq!(
            reply(&mut socket, "reserve").await,
            json!([3,"reserve",{"status":"Accepted"}])
        );
        send(&mut socket, json!([2, "reserve", "ReserveNow", request])).await;
        assert_eq!(reply(&mut socket, "reserve").await[2]["status"], "Accepted");
        for (index, fields) in [
            json!({"connectorId":-1}),
            json!({"connectorId":4_294_967_296_u64}),
            json!({"reservationId":2_147_483_648_i64}),
            json!({"idTag":"X".repeat(21)}),
            json!({"parentIdTag":null}),
            json!({"expiryDate":"not-a-date"}),
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
            assert_eq!(reply(&mut socket, &id).await[0], 4);
        }
        send(
            &mut socket,
            json!([2,"invalid","CancelReservation",{"reservationId":-113,"connectorId":1}]),
        )
        .await;
        assert_eq!(reply(&mut socket, "invalid").await[0], 4);
        send(
            &mut socket,
            json!([2,"cancel","CancelReservation",{"reservationId":-113}]),
        )
        .await;
        assert_eq!(reply(&mut socket, "cancel").await[2]["status"], "Accepted");
        send(
            &mut socket,
            json!([2,"absent","CancelReservation",{"reservationId":-113}]),
        )
        .await;
        assert_eq!(reply(&mut socket, "absent").await[2]["status"], "Rejected");
        finished.send(()).unwrap();
        ready.await.unwrap();
        send(&mut socket, json!([2,"lost","ReserveNow",{"connectorId":0,"expiryDate":"2099-01-01T00:00:00Z","idTag":"private-token-113","reservationId":0}])).await;
        timeout(Duration::from_secs(5), async {
            while let Some(message) = socket.next().await {
                match message.unwrap() {
                    tokio_tungstenite::tungstenite::Message::Close(_) => return,
                    tokio_tungstenite::tungstenite::Message::Text(text) => {
                        let frame: Value = serde_json::from_str(&text).unwrap();
                        assert_eq!(frame[0], 2, "post-mutation dropped ACK was fabricated");
                        assert_eq!(frame[2], "StatusNotification");
                        send(&mut socket, json!([3, frame[1], {}])).await;
                    }
                    _ => {}
                }
            }
        })
        .await
        .unwrap();
    });
    let client = SimulatorProtocolClient::connect(
        parse_configuration(&source).unwrap().stations[0].client_config(),
    )
    .await
    .unwrap();
    client
        .call(SimulatorCall {
            action: SimulatorAction::BootNotification,
            payload: json!({"chargePointVendor":"UOB","chargePointModel":"Independent"}),
        })
        .await
        .unwrap();
    timeout(Duration::from_secs(5), observed)
        .await
        .unwrap()
        .unwrap();
    client
        .arm_local_reply_fault(NativeReplyFault::DropConnection)
        .unwrap();
    armed.send(()).unwrap();
    timeout(Duration::from_secs(5), peer)
        .await
        .unwrap()
        .unwrap();
    let snapshot = client.reservation16().unwrap().snapshot();
    assert_eq!(snapshot["activeReservations"], 1);
    assert_eq!(snapshot["reservations"][0]["reservationId"], 0);
    let public = format!("{:?}", client.traces());
    assert!(!public.contains("private-token-113"));
    assert!(!public.contains("private-parent-113"));
    client.abort();
    drop(client);
    let config = uob_sim::reservation16::ReservationConfig {
        private_state_file: directory
            .join("reservation.json")
            .to_str()
            .unwrap()
            .to_owned(),
        enabled: true,
        reserve_connector_zero_supported: true,
    };
    let recovered =
        uob_sim::reservation16::ReservationHandle::open("alpha", &[1, 2], &config).unwrap();
    assert_eq!(recovered.snapshot()["reservations"][0]["reservationId"], 0);
    drop(recovered);
    fs::remove_dir_all(directory).unwrap();
}
