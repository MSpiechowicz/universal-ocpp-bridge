mod calls;
mod effects;
mod session;
mod state;

use calls::{apply_observation, call_error, context, event_identity};
pub(super) use state::reconcile;
use state::{invalidation, store_snapshot};

use std::{io, sync::Arc, time::Duration};

use tokio::{sync::watch, task::JoinSet};
use uob_application::{
    CommandClock,
    registration::{RegistrationDecision, availability::AvailabilityContext},
};
use uob_contracts::{
    AvailabilityState, Connectivity, EventId, ProtocolEdition, ResourceRef, ServiceIdentity,
    StationSnapshot, TargetInstanceId, UtcTimestamp,
};
use uob_protocol_adapter::{
    CallSessionConfiguration, DecodedCall, IncomingCall, OcppCallError, OcppErrorCode,
    StationConnection, spawn_call_session, v16, v201,
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
    let result = {
        let server = endpoint.serve_plaintext(listener);
        tokio::pin!(server);
        tokio::pin!(stop);
        loop {
            tokio::select! {
                biased;
                result = &mut server => break Err(io::Error::other(format!("charging listener stopped: {result:?}"))),
                () = &mut stop => break Ok(()),
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
                let call_context = CallContext {
                    store,
                    authorization,
                    commands,
                    identity,
                    target: target.clone(),
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
        let now = Clock.now();
        let event = invalidation(services.store, snapshot, services.identity, now).await?;
        let result = incoming
            .complete_registration_with_invalidation(
                services.store,
                snapshot,
                RegistrationDecision::Accepted,
                60,
                now,
                event,
            )
            .await;
        return if result.is_err_and(|error| error.code == OcppErrorCode::InternalError) {
            Err(unavailable())
        } else {
            Ok(())
        };
    }
    dispatch_call(incoming, snapshot, services, protocol).await
}

async fn dispatch_call(
    incoming: IncomingCall,
    snapshot: &mut StationSnapshot,
    services: CallContext<'_>,
    protocol: ProtocolEdition,
) -> io::Result<()> {
    let mut committed = None;
    let response = match (protocol, incoming.call.action.as_str()) {
        (_, "StatusNotification") => {
            complete_status(
                incoming.call,
                snapshot,
                services.store,
                services.identity,
                protocol,
                &mut committed,
            )
            .await?
        }
        (ProtocolEdition::Ocpp16j, "StartTransaction" | "StopTransaction") => {
            let context = context(
                services.store,
                services.identity,
                &incoming,
                services.target,
            )
            .await?;
            committed = Some(context.event_id.clone());
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
            let now = Clock.now();
            let accepted = match protocol {
                ProtocolEdition::Ocpp16j => uob_application::registration::accepted(snapshot),
                ProtocolEdition::Ocpp201 => uob_application::registration::v201::accepted(snapshot),
            };
            if accepted.is_err() {
                Err(call_error(protocol, OcppErrorCode::ProtocolError))
            } else {
                apply_observation(
                    &incoming,
                    snapshot,
                    services.store,
                    services.identity,
                    services.target,
                    now,
                    &mut committed,
                )
                .await
            }
        }
        _ => Err(call_error(protocol, OcppErrorCode::NotImplemented)),
    };
    if let (Ok(_), Some(event_id)) = (&response, committed) {
        effects::reconcile(
            services.store,
            services.commands,
            &snapshot.station,
            event_id,
        )
        .await?;
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

async fn complete_status(
    call: DecodedCall,
    snapshot: &mut StationSnapshot,
    store: &ChargingStore,
    identity: &ServiceIdentity,
    protocol: ProtocolEdition,
    committed: &mut Option<EventId>,
) -> io::Result<Result<serde_json::Value, OcppCallError>> {
    let (sequence, event_id) = event_identity(store, identity).await?;
    *committed = Some(event_id.clone());
    let context = AvailabilityContext {
        identity: identity.clone(),
        event_id,
        sequence,
    };
    Ok(match protocol {
        ProtocolEdition::Ocpp16j => {
            v16::availability::complete_status(call, store, snapshot, context, Clock.now()).await
        }
        ProtocolEdition::Ocpp201 => {
            v201::availability::complete_status(call, store, snapshot, context, Clock.now()).await
        }
    })
}
