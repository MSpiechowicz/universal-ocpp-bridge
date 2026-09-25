#![allow(dead_code)] // The shared remote-control fixture support exercises a wider command surface.
#[path = "ocpp16_configuration/concurrent.rs"]
mod concurrent;
#[path = "ocpp16_configuration/denials.rs"]
mod denials;
mod endpoint_support;
#[path = "ocpp16_configuration/queue.rs"]
mod queue;
#[path = "ocpp16_configuration/readonly.rs"]
mod readonly;
#[path = "ocpp16_remote_control/support.rs"]
mod support;

use serde_json::{Value, json};
use std::{sync::Arc, time::Duration};
use support::*;
use tokio::time::timeout;
use uob_application::capture::*;
use uob_application::*;
use uob_contracts::*;
use uob_protocol_adapter::v16::remote_control::{
    LocalConfigurationValues, LocalRemoteStartIdentity, ProtectedConfigurationText,
    ProtectedConfigurationValue, RemoteControlSession,
};

const REFERENCE: &str = "cfg:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

fn configured(
    store: &Store,
    running: &RunningSession,
    mut snapshot: StationSnapshot,
    authorization: Arc<Auth>,
    values: Vec<ProtectedConfigurationValue>,
    diagnostics: FlowDiagnostics,
) -> (
    StationSnapshot,
    Arc<LocalConfigurationValues>,
    Arc<Coordinator>,
) {
    for action in ["GetConfiguration", "ChangeConfiguration"] {
        snapshot.capabilities.operations.push(SupportedOperation {
            operation: Operation::ProtocolAction {
                protocol: ProtocolEdition::Ocpp16j,
                action: action.to_owned(),
            },
            parameters: vec![],
        });
    }
    let identity = Arc::new(
        LocalRemoteStartIdentity::new(
            vec![SensitiveAuthorizationToken::new("INDEPENDENT-001").unwrap()],
            authorization,
        )
        .unwrap(),
    );
    let provider = Arc::new(LocalConfigurationValues::new(values).unwrap());
    let port = Arc::new(
        RemoteControlSession::new(
            running.handle.clone(),
            snapshot.clone(),
            identity,
            Arc::new(Clock),
            Arc::new(store.clone()),
        )
        .unwrap()
        .with_configuration_values(provider.clone()),
    );
    let coordinator = Arc::new(
        Coordinator::new(Arc::new(store.clone()), port, Arc::new(Clock))
            .with_diagnostics(diagnostics),
    );
    (snapshot, provider, coordinator)
}

fn write(
    snapshot: &StationSnapshot,
    id: &str,
    key: &str,
    reference: &str,
) -> ExternalCommand<Value> {
    let mut command = command(
        snapshot,
        id,
        protocol(
            "ChangeConfiguration",
            json!({"key":key,"valueReference":reference}),
        ),
    );
    command.request.resource = snapshot.station.clone();
    if let CommandOperation::Ocpp(operation) = &mut command.request.operation {
        operation.payload_schema =
            PayloadSchemaId::new("urn:uob:ocpp16:ChangeConfigurationReference:1").unwrap();
    }
    command
}
fn read(snapshot: &StationSnapshot, id: &str, payload: Value) -> ExternalCommand<Value> {
    let mut command = command(snapshot, id, protocol("GetConfiguration", payload));
    command.request.resource = snapshot.station.clone();
    command
}
fn protected(snapshot: &StationSnapshot, key: &str, value: &str) -> ProtectedConfigurationValue {
    ProtectedConfigurationValue {
        resource: snapshot.station.clone(),
        key: key.to_owned(),
        reference: REFERENCE.to_owned(),
        value: ProtectedConfigurationText::new(value.to_owned()).unwrap(),
        expires_at: time("2026-09-02T00:00:00Z"),
    }
}
async fn exchange(
    peer: &mut uob_hostile_websocket_peer::Peer,
    commands: &Arc<ScopedCommandAdmissionPort<Value>>,
    command: ExternalCommand<Value>,
    expected_payload: Value,
    response_payload: Value,
) -> CommandResult {
    let id = command.request.request_id.as_str().to_owned();
    let action = match &command.request.operation {
        CommandOperation::Ocpp(operation) => operation.action.as_str().to_owned(),
        _ => unreachable!(),
    };
    let commands = commands.clone();
    let submit = tokio::spawn(async move { commands.submit(command).await.unwrap() });
    let frame = receive_json(peer).await;
    assert_eq!(frame, json!([2, id, action, expected_payload]));
    peer.send_text(json!([3, id, response_payload]).to_string())
        .await
        .unwrap();
    submit.await.unwrap()
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // Keep the ordered wire and durable observations in one scenario.
async fn read_partial_unknown_absent_readonly_redaction_and_later_explicit_observation_survive_restart()
 {
    let mut running = session("ocpp1.6", Duration::from_secs(2)).await;
    let database = Database::new();
    let store = database.open();
    let (snapshot, authorization, _, _) = setup(&store, running.handle.clone()).await;
    let app = endpoint_support::application(Environment::Demo, None);
    let capture = CaptureManager::new(true);
    let grant = CaptureGrant::new(
        app.identity().bridge_id.clone(),
        vec![CapturePermission::Capture],
        None,
        None,
    )
    .unwrap();
    capture
        .start(
            &grant,
            CaptureFilter {
                bridge: app.identity().bridge_id.clone(),
                station: None,
                target: None,
            },
            CaptureLevel::Metadata,
            None,
        )
        .unwrap();
    let (diagnostics, diagnostics_rx) = FlowDiagnostics::channel(
        app.runtime_identity().process_instance_id.clone(),
        app.identity().bridge_id.clone(),
        capture,
        Arc::new(Clock),
        128,
    )
    .unwrap();
    let readonly_reference = format!("cfg:{}", "2".repeat(64));
    let mut readonly_value = protected(&snapshot, "NumberOfConnectors", "3");
    readonly_value.reference = readonly_reference.clone();
    let (snapshot, provider, coordinator) = configured(
        &store,
        &running,
        snapshot.clone(),
        authorization,
        vec![
            protected(&snapshot, "VendorPassword", "private-secret"),
            readonly_value,
        ],
        diagnostics,
    );
    let commands = Arc::new(scoped(
        coordinator.clone(),
        &snapshot,
        vec![AccessPermission::PrivilegedControl],
    ));
    let written = exchange(
        &mut running.peer,
        &commands,
        write(&snapshot, "change-private", "VendorPassword", REFERENCE),
        json!({"key":"VendorPassword","value":"private-secret"}),
        json!({"status":"RebootRequired"}),
    )
    .await;
    assert!(matches!(
        written.configuration,
        Some(ConfigurationResult::Write {
            status: ConfigurationWriteStatus::RebootRequired,
            ..
        })
    ));
    assert!(written.configuration_observations.is_empty());
    let selected = fixture("get-configuration-selected")[3].clone();
    let partial = fixture("get-configuration-partial")[2].clone();
    let read_result = exchange(
        &mut running.peer,
        &commands,
        read(&snapshot, "get-configuration-selected", selected.clone()),
        selected,
        partial,
    )
    .await;
    let Some(ConfigurationResult::Read {
        keys: Some(keys),
        unknown_keys: Some(unknown_keys),
        ..
    }) = read_result.configuration
    else {
        panic!("read evidence")
    };
    assert_eq!(unknown_keys, vec!["UnknownParameter"]);
    assert_eq!(keys[0].value.as_deref(), Some("60"));
    assert_eq!(keys[1].value.as_deref(), Some("2"));
    assert!(keys[1].readonly);
    assert!(!keys[2].redacted && keys[2].value.is_none());
    let readonly_denied = commands
        .submit(write(
            &snapshot,
            "readonly-denied",
            "NumberOfConnectors",
            &readonly_reference,
        ))
        .await
        .unwrap();
    assert!(matches!(
        readonly_denied.lifecycle,
        CommandLifecycle::Rejected {
            error: CommandError {
                code: CommandErrorCode::PolicyRejected,
                ..
            }
        }
    ));
    let selected = json!({"key":["VendorPassword"]});
    let reply = json!({"configurationKey":[{"key":"VendorPassword","readonly":false,"value":"private-secret"}]});
    let redacted = exchange(
        &mut running.peer,
        &commands,
        read(&snapshot, "read-private", selected.clone()),
        selected,
        reply,
    )
    .await;
    let Some(ConfigurationResult::Read {
        keys: Some(keys),
        unknown_keys: None,
        ..
    }) = redacted.configuration
    else {
        panic!("private read evidence")
    };
    assert!(keys[0].redacted && keys[0].value.is_none());
    let safe_named_key = json!({"key":["HeartbeatInterval"]});
    let unsafe_value = exchange(&mut running.peer, &commands,
        read(&snapshot, "unsafe-known-value", safe_named_key.clone()), safe_named_key,
        json!({"configurationKey":[{"key":"HeartbeatInterval","readonly":false,"value":"private-secret"}]})).await;
    assert!(
        matches!(unsafe_value.configuration, Some(ConfigurationResult::Read { keys: Some(keys), .. }) if keys[0].redacted && keys[0].value.is_none())
    );
    for (id, payload, response, expected_keys, expected_unknown) in [
        (
            "read-all",
            fixture("get-configuration-all")[3].clone(),
            json!({}),
            None,
            None,
        ),
        (
            "get-configuration-empty-keys",
            fixture("get-configuration-empty-keys")[3].clone(),
            fixture("get-configuration-empty-response")[2].clone(),
            Some(0),
            Some(0),
        ),
        (
            "unknown-only",
            json!({"key":["UnknownParameter"]}),
            json!({"unknownKey":["UnknownParameter"]}),
            None,
            Some(1),
        ),
        (
            "known-only-empty-unknown",
            json!({}),
            json!({"configurationKey":[]}),
            Some(0),
            None,
        ),
    ] {
        let result = exchange(
            &mut running.peer,
            &commands,
            read(&snapshot, id, payload.clone()),
            payload.clone(),
            response,
        )
        .await;
        let Some(ConfigurationResult::Read {
            requested_keys,
            keys,
            unknown_keys,
        }) = result.configuration.as_ref()
        else {
            panic!("read evidence")
        };
        assert_eq!(
            requested_keys.as_ref().map(Vec::len),
            payload
                .get("key")
                .map(|keys| keys.as_array().unwrap().len())
        );
        assert_eq!(
            keys.as_ref().map(Vec::len),
            expected_keys,
            "{id}: known keys"
        );
        assert_eq!(
            unknown_keys.as_ref().map(Vec::len),
            expected_unknown,
            "{id}: unknown keys"
        );
        if id == "unknown-only" {
            assert_eq!(unknown_keys.as_ref().unwrap(), &["UnknownParameter"]);
        }
        let stored = store
            .command_result_by_request_id(RequestId::new(id).unwrap())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            stored.configuration, result.configuration,
            "{id}: persisted evidence"
        );
    }
    let limit_request = json!({"key":["GetConfigurationMaxKeys"]});
    exchange(
        &mut running.peer,
        &commands,
        read(&snapshot, "learn-max", limit_request.clone()),
        limit_request,
        json!({"configurationKey":[{"key":"GetConfigurationMaxKeys","readonly":true,"value":"1"}]}),
    )
    .await;
    let over_limit = commands
        .submit(read(
            &snapshot,
            "over-limit",
            json!({"key":["HeartbeatInterval","NumberOfConnectors"]}),
        ))
        .await
        .unwrap();
    assert!(matches!(
        over_limit.lifecycle,
        CommandLifecycle::Rejected {
            error: CommandError {
                code: CommandErrorCode::InvalidParameters,
                ..
            }
        }
    ));
    let linked = coordinator
        .reconcile_configuration_observation(
            RequestId::new("change-private").unwrap(),
            RequestId::new("read-private").unwrap(),
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(linked.configuration_observations.len(), 1);
    assert!(linked.configuration_observations[0].key.redacted);
    assert!(
        !serde_json::to_string(&linked)
            .unwrap()
            .contains("private-secret")
    );
    assert!(
        !serde_json::to_string(
            &store
                .command_by_request_id(RequestId::new("change-private").unwrap())
                .await
                .unwrap()
        )
        .unwrap()
        .contains("private-secret")
    );
    let records = diagnostics_rx.try_iter().collect::<Vec<_>>();
    assert!(!records.is_empty(), "command diagnostics must be captured");
    assert!(
        records.iter().all(
            |record| !String::from_utf8_lossy(record.encoded_json()).contains("private-secret")
        )
    );
    running.task.shutdown(Duration::from_secs(1)).await.unwrap();
    running.server.abort();
    store.shutdown(Duration::from_secs(1)).await.unwrap();
    let restored = database.open();
    let durable = restored
        .command_result_by_request_id(RequestId::new("change-private").unwrap())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(durable.configuration_observations.len(), 1);
    assert_eq!(durable.schema_version, ContractVersion::V1_CONFIGURATION);
    for (id, expected_keys, expected_unknown) in [
        ("read-all", None, None),
        ("get-configuration-empty-keys", Some(0), Some(0)),
        ("unknown-only", None, Some(1)),
        ("known-only-empty-unknown", Some(0), None),
    ] {
        let result = restored
            .command_result_by_request_id(RequestId::new(id).unwrap())
            .await
            .unwrap()
            .unwrap();
        let Some(ConfigurationResult::Read {
            keys, unknown_keys, ..
        }) = result.configuration
        else {
            panic!("restored read evidence")
        };
        assert_eq!(
            keys.as_ref().map(Vec::len),
            expected_keys,
            "{id}: restored known keys"
        );
        assert_eq!(
            unknown_keys.as_ref().map(Vec::len),
            expected_unknown,
            "{id}: restored unknown keys"
        );
    }
    restored.shutdown(Duration::from_secs(1)).await.unwrap();
    drop(provider);
}
