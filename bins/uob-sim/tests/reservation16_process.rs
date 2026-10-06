use serde_json::{Value, json};
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::Path,
    process::{Child, Command, Stdio},
    time::Duration,
};
use tokio::{net::TcpListener, time::timeout};
#[path = "local_authorization16/socket.rs"]
mod socket;
use socket::{accept, boot, closed, receive, send};

struct Process(Option<Child>);

impl Drop for Process {
    fn drop(&mut self) {
        if let Some(mut child) = self.0.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}
fn private(path: &Path, content: &str) {
    fs::write(path, content).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
}

fn launch(directory: &Path, scenario: &str) -> Process {
    Process(Some(
        Command::new(env!("CARGO_BIN_EXE_uob-sim"))
            .args(["run", "--config"])
            .arg(directory.join("config.toml"))
            .arg("--scenario")
            .arg(directory.join(scenario))
            .args(["--format", "jsonl"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    ))
}

const CONNECT_BOOT: &str = "schema_version=1\nseed=113\n\n[[steps]]\nid='connect'\nstation='alpha'\naction='connect'\ntimeout_ms=5000\n\n[[steps]]\nid='boot'\nstation='alpha'\naction='boot'\ntimeout_ms=5000\nfixture_id='wire.ocpp16.boot.valid'\npayload={chargePointVendor='UOB',chargePointModel='Reservations 16'}\n";

#[tokio::test]
#[allow(clippy::too_many_lines)] // One live socket narrative; splitting would hide state carried across steps.
async fn killed_binary_recovers_real_reservation_and_actual_group_start_terminates_it() {
    let directory =
        std::env::temp_dir().join(format!("uob-reservation-process-{}", uuid::Uuid::new_v4()));
    fs::create_dir(&directory).unwrap();
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).unwrap();
    let directory = fs::canonicalize(directory).unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    private(
        &directory.join("config.toml"),
        &format!(
            "schema_version=1\n[[stations]]\nid='alpha'\nendpoint='ws://{}'\nocpp_version='1.6'\nconnectors=[1,2]\nrequest_timeout_ms=5000\n[stations.local_authorization]\nprivate_state_file='{}'\n[stations.reservation16]\nprivate_state_file='{}'\nenabled=true\nreserve_connector_zero_supported=true\n",
            listener.local_addr().unwrap(),
            directory.join("identity.json").display(),
            directory.join("reservations.json").display()
        ),
    );
    private(
        &directory.join("first.toml"),
        &(CONNECT_BOOT.to_owned()
            + "\n[[steps]]\nid='accepted-native'\nstation='alpha'\naction='await_reservation'\ntimeout_ms=5000\nexpect_response={activeReservations=1}\n\n[[steps]]\nid='park'\nstation='alpha'\naction='wait'\nduration_ms=60000\ntimeout_ms=65000\n"),
    );
    let mut first = launch(&directory, "first.toml");
    let mut first_socket = accept(&listener).await;
    boot(&mut first_socket).await;
    send(
        &mut first_socket,
        json!([2,"private-reserve","ReserveNow",{
            "connectorId":0,"expiryDate":"2099-01-01T00:00:00Z","idTag":"private-owner-113",
            "parentIdTag":"private-group-113","reservationId":-113
        }]),
    )
    .await;
    assert_eq!(
        receive(&mut first_socket).await,
        json!([3,"private-reserve",{"status":"Accepted"}])
    );
    first.0.as_mut().unwrap().kill().unwrap();
    let first_output = first.0.take().unwrap().wait_with_output().unwrap();
    drop(first_socket);
    let before: Value =
        serde_json::from_slice(&fs::read(directory.join("reservations.json")).unwrap()).unwrap();
    assert_eq!(before["reservations"]["-113"]["connectorId"], 0);
    assert_eq!(before["revision"], 1);
    private(
        &directory.join("second.toml"),
        &format!(
            "{CONNECT_BOOT}\n[[steps]]\nid='recovered-native'\nstation='alpha'\naction='assert_reservation'\ntimeout_ms=5000\nexpect_response={{activeReservations=1,revision=1}}\n\n[[steps]]\nid='actual-member-authorize'\nstation='alpha'\naction='authorize'\ntimeout_ms=5000\nfixture_id='wire.ocpp16.authorization.valid'\npayload={{idTag='private-member-113'}}\n\n[[steps]]\nid='actual-matching-group-start'\nstation='alpha'\naction='start_transaction'\ntimeout_ms=5000\nfixture_id='wire.ocpp16.transaction-start.valid'\nuse_current_timestamp=true\npayload={{connectorId=2,idTag='private-member-113',meterStart=113}}\n\n[[steps]]\nid='terminated-native'\nstation='alpha'\naction='assert_reservation'\ntimeout_ms=5000\nexpect_response={{activeReservations=0}}\n\n[[steps]]\nid='disconnect'\nstation='alpha'\naction='disconnect'\ntimeout_ms=5000\n"
        ),
    );
    let mut second = launch(&directory, "second.toml");
    let mut second_socket = accept(&listener).await;
    boot(&mut second_socket).await;
    let authorize = receive(&mut second_socket).await;
    assert_eq!(authorize[2], "Authorize");
    assert_eq!(authorize[3]["idTag"], "private-member-113");
    let earliest_start = time::OffsetDateTime::now_utc();
    send(&mut second_socket, json!([3,authorize[1],{"idTagInfo":{"status":"Accepted","parentIdTag":"PRIVATE-GROUP-113"}}])).await;
    let started = receive(&mut second_socket).await;
    assert_eq!(started[2], "StartTransaction");
    assert_eq!(started[3]["connectorId"], 2);
    assert_eq!(started[3]["reservationId"], -113);
    let source_time = time::OffsetDateTime::parse(
        started[3]["timestamp"].as_str().unwrap(),
        &time::format_description::well_known::Rfc3339,
    )
    .unwrap();
    assert!(source_time >= earliest_start);
    assert!(source_time <= time::OffsetDateTime::now_utc());
    send(
        &mut second_socket,
        json!([3,started[1],{"transactionId":113,"idTagInfo":{"status":"Accepted"}}]),
    )
    .await;
    closed(&mut second_socket).await;
    timeout(Duration::from_secs(5), async {
        while second.0.as_mut().unwrap().try_wait().unwrap().is_none() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let output = second.0.take().unwrap().wait_with_output().unwrap();
    assert!(output.status.success(), "native process scenario failed");
    for bytes in [
        &first_output.stdout,
        &first_output.stderr,
        &output.stdout,
        &output.stderr,
    ] {
        let public = String::from_utf8_lossy(bytes).to_lowercase();
        for marker in [
            "private-owner-113",
            "private-member-113",
            "private-group-113",
        ] {
            assert!(!public.contains(marker));
        }
    }
    let after: Value =
        serde_json::from_slice(&fs::read(directory.join("reservations.json")).unwrap()).unwrap();
    assert_eq!(after["reservations"], json!({}));
    assert_eq!(after["connectors"]["2"], "Occupied");
    fs::remove_dir_all(directory).unwrap();
}
