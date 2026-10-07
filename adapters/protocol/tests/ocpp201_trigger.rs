#[path = "ocpp201_trigger/checks.rs"]
mod checks;
mod endpoint_support;
#[path = "ocpp201_remote_control/support.rs"]
mod support;

use serde_json::{Value, json};
use std::{sync::Arc, time::Duration};
use support::*;
use uob_application::*;
use uob_contracts::*;
use uob_protocol_adapter::{DecodeErrorKind, v201};

fn enable(snapshot: &mut StationSnapshot, port: &Arc<v201::remote_control::RemoteControlSession>) {
    for capabilities in std::iter::once(&mut snapshot.capabilities).chain(
        snapshot
            .resources
            .iter_mut()
            .map(|entry| &mut entry.capabilities),
    ) {
        capabilities.operations.push(SupportedOperation {
            operation: Operation::ProtocolAction {
                protocol: ProtocolEdition::Ocpp201,
                action: "TriggerMessage".to_owned(),
            },
            parameters: vec![],
        });
    }
    port.update_committed(snapshot.clone()).unwrap();
}

fn request(
    snapshot: &StationSnapshot,
    id: &str,
    payload: Value,
    resource: ResourceRef,
) -> ExternalCommand<Value> {
    let mut command = command(snapshot, id, protocol("TriggerMessage", payload));
    command.request.resource = resource;
    command
}

#[tokio::test]
async fn trigger_sends_exact_scope_and_preserves_response_separate_from_later_report() {
    let mut running = session("ocpp2.0.1", Duration::from_secs(2)).await;
    let database = Database::new();
    let store = database.open();
    let (mut snapshot, _, port, coordinator) =
        Box::pin(setup(&store, running.handle.clone())).await;
    enable(&mut snapshot, &port);
    let commands = Arc::new(scoped(
        coordinator,
        &snapshot,
        vec![
            AccessPermission::Control,
            AccessPermission::PrivilegedControl,
        ],
    ));
    checks::check_native_statuses(&snapshot, &commands, &mut running, &store).await;
    assert!(persisted(&store).await.transactions.is_empty());
    let heartbeat = fixture("trigger-result-heartbeat");
    running.peer.send_text(heartbeat.to_string()).await.unwrap();
    let incoming = tokio::time::timeout(
        endpoint_support::TEST_BOUND,
        running.outputs.incoming.receive(),
    )
    .await
    .expect("triggered report delivery bound")
    .expect("triggered report");
    assert!(matches!(
        incoming.call.observation,
        ChargerObservation::Heartbeat {
            protocol: ProtocolEdition::Ocpp201
        }
    ));
    incoming
        .responder
        .respond(&json!({"currentTime":"2026-09-01T00:00:00Z"}))
        .unwrap();
    assert_eq!(
        receive_json(&mut running.peer).await,
        json!([3, heartbeat[1], {"currentTime":"2026-09-01T00:00:00Z"}])
    );
    checks::check_invalid_reply(&snapshot, &commands, &mut running).await;
    running.task.shutdown(Duration::from_secs(1)).await.unwrap();
    running.server.abort();
    store.shutdown(Duration::from_secs(1)).await.unwrap();
}

#[tokio::test]
async fn trigger_denials_and_pending_registration_never_transmit() {
    let mut running = session("ocpp2.0.1", Duration::from_secs(1)).await;
    let database = Database::new();
    let store = database.open();
    let (mut snapshot, _, port, coordinator) =
        Box::pin(setup(&store, running.handle.clone())).await;
    let commands = Arc::new(scoped(
        coordinator.clone(),
        &snapshot,
        vec![
            AccessPermission::Control,
            AccessPermission::PrivilegedControl,
        ],
    ));
    let no_capability = commands
        .submit(request(
            &snapshot,
            "no-capability",
            json!({"requestedMessage":"Heartbeat"}),
            snapshot.station.clone(),
        ))
        .await
        .unwrap();
    assert!(
        matches!(no_capability.lifecycle, CommandLifecycle::Rejected { error } if error.code == CommandErrorCode::UnsupportedOperation)
    );
    enable(&mut snapshot, &port);
    let commands = Arc::new(scoped(
        coordinator.clone(),
        &snapshot,
        vec![
            AccessPermission::Control,
            AccessPermission::PrivilegedControl,
        ],
    ));
    let unauthorized = scoped(coordinator, &snapshot, vec![AccessPermission::Control]);
    assert!(
        unauthorized
            .submit(request(
                &snapshot,
                "no-permission",
                json!({"requestedMessage":"Heartbeat"}),
                snapshot.station.clone()
            ))
            .await
            .is_err()
    );
    checks::check_denials(&snapshot, &commands).await;
    let mut pending = snapshot.clone();
    for value in &mut pending.current_values {
        if value.point_id.as_str() == "ocpp201/registration/status" {
            value.value = Some(TypedValue::Text("Pending".to_owned()));
        }
    }
    port.update_committed(pending.clone()).unwrap();
    let denied = commands
        .submit(request(
            &pending,
            "preaccepted-heartbeat",
            json!({"requestedMessage":"Heartbeat"}),
            pending.station.clone(),
        ))
        .await
        .unwrap();
    assert!(matches!(
        denied.lifecycle,
        CommandLifecycle::Rejected { .. }
    ));
    assert!(
        tokio::time::timeout(Duration::from_millis(40), running.peer.receive())
            .await
            .is_err()
    );
    running.task.shutdown(Duration::from_secs(1)).await.unwrap();
    running.server.abort();
    store.shutdown(Duration::from_secs(1)).await.unwrap();
}

#[tokio::test]
async fn preaccepted_boot_and_delayed_native_reply_remain_distinct() {
    let mut running = session("ocpp2.0.1", Duration::from_millis(150)).await;
    let database = Database::new();
    let store = database.open();
    let (mut snapshot, _, port, coordinator) =
        Box::pin(setup(&store, running.handle.clone())).await;
    enable(&mut snapshot, &port);
    let commands = Arc::new(scoped(
        coordinator,
        &snapshot,
        vec![
            AccessPermission::Control,
            AccessPermission::PrivilegedControl,
        ],
    ));
    for value in &mut snapshot.current_values {
        if value.point_id.as_str() == "ocpp201/registration/status" {
            value.value = Some(TypedValue::Text("Pending".to_owned()));
        }
    }
    port.update_committed(snapshot.clone()).unwrap();
    let boot = request(
        &snapshot,
        "preaccepted-boot",
        json!({"requestedMessage":"BootNotification"}),
        snapshot.station.clone(),
    );
    let sender = commands.clone();
    let submission = tokio::spawn(async move { sender.submit(boot).await.unwrap() });
    assert_eq!(
        receive_json(&mut running.peer).await,
        json!([2,"preaccepted-boot","TriggerMessage",{"requestedMessage":"BootNotification"}])
    );
    let result = submission.await.unwrap();
    assert!(matches!(
        result.lifecycle,
        CommandLifecycle::TransmissionUncertain { .. }
    ));
    assert_eq!(
        result.trigger_observation_201.unwrap().native_response,
        None
    );
    running
        .peer
        .send_text(json!([3,"preaccepted-boot",{"status":"Accepted"}]).to_string())
        .await
        .unwrap();
    let nonboot = commands
        .submit(request(
            &snapshot,
            "preaccepted-log",
            json!({"requestedMessage":"LogStatusNotification"}),
            snapshot.station.clone(),
        ))
        .await
        .unwrap();
    assert!(matches!(
        nonboot.lifecycle,
        CommandLifecycle::Rejected { .. }
    ));
    running.task.shutdown(Duration::from_secs(1)).await.unwrap();
    running.server.abort();
    store.shutdown(Duration::from_secs(1)).await.unwrap();
}

#[test]
fn incoming_trigger_reports_validate_native_identity_and_do_not_sign_certificates() {
    let reports = [
        (
            "LogStatusNotification",
            json!({"status":"Idle"}),
            TriggerMessageClass201::LogStatusNotification,
        ),
        (
            "FirmwareStatusNotification",
            json!({"status":"Installed","requestId":1}),
            TriggerMessageClass201::FirmwareStatusNotification,
        ),
        (
            "PublishFirmwareStatusNotification",
            json!({"status":"Published","requestId":1,"location":["https://example.test/firmware"]}),
            TriggerMessageClass201::PublishFirmwareStatusNotification,
        ),
    ];
    for (action, payload, class) in reports {
        let call = v201::decode_call(json!([2, "report", action, payload]).to_string().as_bytes())
            .unwrap();
        // Firmware and log reports carry their own typed observation; trigger receipt maps it
        // back.
        assert!(
            matches!(call.observation, ChargerObservation::TriggerStatus201 { class: received, .. } if received == class)
                || (class == TriggerMessageClass201::FirmwareStatusNotification
                    && matches!(
                        call.observation,
                        ChargerObservation::FirmwareStatus201 {
                            request_id: Some(1),
                            ..
                        }
                    ))
                || (class == TriggerMessageClass201::LogStatusNotification
                    && matches!(
                        call.observation,
                        ChargerObservation::LogStatus201 {
                            request_id: None,
                            ..
                        }
                    ))
        );
    }
    let certificate = v201::decode_call(json!([2,"cert","SignCertificate",{"csr":"-----BEGIN CERTIFICATE REQUEST-----","certificateType":"V2GCertificate"}]).to_string().as_bytes()).unwrap();
    assert!(matches!(
        certificate.observation,
        ChargerObservation::TriggerCertificate201 {
            class: TriggerMessageClass201::SignV2GCertificate
        }
    ));
    for (action, payload) in [
        ("SignCertificate", json!({"csr":""})),
        (
            "SignCertificate",
            json!({"csr":"x","certificateType":"wrong"}),
        ),
        ("SignCertificate", json!({"csr":"x","unknown":false})),
        ("LogStatusNotification", json!({"status":"fabricated"})),
        // N01.FR.13: only Idle may omit the upload identity.
        ("LogStatusNotification", json!({"status":"Uploaded"})),
        (
            "FirmwareStatusNotification",
            json!({"status":"Idle","requestId":2_147_483_648_u64}),
        ),
        // L01.FR.20: only Idle may omit the update identity.
        ("FirmwareStatusNotification", json!({"status":"Installed"})),
        (
            "PublishFirmwareStatusNotification",
            json!({"status":"Published","location":[null]}),
        ),
    ] {
        let error = v201::decode_call(
            json!([2, "invalid", action, payload])
                .to_string()
                .as_bytes(),
        )
        .unwrap_err();
        assert_eq!(
            error.kind(),
            DecodeErrorKind::InvalidPayload,
            "{action}: {payload}"
        );
    }
}
