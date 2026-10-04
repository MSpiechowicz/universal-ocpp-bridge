mod calls;
mod effects;
mod session;
mod state;
mod status;
mod trigger;
mod trigger201;

use calls::{apply_observation, call_error, context, event_identity};
pub(super) use state::reconcile;
use state::{invalidation, store_snapshot};

use std::{io, sync::Arc, time::Duration};

use tokio::{sync::watch, task::JoinSet};
use uob_application::{CommandClock, registration::RegistrationDecision};
use uob_contracts::{
    AvailabilityState, Connectivity, EventId, ProtocolEdition, ResourceRef, ServiceIdentity,
    StationSnapshot, TargetInstanceId, TriggerMessageClass201, TriggerTarget201, UtcTimestamp,
};
use uob_protocol_adapter::{
    CallSessionConfiguration, IncomingCall, OcppCallError, OcppErrorCode, StationConnection,
    spawn_call_session, v16, v201,
};
use uob_provider_adapter::{LocalAuthorizationProvider, LocalChargingIdentityProvider};

use super::{
    ChargingAuthorization, ChargingRuntime, ChargingStore, StationSettings, commands::LiveCommands,
};

pub(super) struct Clock;
impl CommandClock for Clock {
    fn now(&self) -> UtcTimestamp {
        UtcTimestamp::new(time::OffsetDateTime::now_utc())
    }
}
fn unavailable() -> io::Error {
    io::Error::other("charging station state unavailable")
}

#[derive(Clone)]
struct StationContext {
    store: ChargingStore,
    authorization: Arc<ChargingAuthorization>,
    commands: Arc<LiveCommands>,
    commands_enabled: bool,
    application: uob_application::Application,
    identity: ServiceIdentity,
    target: Option<(TargetInstanceId, u64)>,
}

struct CallContext<'a> {
    store: &'a ChargingStore,
    authorization: &'a ChargingAuthorization,
    commands: &'a Arc<LiveCommands>,
    identity: &'a ServiceIdentity,
    target: Option<(TargetInstanceId, u64)>,
    trigger_enabled: bool,
}

#[derive(Default)]
struct CommitState {
    committed: Option<EventId>,
    trigger_committed: bool,
}

pub(super) async fn serve(
    runtime: ChargingRuntime,
    application: uob_application::Application,
    stop: impl Future<Output = ()>,
) -> io::Result<()> {
    let ChargingRuntime {
        state,
        listener,
        endpoint,
        mut receiver,
        resources,
        settings,
        target,
    } = runtime;
    let context = StationContext {
        store: state.store.clone(),
        authorization: state.authorization.clone(),
        commands: state.commands.clone(),
        commands_enabled: state.credentials.is_some(),
        identity: application.identity().clone(),
        application,
        target,
    };
    let (shutdown, _) = watch::channel(false);
    let mut tasks = JoinSet::new();
    let mut trigger_cursor = None;
    let mut trigger_timer = tokio::time::interval(Duration::from_secs(1));
    let result = {
        let server = endpoint.serve_plaintext(listener);
        tokio::pin!(server);
        tokio::pin!(stop);
        loop {
            tokio::select! {
                biased;
                result = &mut server => break Err(io::Error::other(format!("charging listener stopped: {result:?}"))),
                () = &mut stop => break Ok(()),
                _ = trigger_timer.tick(), if context.commands_enabled => {
                    if let Err(error) = trigger::sweep(&context.store, &context.commands, &mut trigger_cursor).await {
                        break Err(error);
                    }
                },
                result = tasks.join_next(), if !tasks.is_empty() => {
                    if let Some(result) = result {
                        match result {
                            Ok(Ok(())) => (),
                            _ => break Err(unavailable()),
                        }
                    }
                },
                connection = receiver.receive() => {
                    let Some(connection) = connection else { break Err(unavailable()); };
                    let Some(mapped) = resources.get(&connection.station().station_id) else { break Err(unavailable()); };
                    let Some(configuration) = settings.get(&connection.station().station_id) else { break Err(unavailable()); };
                    tasks.spawn(station(connection, mapped.clone(), configuration.clone(), context.clone(), shutdown.subscribe()));
                }
            }
        }
    };
    let _ = shutdown.send(true);
    while !tasks.is_empty() {
        if !matches!(
            tokio::time::timeout(Duration::from_secs(5), tasks.join_next()).await,
            Ok(Some(Ok(Ok(()))))
        ) {
            tasks.abort_all();
            return Err(unavailable());
        }
    }
    result
}

async fn station(
    connection: StationConnection,
    resources: Vec<ResourceRef>,
    configuration: StationSettings,
    context: StationContext,
    mut stop: watch::Receiver<bool>,
) -> io::Result<()> {
    let station = connection.station().clone();
    let StationContext {
        store,
        authorization,
        commands,
        application,
        identity,
        target,
        ..
    } = &context;
    let mut snapshot = state::connected_snapshot(
        store,
        &resources,
        &configuration,
        station.protocol,
        identity,
    )
    .await?;
    let (handle, mut outputs, task) = spawn_call_session(
        connection,
        application,
        CallSessionConfiguration {
            pending_call_capacity: 16,
            incoming_call_capacity: 16,
            diagnostic_capacity: 16,
            response_timeout: Duration::from_secs(30),
        },
    )
    .map_err(|_| unavailable())?;
    let generation = session::attach(
        &context,
        &station.station_id,
        station.protocol,
        &configuration,
        &handle,
        &snapshot,
    )
    .await?;
    drop(handle);
    let mut error = None;
    loop {
        tokio::select! {
            biased;
            changed = stop.changed() => { if changed.is_err() || *stop.borrow() { break; } },
            call = outputs.incoming.receive() => {
                let Some(call) = call else { break; };
                let _profile_commit = if let Some(generation) =
                    generation.filter(|_| call.call.action.as_str() == "TransactionEvent")
                {
                    if let Ok(commit) = commands.begin_snapshot_commit(&station.station_id, generation) {
                        commit
                    } else {
                        error = Some(unavailable());
                        break;
                    }
                } else { None };
                let call_context = CallContext {
                    store,
                    authorization,
                    commands,
                    identity,
                    target: target.clone(),
                    trigger_enabled: configuration.control.trigger_message.enabled()
                        && context.commands_enabled,
                };
                if let Err(failure) = handle_call(call, &mut snapshot, call_context).await {
                    error = Some(failure); break;
                }
                if generation.is_some_and(|generation| commands.update(&station.station_id, generation, snapshot.clone()).is_err()) {
                    error = Some(unavailable()); break;
                }
            }
        }
    }
    if let Some(generation) = generation {
        commands.remove(&station.station_id, generation);
    }
    if task.shutdown(Duration::from_secs(3)).await.is_err() {
        error = Some(unavailable());
    }
    snapshot.connectivity = Connectivity::Disconnected;
    snapshot.observed_at = Clock.now();
    for resource in &mut snapshot.resources {
        resource.availability = AvailabilityState::Unknown;
    }
    if store_snapshot(store, snapshot, identity).await.is_err() {
        error = Some(unavailable());
    }
    error.map_or(Ok(()), Err)
}

async fn handle_call(
    incoming: IncomingCall,
    snapshot: &mut StationSnapshot,
    services: CallContext<'_>,
) -> io::Result<()> {
    let protocol = match &snapshot.connectivity {
        Connectivity::Connected { protocol, .. } => *protocol,
        _ => return Err(unavailable()),
    };
    if matches!(
        incoming.call.action.as_str(),
        "BootNotification" | "Heartbeat"
    ) {
        return complete_registration(incoming, snapshot, &services, protocol).await;
    }
    dispatch_call(incoming, snapshot, services, protocol).await
}

async fn complete_registration(
    incoming: IncomingCall,
    snapshot: &mut StationSnapshot,
    services: &CallContext<'_>,
    protocol: ProtocolEdition,
) -> io::Result<()> {
    let now = Clock.now();
    let event = invalidation(services.store, snapshot, services.identity, now).await?;
    let marker = if services.trigger_enabled {
        match protocol {
            ProtocolEdition::Ocpp16j => {
                let class = if incoming.call.action.as_str() == "BootNotification" {
                    uob_contracts::TriggerMessageClass::BootNotification
                } else {
                    uob_contracts::TriggerMessageClass::Heartbeat
                };
                trigger::marker(
                    services.store,
                    &snapshot.station,
                    services.identity,
                    class,
                    None,
                    None,
                    now,
                )
                .await?
            }
            ProtocolEdition::Ocpp201 => {
                let class = if incoming.call.action.as_str() == "BootNotification" {
                    TriggerMessageClass201::BootNotification
                } else {
                    TriggerMessageClass201::Heartbeat
                };
                trigger201::marker(
                    services.store,
                    &snapshot.station,
                    services.identity,
                    class,
                    TriggerTarget201::Station,
                    None,
                    now,
                )
                .await?
            }
        }
    } else {
        None
    };
    let trigger_committed = marker.is_some();
    // Registration commit futures are large; box only these uncommon branches,
    // leaving the ordinary incoming CALL path allocation-free.
    let result = if let Some(marker) = marker {
        Box::pin(incoming.complete_registration_with_trigger(
            services.store,
            snapshot,
            RegistrationDecision::Accepted,
            60,
            now,
            (event, Some(marker)),
        ))
        .await
    } else {
        Box::pin(incoming.complete_registration_with_invalidation(
            services.store,
            snapshot,
            RegistrationDecision::Accepted,
            60,
            now,
            event,
        ))
        .await
    };
    if result.is_ok() && trigger_committed {
        trigger::sweep(services.store, services.commands, &mut None).await?;
    }
    if result.is_err_and(|error| error.code == OcppErrorCode::InternalError) {
        Err(unavailable())
    } else {
        Ok(())
    }
}

async fn dispatch_call(
    incoming: IncomingCall,
    snapshot: &mut StationSnapshot,
    mut services: CallContext<'_>,
    protocol: ProtocolEdition,
) -> io::Result<()> {
    let mut commits = CommitState::default();
    let response = match (protocol, incoming.call.action.as_str()) {
        (_, "StatusNotification") => {
            status::complete_status(incoming.call, snapshot, &services, protocol, &mut commits)
                .await?
        }
        (ProtocolEdition::Ocpp16j, "Authorize") => {
            v16::complete_authorization(
                incoming.call,
                &snapshot.station,
                services.authorization,
                &LocalAuthorizationProvider,
                &Clock,
                Duration::from_secs(2),
            )
            .await
        }
        (ProtocolEdition::Ocpp16j, "StartTransaction" | "StopTransaction") => {
            let context = context(
                services.store,
                services.identity,
                &incoming,
                services.target.take(),
            )
            .await?;
            commits.committed = Some(context.event_id.clone());
            let transaction_services = v16::TransactionServices {
                store: services.store,
                authorization: services.authorization,
                provider: &LocalAuthorizationProvider,
                clock: &Clock,
                authorization_timeout: Duration::from_secs(2),
            };
            v16::complete_transaction(incoming.call, snapshot, &transaction_services, context).await
        }
        (ProtocolEdition::Ocpp201, "Authorize") => {
            v201::complete_authorization(
                incoming.call,
                &snapshot.station,
                services.authorization,
                &LocalChargingIdentityProvider,
                &Clock,
                Duration::from_secs(2),
            )
            .await
        }
        (ProtocolEdition::Ocpp201, "TransactionEvent") | (_, "MeterValues") => {
            complete_observation(&incoming, snapshot, &mut services, protocol, &mut commits).await
        }
        (
            ProtocolEdition::Ocpp16j,
            "DiagnosticsStatusNotification" | "FirmwareStatusNotification",
        ) => status::complete_trigger_status(&incoming, snapshot, &services, &mut commits).await?,
        (
            ProtocolEdition::Ocpp201,
            "LogStatusNotification"
            | "FirmwareStatusNotification"
            | "PublishFirmwareStatusNotification"
            | "SignCertificate",
        ) => {
            status::complete_trigger_receipt_201(&incoming, snapshot, &services, &mut commits)
                .await?
        }
        _ => Err(call_error(protocol, OcppErrorCode::NotImplemented)),
    };
    if let (Ok(_), Some(event_id)) = (&response, commits.committed) {
        effects::reconcile(
            services.store,
            services.commands,
            &snapshot.station,
            event_id,
        )
        .await?;
    }
    if commits.trigger_committed {
        trigger::sweep(services.store, services.commands, &mut None).await?;
    }
    let failed_storage = response
        .as_ref()
        .is_err_and(|error| error.code == OcppErrorCode::InternalError);
    match response {
        Ok(response) => incoming
            .responder
            .respond(&response[2])
            .map_err(|_| unavailable())?,
        Err(error) => incoming
            .responder
            .reject(error)
            .map_err(|_| unavailable())?,
    }
    if failed_storage {
        Err(unavailable())
    } else {
        Ok(())
    }
}

async fn complete_observation(
    incoming: &IncomingCall,
    snapshot: &mut StationSnapshot,
    services: &mut CallContext<'_>,
    protocol: ProtocolEdition,
    commits: &mut CommitState,
) -> Result<serde_json::Value, OcppCallError> {
    let accepted = match protocol {
        ProtocolEdition::Ocpp16j => uob_application::registration::accepted(snapshot),
        ProtocolEdition::Ocpp201 => uob_application::registration::v201::accepted(snapshot),
    };
    if accepted.is_err() {
        Err(call_error(protocol, OcppErrorCode::ProtocolError))
    } else {
        apply_observation(incoming, snapshot, services, Clock.now(), commits).await
    }
}
