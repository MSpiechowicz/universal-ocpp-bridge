mod calls;
mod dispatch;
mod effects;
mod firmware;
mod negotiation201;
mod session;
mod state;
mod status;
mod trigger;
mod trigger201;
use dispatch::dispatch_call;

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
    spawn_call_session,
};

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
    credentials: Option<Arc<super::control_auth::ControlCredentials>>,
}

struct CallContext<'a> {
    store: &'a ChargingStore,
    authorization: &'a ChargingAuthorization,
    commands: &'a Arc<LiveCommands>,
    identity: &'a ServiceIdentity,
    target: Option<(TargetInstanceId, u64)>,
    trigger_enabled: bool,
    reservations: Option<&'a uob_protocol_adapter::v16::remote_control::ReservationValues16>,
    reservations_201: Option<&'a uob_protocol_adapter::v201::remote_control::ReservationValues201>,
    negotiation: uob_application::NegotiationPolicy201,
    /// Native firmware family whose notifications reconcile durable jobs.
    firmware: Option<crate::configuration::charging::StationFirmware>,
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
        artifact_server,
    } = runtime;
    let context = StationContext {
        store: state.store.clone(),
        authorization: state.authorization.clone(),
        commands: state.commands.clone(),
        commands_enabled: state.credentials.is_some(),
        credentials: state.credentials.clone(),
        identity: application.identity().clone(),
        application,
        target,
    };
    let (shutdown, _) = watch::channel(false);
    let mut tasks = JoinSet::new();
    if let Some(server) = artifact_server {
        tasks.spawn(super::firmware::ArtifactServer::serve(
            server.providers,
            server.listener,
            shutdown.subscribe(),
        ));
    }
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
                _ = trigger_timer.tick() => {
                    if context.commands_enabled
                        && let Err(error) = trigger::sweep(&context.store, &context.commands, &mut trigger_cursor).await
                    {
                        break Err(error);
                    }
                    if uob_application::ReservationStore16::expire_reservations_16(&context.store, Clock.now()).await.is_err()
                        || uob_application::ReservationStore201::expire_reservations_201(&context.store, Clock.now()).await.is_err()
                        || uob_application::FirmwareStore16::expire_firmware_jobs_16(&context.store, Clock.now()).await.is_err()
                        || uob_application::FirmwareStore201::expire_firmware_jobs_201(&context.store, Clock.now()).await.is_err()
                    {
                        break Err(unavailable());
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
        commands,
        application,
        identity,
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
            diagnostic = outputs.diagnostics.recv() => {
                if let Some(diagnostic) = diagnostic {
                    if super::reservations::late_response(store, &snapshot, diagnostic).await.is_err() {
                        error = Some(unavailable()); break;
                    }
                }
                else { break; }
            },
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
                if let Err(failure) = handle_call(call, &mut snapshot, call_context(&context, &configuration)).await {
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

fn call_context<'a>(
    context: &'a StationContext,
    configuration: &'a StationSettings,
) -> CallContext<'a> {
    CallContext {
        store: &context.store,
        authorization: &context.authorization,
        commands: &context.commands,
        identity: &context.identity,
        target: context.target.clone(),
        trigger_enabled: configuration.control.trigger_message.enabled()
            && context.commands_enabled,
        reservations: configuration.reservations.as_deref(),
        reservations_201: configuration.reservations_201.as_deref(),
        // Processing promises a later TxProfile, which only an enabled command path can send.
        negotiation: uob_application::NegotiationPolicy201 {
            charging_needs_processing: configuration.control.ev_charging_needs_processing
                && context.commands_enabled,
        },
        firmware: configuration
            .firmware
            .filter(|_| configuration.firmware_providers.is_some()),
    }
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
