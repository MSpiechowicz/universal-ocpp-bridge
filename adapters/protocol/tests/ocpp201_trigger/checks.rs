use super::*;

type NativeStatusCase = (
    &'static str,
    &'static str,
    Value,
    ResourceRef,
    TriggerTarget201,
    &'static str,
);

fn cases(snapshot: &StationSnapshot) -> [NativeStatusCase; 10] {
    [
        (
            "log",
            "LogStatusNotification",
            json!({"requestedMessage":"LogStatusNotification"}),
            snapshot.station.clone(),
            TriggerTarget201::Station,
            "NotImplemented",
        ),
        (
            "firmware",
            "FirmwareStatusNotification",
            json!({"requestedMessage":"FirmwareStatusNotification"}),
            snapshot.station.clone(),
            TriggerTarget201::Station,
            "Rejected",
        ),
        (
            "heartbeat",
            "Heartbeat",
            json!({"requestedMessage":"Heartbeat"}),
            snapshot.station.clone(),
            TriggerTarget201::Station,
            "Accepted",
        ),
        (
            "meter",
            "MeterValues",
            json!({"requestedMessage":"MeterValues","evse":{"id":1,"connectorId":1}}),
            snapshot.resources[0].resource.clone(),
            TriggerTarget201::Evse { id: 1 },
            "Rejected",
        ),
        (
            "charging-certificate",
            "SignChargingStationCertificate",
            json!({"requestedMessage":"SignChargingStationCertificate","evse":{"id":1}}),
            snapshot.station.clone(),
            TriggerTarget201::Station,
            "Accepted",
        ),
        (
            "v2g-certificate",
            "SignV2GCertificate",
            json!({"requestedMessage":"SignV2GCertificate","evse":{"id":1}}),
            snapshot.resources[0].resource.clone(),
            TriggerTarget201::Evse { id: 1 },
            "Accepted",
        ),
        (
            "status",
            "StatusNotification",
            json!({"requestedMessage":"StatusNotification","evse":{"id":1,"connectorId":1}}),
            snapshot.resources[1].resource.clone(),
            TriggerTarget201::Connector {
                id: 1,
                connector_id: 1,
            },
            "NotImplemented",
        ),
        (
            "transaction",
            "TransactionEvent",
            json!({"requestedMessage":"TransactionEvent","evse":{"id":1,"connectorId":1}}),
            snapshot.resources[1].resource.clone(),
            TriggerTarget201::Connector {
                id: 1,
                connector_id: 1,
            },
            "Accepted",
        ),
        (
            "combined-certificate",
            "SignCombinedCertificate",
            json!({"requestedMessage":"SignCombinedCertificate"}),
            snapshot.station.clone(),
            TriggerTarget201::Station,
            "Accepted",
        ),
        (
            "publish",
            "PublishFirmwareStatusNotification",
            json!({"requestedMessage":"PublishFirmwareStatusNotification"}),
            snapshot.station.clone(),
            TriggerTarget201::Station,
            "Accepted",
        ),
    ]
}

pub(super) async fn check_native_statuses(
    snapshot: &StationSnapshot,
    commands: &Arc<ScopedCommandAdmissionPort<Value>>,
    running: &mut RunningSession,
    store: &Store,
) {
    for case in cases(snapshot) {
        check_native_status(snapshot, commands, running, store, case).await;
    }
}

async fn check_native_status(
    snapshot: &StationSnapshot,
    commands: &Arc<ScopedCommandAdmissionPort<Value>>,
    running: &mut RunningSession,
    store: &Store,
    (id, class, payload, resource, target, status): NativeStatusCase,
) {
    let operation = request(snapshot, id, payload.clone(), resource);
    let sender = commands.clone();
    let submitted = tokio::spawn(async move { sender.submit(operation).await.unwrap() });
    let wire = receive_json(&mut running.peer).await;
    let expected = if id == "heartbeat" {
        let mut frame = fixture("trigger-heartbeat");
        frame[1] = json!(id);
        frame
    } else {
        json!([2, id, "TriggerMessage", payload])
    };
    assert_eq!(wire, expected);
    let dispatched = store
        .command_result_by_request_id(RequestId::new(id).unwrap())
        .await
        .unwrap()
        .unwrap();
    let observed = dispatched.trigger_observation_201.unwrap();
    assert_eq!(observed.expected_targets, vec![target]);
    assert!(observed.native_response.is_none());
    running.peer.send_text(json!([3,id,{"status":status,"statusInfo":{"reasonCode":"native","additionalInfo":"report pending"}}]).to_string()).await.unwrap();
    let result = submitted.await.unwrap();
    if status == "Accepted" {
        accepted(&result);
    } else {
        assert!(
            matches!(
                result.lifecycle,
                CommandLifecycle::ProtocolResponse {
                    accepted: false,
                    ..
                }
            ),
            "{class}"
        );
    }
    let observed = result.trigger_observation_201.unwrap();
    assert_eq!(
        observed
            .native_response
            .unwrap()
            .status_info
            .unwrap()
            .reason_code,
        "native"
    );
    assert!(
        observed.observed.is_empty(),
        "native response is not the resulting CALL"
    );
    assert_eq!(observed.expected_targets, vec![target]);
}

pub(super) async fn check_invalid_reply(
    snapshot: &StationSnapshot,
    commands: &Arc<ScopedCommandAdmissionPort<Value>>,
    running: &mut RunningSession,
) {
    let invalid = request(
        snapshot,
        "invalid-native-reply",
        json!({"requestedMessage":"Heartbeat"}),
        snapshot.station.clone(),
    );
    let sender = commands.clone();
    let submission = tokio::spawn(async move { sender.submit(invalid).await.unwrap() });
    assert_eq!(
        receive_json(&mut running.peer).await,
        json!([2,"invalid-native-reply","TriggerMessage",{"requestedMessage":"Heartbeat"}])
    );
    running.peer.send_text(json!([3,"invalid-native-reply",{"status":"Accepted","statusInfo":{"reasonCode":"x".repeat(21)}}]).to_string()).await.unwrap();
    let result = submission.await.unwrap();
    assert!(matches!(
        result.lifecycle,
        CommandLifecycle::TransmissionUncertain { .. }
    ));
    assert!(
        result
            .trigger_observation_201
            .unwrap()
            .native_response
            .is_none()
    );
}

pub(super) async fn check_denials(
    snapshot: &StationSnapshot,
    commands: &Arc<ScopedCommandAdmissionPort<Value>>,
) {
    for (id, payload, resource) in [
        (
            "boot-after-accepted",
            json!({"requestedMessage":"BootNotification"}),
            snapshot.station.clone(),
        ),
        (
            "connector-wide-meter",
            json!({"requestedMessage":"MeterValues","evse":{"id":1,"connectorId":1}}),
            snapshot.resources[1].resource.clone(),
        ),
        (
            "mismatched-connector",
            json!({"requestedMessage":"StatusNotification","evse":{"id":1,"connectorId":2}}),
            snapshot.resources[1].resource.clone(),
        ),
        (
            "unknown-enum",
            json!({"requestedMessage":"DiagnosticsStatusNotification"}),
            snapshot.station.clone(),
        ),
    ] {
        let result = commands
            .submit(request(snapshot, id, payload, resource))
            .await
            .unwrap();
        assert!(
            matches!(result.lifecycle, CommandLifecycle::Rejected { .. }),
            "{id}"
        );
        assert!(result.trigger_observation_201.is_none());
    }
}
