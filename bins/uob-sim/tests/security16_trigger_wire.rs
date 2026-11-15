//! `ExtendedTriggerMessage` over a real OCPP-J socket: the configured reply for every pinned
//! class, the triggered message that follows an accepted reply, and strict request validation.
mod security16_support;
#[allow(dead_code)]
#[path = "local_authorization16/socket.rs"]
mod socket;
use futures::StreamExt;
use security16_support::{Socket, boot_call, connect, private_directory};
use serde_json::{Value, json};
use socket::{accept, boot, receive, send};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};
use tokio::net::TcpListener;
use tokio::time::timeout;
use uob_sim::{ProtocolClient, SimulatorAction, SimulatorCall, SimulatorProtocolClient};

struct Station {
    directory: PathBuf,
    client: SimulatorProtocolClient,
    socket: std::sync::Mutex<Option<Socket>>,
}

impl Station {
    async fn start(security: &str, tables: &str) -> Self {
        let directory = private_directory("security-trigger");
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let tables = tables.replace("{dir}", &directory.display().to_string());
        let (client, socket) =
            connect(&listener, &directory, security, &tables, accept(&listener)).await;
        Self {
            directory,
            client,
            socket: std::sync::Mutex::new(Some(socket)),
        }
    }

    fn take(&self) -> Socket {
        self.socket.lock().unwrap().take().expect("accepted socket")
    }

    /// The peer answers the station's Boot on the accepted socket.
    async fn peer(&self) -> Socket {
        let mut socket = self.take();
        boot(&mut socket).await;
        socket
    }

    fn finish(self) {
        self.client.abort();
        std::fs::remove_dir_all(self.directory).unwrap();
    }
}

/// Sends one `ExtendedTriggerMessage` and returns the station's answer frame.
async fn trigger(socket: &mut Socket, requested: Value) -> Value {
    // The station replays its answer for a repeated message ID, so every request has its own.
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let id = format!("trigger-{}", NEXT.fetch_add(1, Ordering::Relaxed));
    send(socket, json!([2, id, "ExtendedTriggerMessage", requested])).await;
    let frame = receive(socket).await;
    assert_eq!(frame[1], id.as_str(), "{frame}");
    frame
}

async fn status(socket: &mut Socket, requested: Value) -> String {
    let frame = trigger(socket, requested).await;
    assert_eq!(frame[0], 3, "{frame}");
    frame[2]["status"].as_str().unwrap().to_owned()
}

/// Receive the triggered CALL, check its action and payload, and acknowledge it.
async fn expect_call(socket: &mut Socket, action: &str, payload: Value) -> Value {
    let frame = receive(socket).await;
    assert_eq!(
        (frame[0].as_u64(), frame[2].as_str()),
        (Some(2), Some(action)),
        "{frame}"
    );
    assert_eq!(frame[3], payload);
    send(socket, json!([3, frame[1], {}])).await;
    frame
}

async fn silent(socket: &mut Socket, period: Duration) {
    assert!(
        timeout(period, socket.next()).await.is_err(),
        "unexpected frame"
    );
}

fn requested(class: &str) -> Value {
    json!({"requestedMessage": class})
}

#[tokio::test]
async fn heartbeat_and_status_notification_follow_the_accepted_reply() {
    let station = Station::start("", "").await;
    let peer = async {
        let mut socket = station.peer().await;
        assert_eq!(
            status(&mut socket, requested("Heartbeat")).await,
            "Accepted"
        );
        expect_call(&mut socket, "Heartbeat", json!({})).await;
        let one = json!({"requestedMessage": "StatusNotification", "connectorId": 1});
        assert_eq!(status(&mut socket, one).await, "Accepted");
        let available = json!({"connectorId": 1, "status": "Available", "errorCode": "NoError"});
        expect_call(&mut socket, "StatusNotification", available.clone()).await;
        // Without a connector the station reports itself and every connector.
        assert_eq!(
            status(&mut socket, requested("StatusNotification")).await,
            "Accepted"
        );
        let main = json!({"connectorId": 0, "status": "Available", "errorCode": "NoError"});
        expect_call(&mut socket, "StatusNotification", main).await;
        expect_call(&mut socket, "StatusNotification", available).await;
        let unknown = json!({"requestedMessage": "StatusNotification", "connectorId": 9});
        assert_eq!(status(&mut socket, unknown).await, "Rejected");
        silent(&mut socket, Duration::from_millis(300)).await;
    };
    let ((), booted) = tokio::join!(peer, boot_call(&station.client));
    assert_eq!(booted["status"], "Accepted");
    assert_eq!(
        station.client.security16().unwrap().snapshot()["triggersReceived"],
        4
    );
    station.finish();
}

#[tokio::test]
async fn meter_values_report_the_latest_reading_with_trigger_context() {
    let station = Station::start("", "").await;
    let meter = json!({"requestedMessage": "MeterValues", "connectorId": 1});
    let peer = async {
        let mut socket = station.peer().await;
        // No reading has been taken yet, so there is nothing to report.
        assert_eq!(status(&mut socket, meter.clone()).await, "Rejected");
        silent(&mut socket, Duration::from_millis(300)).await;
        let reading = receive(&mut socket).await;
        assert_eq!(reading[2], "MeterValues");
        send(&mut socket, json!([3, reading[1], {}])).await;
        assert_eq!(status(&mut socket, meter.clone()).await, "Accepted");
        let frame = receive(&mut socket).await;
        assert_eq!(
            (frame[2].as_str(), &frame[3]["connectorId"]),
            (Some("MeterValues"), &json!(1))
        );
        let sample = &frame[3]["meterValue"][0]["sampledValue"][0];
        assert_eq!(
            (&sample["context"], &sample["value"]),
            (&json!("Trigger"), &json!("1250"))
        );
        send(&mut socket, json!([3, frame[1], {}])).await;
    };
    let reading = async {
        boot_call(&station.client).await;
        tokio::time::sleep(Duration::from_millis(500)).await;
        station
            .client
            .call(SimulatorCall {
                action: SimulatorAction::MeterValues,
                payload: json!({"connectorId":1,"meterValue":[{"timestamp":"2026-09-01T00:05:02Z",
                    "sampledValue":[{"value":"1250","context":"Sample.Periodic","unit":"Wh"}]}]}),
            })
            .await
            .unwrap();
    };
    tokio::join!(peer, reading);
    station.finish();
}

#[tokio::test]
async fn log_and_signed_firmware_status_are_idle_unless_an_upload_is_ongoing() {
    let tables = "[stations.diagnostics16]\nprivate_state_file='{dir}/diagnostics.json'\nlegacy=false\nsecurity_log=true\nupload_timeout_ms=60000\n";
    // Accepts connections but never answers, so the upload stays ongoing.
    let stalled = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let location = format!("http://{}/uploads/stalled/", stalled.local_addr().unwrap());
    let station = Station::start("", tables).await;
    let peer = async {
        let mut socket = station.peer().await;
        // N01.FR.12: no upload ongoing, so Idle and no requestId.
        assert_eq!(
            status(&mut socket, requested("LogStatusNotification")).await,
            "Accepted"
        );
        expect_call(
            &mut socket,
            "LogStatusNotification",
            json!({"status": "Idle"}),
        )
        .await;
        // L01.FR.28: without a signed update the answer is Idle, without requestId.
        assert_eq!(
            status(&mut socket, requested("FirmwareStatusNotification")).await,
            "Accepted"
        );
        expect_call(
            &mut socket,
            "SignedFirmwareStatusNotification",
            json!({"status": "Idle"}),
        )
        .await;
        let get_log =
            json!({"logType": "SecurityLog", "requestId": 5, "log": {"remoteLocation": location}});
        send(&mut socket, json!([2, "log", "GetLog", get_log])).await;
        assert_eq!(receive(&mut socket).await[2]["status"], "Accepted");
        expect_call(
            &mut socket,
            "LogStatusNotification",
            json!({"status": "Uploading", "requestId": 5}),
        )
        .await;
        assert_eq!(
            status(&mut socket, requested("LogStatusNotification")).await,
            "Accepted"
        );
        let current = json!({"status": "Uploading", "requestId": 5});
        expect_call(&mut socket, "LogStatusNotification", current).await;
    };
    let ((), booted) = tokio::join!(peer, boot_call(&station.client));
    assert_eq!(booted["status"], "Accepted");
    drop(stalled);
    station.finish();
}

#[tokio::test]
async fn configured_replies_decide_whether_anything_is_sent() {
    let security = "[stations.security16.extended_trigger]\nheartbeat='rejected'\nmeter_values='not_implemented'\nsign_charge_point_certificate='accepted'\n";
    let station = Station::start(security, "").await;
    let peer = async {
        let mut socket = station.peer().await;
        assert_eq!(
            status(&mut socket, requested("Heartbeat")).await,
            "Rejected"
        );
        assert_eq!(
            status(&mut socket, requested("MeterValues")).await,
            "NotImplemented"
        );
        // Certificate signing is not modelled: accepted when configured, but nothing follows.
        assert_eq!(
            status(&mut socket, requested("SignChargePointCertificate")).await,
            "Accepted"
        );
        silent(&mut socket, Duration::from_millis(500)).await;
    };
    let ((), booted) = tokio::join!(peer, boot_call(&station.client));
    assert_eq!(booted["status"], "Accepted");
    station.finish();
}

#[tokio::test]
async fn sign_charge_point_certificate_is_not_implemented_by_default() {
    let station = Station::start("", "").await;
    let peer = async {
        let mut socket = station.peer().await;
        let reply = status(&mut socket, requested("SignChargePointCertificate")).await;
        assert_eq!(reply, "NotImplemented");
        silent(&mut socket, Duration::from_millis(300)).await;
    };
    tokio::join!(peer, boot_call(&station.client));
    station.finish();
}

#[tokio::test]
async fn a_delayed_reply_is_written_late_and_the_message_still_follows_it() {
    let security = "[stations.security16.extended_trigger]\nreply_delay_ms=500\n";
    let station = Station::start(security, "").await;
    let peer = async {
        let mut socket = station.peer().await;
        let started = Instant::now();
        assert_eq!(
            status(&mut socket, requested("Heartbeat")).await,
            "Accepted"
        );
        assert!(
            started.elapsed() >= Duration::from_millis(450),
            "{:?}",
            started.elapsed()
        );
        expect_call(&mut socket, "Heartbeat", json!({})).await;
    };
    tokio::join!(peer, boot_call(&station.client));
    station.finish();
}

#[tokio::test]
async fn omit_accepts_the_request_but_never_sends_the_message() {
    let station = Station::start("[stations.security16.extended_trigger]\nomit=true\n", "").await;
    let peer = async {
        let mut socket = station.peer().await;
        assert_eq!(
            status(&mut socket, requested("Heartbeat")).await,
            "Accepted"
        );
        silent(&mut socket, Duration::from_millis(700)).await;
        // The station stays responsive.
        assert_eq!(
            status(&mut socket, requested("StatusNotification")).await,
            "Accepted"
        );
        silent(&mut socket, Duration::from_millis(300)).await;
    };
    tokio::join!(peer, boot_call(&station.client));
    station.finish();
}

#[tokio::test]
async fn invalid_requests_are_refused_without_sending_anything() {
    let station = Station::start("", "").await;
    let peer = async {
        let mut socket = station.peer().await;
        for (payload, code) in [
            (
                json!({"requestedMessage": "Heartbeat", "extra": 1}),
                "FormationViolation",
            ),
            (
                json!({"requestedMessage": "DiagnosticsStatusNotification"}),
                "FormationViolation",
            ),
            (json!({"connectorId": 1}), "FormationViolation"),
            (
                json!({"requestedMessage": "StatusNotification", "connectorId": "1"}),
                "FormationViolation",
            ),
            (
                json!({"requestedMessage": "StatusNotification", "connectorId": 0}),
                "PropertyConstraintViolation",
            ),
            (
                json!({"requestedMessage": "StatusNotification", "connectorId": -1}),
                "PropertyConstraintViolation",
            ),
        ] {
            let frame = trigger(&mut socket, payload.clone()).await;
            assert_eq!(
                (frame[0].as_u64(), frame[2].as_str()),
                (Some(4), Some(code)),
                "{payload}"
            );
        }
        silent(&mut socket, Duration::from_millis(300)).await;
    };
    tokio::join!(peer, boot_call(&station.client));
    station.finish();
}

#[tokio::test]
async fn a_boot_is_refused_once_accepted_and_sent_while_the_station_is_pending() {
    let station = Station::start("", "").await;
    let peer = async {
        let mut socket = station.take();
        let first = receive(&mut socket).await;
        let pending =
            json!({"status": "Pending", "currentTime": "2026-10-04T00:00:00Z", "interval": 60});
        send(&mut socket, json!([3, first[1], pending])).await;
        // Not accepted yet: the station reports its Boot again and the CSMS accepts it.
        assert_eq!(
            status(&mut socket, requested("BootNotification")).await,
            "Accepted"
        );
        let boot = receive(&mut socket).await;
        assert_eq!(
            (boot[2].as_str(), &boot[3]),
            (Some("BootNotification"), &first[3])
        );
        let accepted =
            json!({"status": "Accepted", "currentTime": "2026-10-04T00:00:00Z", "interval": 60});
        send(&mut socket, json!([3, boot[1], accepted])).await;
        timeout(Duration::from_secs(5), async {
            while station.client.accepted_registration() != Some(true) {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(
            status(&mut socket, requested("BootNotification")).await,
            "Rejected"
        );
    };
    let ((), booted) = tokio::join!(peer, boot_call(&station.client));
    assert_eq!(booted["status"], "Pending");
    station.finish();
}

#[tokio::test]
async fn a_station_without_a_security_model_keeps_the_library_reply() {
    let directory = private_directory("security-trigger-none");
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let source = format!(
        "schema_version=1\n[[stations]]\nid='alpha'\nendpoint='ws://{}/ocpp/alpha'\nocpp_version='1.6'\nrequest_timeout_ms=5000\n",
        listener.local_addr().unwrap()
    );
    let configuration = uob_sim::scenario::parse_configuration(&source).unwrap();
    let (client, mut socket) = tokio::join!(
        SimulatorProtocolClient::connect(configuration.stations[0].client_config()),
        accept(&listener)
    );
    let client = client.unwrap();
    let peer = async {
        boot(&mut socket).await;
        let frame = trigger(&mut socket, requested("Heartbeat")).await;
        assert_eq!(
            (frame[0].as_u64(), frame[2].as_str()),
            (Some(4), Some("NotImplemented"))
        );
    };
    tokio::join!(peer, boot_call(&client));
    client.abort();
    std::fs::remove_dir_all(directory).unwrap();
}
