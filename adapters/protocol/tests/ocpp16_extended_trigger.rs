//! Corpus-driven OCPP 1.6 `ExtendedTriggerMessage` against a raw station peer.
mod endpoint_support;
#[path = "ocpp16_extended_trigger/recovery.rs"]
mod recovery;
#[path = "ocpp16_extended_trigger/refusals.rs"]
mod refusals;
#[allow(dead_code)] // Independently compiled roots share this opt-in socket harness.
#[path = "ocpp16_remote_control/support.rs"]
mod support;

use serde_json::{Value, json};
use std::{sync::Arc, time::Duration};
use support::*;
use uob_application::*;
use uob_contracts::*;
use uob_protocol_adapter::v16::remote_control::RemoteControlSession;

const SCHEMA: &str = "urn:OCPP:Cp:1.6:2020:3:ExtendedTriggerMessage.req";

/// Turns the station option on for the station and every connector, plus the named station
/// operations that gate the Whitepaper classes.
fn enable(
    snapshot: &mut StationSnapshot,
    port: &Arc<RemoteControlSession>,
    station_extra: &[&str],
) {
    let operation = |action: &str| SupportedOperation {
        operation: Operation::ProtocolAction {
            protocol: ProtocolEdition::Ocpp16j,
            action: action.to_owned(),
        },
        parameters: vec![],
    };
    for action in station_extra
        .iter()
        .copied()
        .chain(["ExtendedTriggerMessage"])
    {
        snapshot.capabilities.operations.push(operation(action));
    }
    for entry in &mut snapshot.resources {
        entry
            .capabilities
            .operations
            .push(operation("ExtendedTriggerMessage"));
    }
    port.update_committed(snapshot.clone()).unwrap();
}

fn extended(
    snapshot: &StationSnapshot,
    id: &str,
    payload: Value,
    station: bool,
) -> ExternalCommand<Value> {
    let mut external = command(
        snapshot,
        id,
        CommandOperation::Ocpp(PrivilegedOcppOperation {
            protocol: ProtocolEdition::Ocpp16j,
            action: ProtocolActionName::new("ExtendedTriggerMessage").unwrap(),
            payload_schema: PayloadSchemaId::new(SCHEMA).unwrap(),
            payload,
        }),
    );
    if station {
        external.request.resource = snapshot.station.clone();
    }
    external
}

fn controls(
    coordinator: Arc<Coordinator>,
    snapshot: &StationSnapshot,
) -> Arc<ScopedCommandAdmissionPort<Value>> {
    Arc::new(scoped(
        coordinator,
        snapshot,
        vec![
            AccessPermission::Control,
            AccessPermission::PrivilegedControl,
        ],
    ))
}

fn with_id(name: &str, id: &str) -> Value {
    let mut frame = fixture(name);
    frame[1] = json!(id);
    frame
}

async fn quiet(running: &mut RunningSession) -> bool {
    tokio::time::timeout(Duration::from_millis(60), running.peer.receive())
        .await
        .is_err()
}

/// One accepted request fixture: the exact wire CALL, then the frozen observation.
struct Case {
    fixture: &'static str,
    class: TriggerMessageClass,
    scope: Option<u32>,
    station: bool,
    targets: Vec<u32>,
}

fn cases() -> Vec<Case> {
    let case = |fixture, class, scope, station, targets: &[u32]| Case {
        fixture,
        class,
        scope,
        station,
        targets: targets.to_vec(),
    };
    vec![
        case(
            "extended-trigger-firmware-status",
            TriggerMessageClass::SignedFirmwareStatusNotification,
            None,
            true,
            &[0],
        ),
        case(
            "extended-trigger-heartbeat",
            TriggerMessageClass::Heartbeat,
            None,
            true,
            &[0],
        ),
        case(
            "extended-trigger-log-status",
            TriggerMessageClass::LogStatusNotification,
            None,
            true,
            &[0],
        ),
        case(
            "extended-trigger-meter-all",
            TriggerMessageClass::MeterValues,
            None,
            true,
            &[1, 2],
        ),
        case(
            "extended-trigger-meter-connector-1",
            TriggerMessageClass::MeterValues,
            Some(1),
            false,
            &[1],
        ),
        case(
            "extended-trigger-status-all",
            TriggerMessageClass::StatusNotification,
            None,
            true,
            &[0, 1, 2],
        ),
        case(
            "extended-trigger-status-connector-1",
            TriggerMessageClass::StatusNotification,
            Some(1),
            false,
            &[1],
        ),
    ]
}

#[tokio::test]
async fn every_request_fixture_is_sent_as_that_exact_native_call_with_frozen_targets() {
    let mut running = session("ocpp1.6", Duration::from_secs(2)).await;
    let database = Database::new();
    let store = database.open();
    let (mut snapshot, _, port, coordinator) = setup(&store, running.handle.clone()).await;
    enable(&mut snapshot, &port, &["GetLog", "SignedUpdateFirmware"]);
    let commands = controls(coordinator, &snapshot);
    for case in cases() {
        let wire = fixture(case.fixture);
        let id = wire[1].as_str().unwrap().to_owned();
        let external = extended(&snapshot, &id, wire[3].clone(), case.station);
        let sender = commands.clone();
        let submission = tokio::spawn(async move { sender.submit(external).await.unwrap() });
        assert_eq!(
            receive_json(&mut running.peer).await,
            wire,
            "{}",
            case.fixture
        );

        let dispatched = store
            .command_result_by_request_id(RequestId::new(&id).unwrap())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(dispatched.lifecycle, CommandLifecycle::Dispatched);
        let observation = dispatched.trigger_observation.as_ref().unwrap();
        assert_eq!(observation.requested_class, case.class, "{}", case.fixture);
        assert_eq!(observation.native_scope, case.scope, "{}", case.fixture);
        assert_eq!(
            observation.expected_targets, case.targets,
            "{}",
            case.fixture
        );
        assert_eq!(observation.native_response, None);
        let version = if matches!(
            case.class,
            TriggerMessageClass::LogStatusNotification
                | TriggerMessageClass::SignedFirmwareStatusNotification
        ) {
            ContractVersion::V1_EXTENDED_TRIGGER_16
        } else {
            ContractVersion::V1_TRIGGER
        };
        assert_eq!(dispatched.schema_version, version, "{}", case.fixture);

        running
            .peer
            .send_text(with_id("extended-trigger-accepted", &id).to_string())
            .await
            .unwrap();
        let result = submission.await.unwrap();
        assert_eq!(result.schema_version, version, "{}", case.fixture);
        let observation = result.trigger_observation.as_ref().unwrap();
        assert_eq!(
            observation.native_response,
            Some(TriggerNativeResponse::Accepted)
        );
        assert_eq!(observation.status, TriggerObservationStatus::Pending);
        accepted(&result);
    }
    running.task.shutdown(Duration::from_secs(1)).await.unwrap();
    running.server.abort();
    store.shutdown(Duration::from_secs(1)).await.unwrap();
}

#[tokio::test]
async fn each_reply_fixture_maps_to_its_native_response_and_observation() {
    let mut running = session("ocpp1.6", Duration::from_secs(2)).await;
    let database = Database::new();
    let store = database.open();
    let (mut snapshot, _, port, coordinator) = setup(&store, running.handle.clone()).await;
    enable(&mut snapshot, &port, &[]);
    let commands = controls(coordinator, &snapshot);
    for (reply, native, status) in [
        (
            "extended-trigger-accepted",
            TriggerNativeResponse::Accepted,
            TriggerObservationStatus::Pending,
        ),
        (
            "extended-trigger-rejected",
            TriggerNativeResponse::Rejected,
            TriggerObservationStatus::Unsupported,
        ),
        (
            "extended-trigger-not-implemented",
            TriggerNativeResponse::NotImplemented,
            TriggerObservationStatus::Unsupported,
        ),
    ] {
        let id = format!("reply-{reply}");
        let external = extended(
            &snapshot,
            &id,
            json!({"requestedMessage":"Heartbeat"}),
            true,
        );
        let sender = commands.clone();
        let submission = tokio::spawn(async move { sender.submit(external).await.unwrap() });
        assert_eq!(
            receive_json(&mut running.peer).await,
            with_id("extended-trigger-heartbeat", &id)
        );
        running
            .peer
            .send_text(with_id(reply, &id).to_string())
            .await
            .unwrap();
        let result = submission.await.unwrap();
        let observation = result.trigger_observation.as_ref().unwrap();
        assert_eq!(observation.native_response, Some(native), "{reply}");
        assert_eq!(observation.status, status, "{reply}");
        assert!(observation.observed.is_empty());
        assert!(
            matches!(&result.lifecycle, CommandLifecycle::ProtocolResponse { accepted, .. }
                if *accepted == (native == TriggerNativeResponse::Accepted)),
            "{reply}"
        );
        assert_eq!(
            store
                .command_result_by_request_id(RequestId::new(&id).unwrap())
                .await
                .unwrap(),
            Some(result)
        );
    }
    running.task.shutdown(Duration::from_secs(1)).await.unwrap();
    running.server.abort();
    store.shutdown(Duration::from_secs(1)).await.unwrap();
}
