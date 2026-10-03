use crate::support::*;
use serde_json::{Value, json};
use std::sync::Arc;
use uob_application::*;
use uob_contracts::*;
use uob_protocol_adapter::v201::remote_control::{
    LocalRemoteStartIdentity, RemoteControlSession, RemoteStartIdentity, configuration201_values::*,
};
use uob_provider_adapter::LocalChargingIdentityProvider;

pub fn reference(index: usize) -> String {
    format!("cfg201:{index:064x}")
}
pub fn entry(name: &str, index: usize) -> SetVariableReference201 {
    SetVariableReference201 {
        component: DeviceComponent201 {
            name: "VendorCtrlr".into(),
            instance: None,
            evse: None,
        },
        variable: DeviceVariable201 {
            name: name.into(),
            instance: None,
        },
        attribute_type: None,
        value_reference: reference(index),
    }
}
pub async fn configured(
    store: &Store,
    running: &RunningSession,
    variables: Vec<(SetVariableReference201, String)>,
    profile: Option<&Value>,
    clock: Arc<dyn CommandClock>,
) -> (
    StationSnapshot,
    Arc<LocalConfigurationValues201>,
    Arc<RemoteControlSession>,
    Arc<ScopedCommandAdmissionPort<Value>>,
) {
    let (snapshot, auth, _, _) = setup_device(store, running.handle.clone()).await;
    let identity = Arc::new(
        LocalRemoteStartIdentity::new(vec![], &LocalChargingIdentityProvider, auth)
            .await
            .unwrap(),
    );
    configured_session(
        store, running, snapshot, identity, variables, profile, clock,
    )
}

fn configured_session(
    store: &Store,
    running: &RunningSession,
    mut snapshot: StationSnapshot,
    identity: Arc<dyn RemoteStartIdentity>,
    variables: Vec<(SetVariableReference201, String)>,
    profile: Option<&Value>,
    clock: Arc<dyn CommandClock>,
) -> (
    StationSnapshot,
    Arc<LocalConfigurationValues201>,
    Arc<RemoteControlSession>,
    Arc<ScopedCommandAdmissionPort<Value>>,
) {
    for action in ["SetVariables", "SetNetworkProfile"] {
        snapshot.capabilities.operations.push(SupportedOperation {
            operation: Operation::ProtocolAction {
                protocol: ProtocolEdition::Ocpp201,
                action: action.into(),
            },
            parameters: vec![],
        });
    }
    let variables = variables
        .into_iter()
        .map(|(entry, value)| ProtectedVariableValue201 {
            resource: snapshot.station.clone(),
            entry,
            value: ProtectedVariableText201::new(value).unwrap(),
            expires_at: time("2026-09-01T03:00:00Z"),
        })
        .collect();
    let profiles = profile
        .into_iter()
        .map(|profile| ProtectedNetworkProfileValue201 {
            resource: snapshot.station.clone(),
            configuration_slot: 0,
            reference: reference(999),
            profile: ProtectedNetworkProfile201::from_json_bytes(
                serde_json::to_vec(profile).unwrap(),
            )
            .unwrap(),
            expires_at: time("2026-09-01T03:00:00Z"),
        })
        .collect();
    let provider = Arc::new(LocalConfigurationValues201::new(variables, profiles).unwrap());
    let port = Arc::new(
        RemoteControlSession::new(
            running.handle.clone(),
            snapshot.clone(),
            identity,
            clock.clone(),
            Arc::new(store.clone()),
        )
        .unwrap()
        .with_configuration_201(provider.clone())
        .with_device_model(Arc::new(store.clone()), 7),
    );
    let coordinator = Arc::new(Coordinator::new(
        Arc::new(store.clone()),
        port.clone(),
        clock,
    ));
    let commands = Arc::new(scoped(
        coordinator,
        &snapshot,
        vec![
            AccessPermission::Control,
            AccessPermission::PrivilegedControl,
        ],
    ));
    (snapshot, provider, port, commands)
}
pub fn external(
    snapshot: &StationSnapshot,
    id: &str,
    action: &str,
    payload: Value,
) -> ExternalCommand<Value> {
    let schema = match action {
        "SetVariables" => SET_VARIABLES_REFERENCE_SCHEMA_201,
        "SetNetworkProfile" => SET_NETWORK_PROFILE_REFERENCE_SCHEMA_201,
        _ => unreachable!(),
    };
    let mut request = command(
        snapshot,
        id,
        CommandOperation::Ocpp(PrivilegedOcppOperation {
            protocol: ProtocolEdition::Ocpp201,
            action: ProtocolActionName::new(action).unwrap(),
            payload_schema: PayloadSchemaId::new(schema).unwrap(),
            payload,
        }),
    );
    request.request.resource = snapshot.station.clone();
    request
}
pub fn variables(
    snapshot: &StationSnapshot,
    id: &str,
    entries: &[SetVariableReference201],
) -> ExternalCommand<Value> {
    external(
        snapshot,
        id,
        "SetVariables",
        json!({"setVariableData":entries}),
    )
}
pub async fn learn(
    running: &mut RunningSession,
    snapshot: &StationSnapshot,
    commands: &Arc<ScopedCommandAdmissionPort<Value>>,
    items: usize,
    bytes: usize,
    suffix: &str,
) {
    for (name, number) in [("ItemsPerMessage", items), ("BytesPerMessage", bytes)] {
        let id = format!("learn-{name}-{suffix}");
        let component = json!({"name":"DeviceDataCtrlr"});
        let variable = json!({"name":name,"instance":"SetVariables"});
        let mut request = command(
            snapshot,
            &id,
            protocol(
                "GetVariables",
                json!({"getVariableData":[{"component":component,"variable":variable}]}),
            ),
        );
        request.request.resource = snapshot.station.clone();
        let commands = commands.clone();
        let submitted = tokio::spawn(async move { commands.submit(request).await.unwrap() });
        assert_eq!(
            receive_json(&mut running.peer).await,
            json!([2,id,"GetVariables",{"getVariableData":[{"component":component,"variable":variable}]}])
        );
        running.peer.send_text(json!([3,id,{"getVariableResult":[{"component":component,"variable":variable,"attributeStatus":"Accepted","attributeValue":number.to_string()}]}]).to_string()).await.unwrap();
        assert!(matches!(
            submitted.await.unwrap().lifecycle,
            CommandLifecycle::ProtocolResponse { accepted: true, .. }
        ));
    }
}
pub async fn stop(running: RunningSession, store: Store) {
    running
        .task
        .shutdown(std::time::Duration::from_secs(1))
        .await
        .unwrap();
    running.server.abort();
    store
        .shutdown(std::time::Duration::from_secs(1))
        .await
        .unwrap();
}

pub fn assert_no_secrets(database: &Database, markers: &[&str]) {
    for suffix in ["", "-wal", "-shm"] {
        if let Ok(bytes) = std::fs::read(database.0.join(format!("state.db{suffix}"))) {
            for marker in markers {
                assert!(
                    !bytes
                        .windows(marker.len())
                        .any(|window| window == marker.as_bytes()),
                    "protected native material reached durable storage"
                );
            }
        }
    }
}
