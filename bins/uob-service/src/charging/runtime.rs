use std::{collections::BTreeMap, io, sync::Arc, time::Duration};

use serde_json::json;
use tokio::{sync::watch, task::JoinSet};
use uob_application::{
    AtomicStoreWrite, ChargerObservation, CommandClock, ObservationCommitError, OperationalStore,
    record_measurements, record_transaction_event,
    registration::{RegistrationDecision, availability::AvailabilityContext},
    transaction16::TransactionContext,
};
use uob_contracts::{
    AvailabilityState, ChargingResourceSnapshot, Connectivity, ContractVersion, EventEnvelope,
    EventId, EventOrigin, EventType, ProtocolEdition, ResourceCapabilities, ResourceRef,
    ServiceIdentity, StationEvent, StationId, StationSnapshot, TargetInstanceId, UtcTimestamp,
};
use uob_protocol_adapter::{
    CallSessionConfiguration, IncomingCall, OcppCallError, OcppErrorCode, StationConnection,
    spawn_call_session, v16, v201,
};
use uob_provider_adapter::{LocalAuthorizationProvider, LocalChargingIdentityProvider};

use super::{ChargingAuthorization, ChargingRuntime, ChargingStore};

struct Clock;
impl CommandClock for Clock {
    fn now(&self) -> UtcTimestamp {
        UtcTimestamp::new(time::OffsetDateTime::now_utc())
    }
}
fn unavailable() -> io::Error {
    io::Error::other("charging station state unavailable")
}

pub(super) async fn reconcile(
    store: &ChargingStore,
    resources: &BTreeMap<StationId, Vec<ResourceRef>>,
    identity: &ServiceIdentity,
) -> io::Result<()> {
    for expected in resources.values() {
        let Some(mut snapshot) = store
            .station_snapshot(expected[0].clone())
            .await
            .map_err(|_| unavailable())?
        else {
            continue;
        };
        if snapshot.station != expected[0]
            || snapshot
                .resources
                .iter()
                .map(|item| &item.resource)
                .ne(expected.iter().skip(1))
        {
            return Err(io::Error::other(
                "charging topology differs from persisted station state",
            ));
        }
        if snapshot.connectivity != Connectivity::Disconnected {
            snapshot.connectivity = Connectivity::Disconnected;
            for resource in &mut snapshot.resources {
                resource.availability = AvailabilityState::Unknown;
            }
            store_snapshot(store, snapshot, identity).await?;
        }
    }
    Ok(())
}

async fn store_snapshot(
    store: &ChargingStore,
    snapshot: StationSnapshot,
    identity: &ServiceIdentity,
) -> io::Result<()> {
    let event = invalidation(store, &snapshot, identity, snapshot.observed_at).await?;
    let mut write = AtomicStoreWrite::empty();
    write.station_snapshot = Some(snapshot);
    write.journal_events.push(event);
    store.write_atomic(write).await.map_err(|_| unavailable())?;
    Ok(())
}

async fn invalidation(
    store: &ChargingStore,
    snapshot: &StationSnapshot,
    identity: &ServiceIdentity,
    observed_at: UtcTimestamp,
) -> io::Result<EventEnvelope<StationEvent>> {
    let (sequence, event_id) = event_identity(store, identity).await?;
    Ok(EventEnvelope {
        event_id,
        schema_version: snapshot.schema_version,
        runtime: identity.runtime.clone(),
        resource: snapshot.station.clone(),
        source_time: None,
        observed_at,
        event_type: EventType::new("station.snapshot.invalidated").map_err(|_| unavailable())?,
        origin: EventOrigin::Bridge,
        sequence,
        correlation_id: None,
        causation_id: None,
        provenance: None,
        payload: StationEvent::Invalidation {
            station_snapshot_invalidated: snapshot.station.station_id.clone(),
        },
    })
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
        target,
    } = runtime;
    let store = state.store.clone();
    let authorization = state.authorization.clone();
    let identity = application.identity().clone();
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
                    tasks.spawn(station(connection, mapped.clone(), store.clone(), authorization.clone(), application.clone(), identity.clone(), target.clone(), shutdown.subscribe()));
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

#[allow(clippy::too_many_arguments)]
async fn station(
    connection: StationConnection,
    resources: Vec<ResourceRef>,
    store: ChargingStore,
    authorization: Arc<ChargingAuthorization>,
    application: uob_application::Application,
    identity: ServiceIdentity,
    target: Option<(TargetInstanceId, u64)>,
    mut stop: watch::Receiver<bool>,
) -> io::Result<()> {
    let station = connection.station().clone();
    let now = Clock.now();
    let mut snapshot = store
        .station_snapshot(resources[0].clone())
        .await
        .map_err(|_| unavailable())?
        .unwrap_or_else(|| StationSnapshot {
            schema_version: ContractVersion::V1_INITIAL,
            station: resources[0].clone(),
            observed_at: now,
            connectivity: Connectivity::Disconnected,
            capabilities: ResourceCapabilities::default(),
            resources: resources
                .iter()
                .skip(1)
                .cloned()
                .map(|resource| ChargingResourceSnapshot {
                    resource,
                    availability: AvailabilityState::Unknown,
                    capabilities: ResourceCapabilities::default(),
                    data_points: vec![],
                    current_values: vec![],
                })
                .collect(),
            transactions: vec![],
            current_values: vec![],
        });
    snapshot.connectivity = Connectivity::Connected {
        protocol: station.protocol,
        connected_at: now,
        last_message_at: None,
    };
    // A transport reconnect is not a new accepted BootNotification, even after a clean restart.
    snapshot
        .current_values
        .retain(|value| !value.point_id.as_str().ends_with("/registration/status"));
    snapshot.observed_at = now;
    store_snapshot(&store, snapshot.clone(), &identity).await?;
    let (handle, mut outputs, task) = spawn_call_session(
        connection,
        &application,
        CallSessionConfiguration {
            pending_call_capacity: 16,
            incoming_call_capacity: 16,
            diagnostic_capacity: 16,
            response_timeout: Duration::from_secs(30),
        },
    )
    .map_err(|_| unavailable())?;
    drop(handle);
    let mut error = None;
    loop {
        tokio::select! {
            biased;
            changed = stop.changed() => { if changed.is_err() || *stop.borrow() { break; } },
            call = outputs.incoming.receive() => {
                let Some(call) = call else { break; };
                if let Err(failure) = handle_call(call, &mut snapshot, &store, &authorization, &identity, target.clone()).await {
                    error = Some(failure); break;
                }
            }
        }
    }
    if task.shutdown(Duration::from_secs(3)).await.is_err() {
        error = Some(unavailable());
    }
    snapshot.connectivity = Connectivity::Disconnected;
    snapshot.observed_at = Clock.now();
    for resource in &mut snapshot.resources {
        resource.availability = AvailabilityState::Unknown;
    }
    if store_snapshot(&store, snapshot, &identity).await.is_err() {
        error = Some(unavailable());
    }
    error.map_or(Ok(()), Err)
}

async fn handle_call(
    incoming: IncomingCall,
    snapshot: &mut StationSnapshot,
    store: &ChargingStore,
    authorization: &ChargingAuthorization,
    identity: &ServiceIdentity,
    target: Option<(TargetInstanceId, u64)>,
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
        let event = invalidation(store, snapshot, identity, now).await?;
        let result = incoming
            .complete_registration_with_invalidation(
                store,
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
    dispatch_call(
        incoming,
        snapshot,
        store,
        authorization,
        identity,
        target,
        protocol,
    )
    .await
}

async fn dispatch_call(
    incoming: IncomingCall,
    snapshot: &mut StationSnapshot,
    store: &ChargingStore,
    authorization: &ChargingAuthorization,
    identity: &ServiceIdentity,
    target: Option<(TargetInstanceId, u64)>,
    protocol: ProtocolEdition,
) -> io::Result<()> {
    let response = match (protocol, incoming.call.action.as_str()) {
        (_, "StatusNotification") => {
            let (sequence, event_id) = event_identity(store, identity).await?;
            let context = AvailabilityContext {
                identity: identity.clone(),
                event_id,
                sequence,
            };
            match protocol {
                ProtocolEdition::Ocpp16j => {
                    v16::availability::complete_status(
                        incoming.call,
                        store,
                        snapshot,
                        context,
                        Clock.now(),
                    )
                    .await
                }
                ProtocolEdition::Ocpp201 => {
                    v201::availability::complete_status(
                        incoming.call,
                        store,
                        snapshot,
                        context,
                        Clock.now(),
                    )
                    .await
                }
            }
        }
        (ProtocolEdition::Ocpp16j, "StartTransaction" | "StopTransaction") => {
            let context = context(store, identity, &incoming, target).await?;
            let services = v16::TransactionServices {
                store,
                authorization,
                provider: &LocalAuthorizationProvider,
                clock: &Clock,
                authorization_timeout: Duration::from_secs(2),
            };
            v16::complete_transaction(incoming.call, snapshot, &services, context).await
        }
        (ProtocolEdition::Ocpp201, "Authorize") => {
            v201::complete_authorization(
                incoming.call,
                &snapshot.station,
                authorization,
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
                apply_observation(&incoming, snapshot, store, identity, target, now).await
            }
        }
        _ => Err(call_error(protocol, OcppErrorCode::NotImplemented)),
    };
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

async fn apply_observation(
    incoming: &IncomingCall,
    snapshot: &mut StationSnapshot,
    store: &ChargingStore,
    identity: &ServiceIdentity,
    target: Option<(TargetInstanceId, u64)>,
    now: UtcTimestamp,
) -> Result<serde_json::Value, OcppCallError> {
    let protocol = match &snapshot.connectivity {
        Connectivity::Connected { protocol, .. } => *protocol,
        _ => {
            return Err(call_error(
                ProtocolEdition::Ocpp16j,
                OcppErrorCode::InternalError,
            ));
        }
    };
    let context = context(store, identity, incoming, target)
        .await
        .map_err(|_| call_error(protocol, OcppErrorCode::InternalError))?;
    let reply = match &incoming.call.observation {
        ChargerObservation::TransactionEvent(observation)
            if protocol == ProtocolEdition::Ocpp201 =>
        {
            record_transaction_event(store, snapshot, observation, context, now)
                .await
                .map_err(|error| commit_error(protocol, &error))?;
            if observation.event == uob_application::TransactionEventKind::Started {
                // A transaction report is not evidence of an authorization grant.
                json!({"idTokenInfo":{"status":"Invalid"}})
            } else {
                json!({})
            }
        }
        ChargerObservation::Measurements(measurements) if measurements.protocol == protocol => {
            record_measurements(store, snapshot, measurements, context, now)
                .await
                .map_err(|error| commit_error(protocol, &error))?;
            json!({})
        }
        _ => return Err(call_error(protocol, OcppErrorCode::NotImplemented)),
    };
    Ok(json!([3, incoming.call.message_id, reply]))
}

async fn context(
    store: &ChargingStore,
    identity: &ServiceIdentity,
    incoming: &IncomingCall,
    target: Option<(TargetInstanceId, u64)>,
) -> io::Result<TransactionContext> {
    let (sequence, event_id) = event_identity(store, identity).await?;
    Ok(TransactionContext {
        identity: identity.clone(),
        event_id,
        sequence,
        correlation_id: Some(incoming.correlation_id.clone()),
        target,
        delivery_deadline: UtcTimestamp::new(Clock.now().into_inner() + time::Duration::days(1)),
    })
}

async fn event_identity(
    store: &ChargingStore,
    identity: &ServiceIdentity,
) -> io::Result<(u64, EventId)> {
    let sequence = store
        .reserve_event_sequence()
        .await
        .map_err(|_| unavailable())?;
    let event_id = EventId::new(format!("{}/event/{sequence}", identity.bridge_id.as_str()))
        .map_err(|_| unavailable())?;
    Ok((sequence, event_id))
}

fn commit_error(protocol: ProtocolEdition, error: &ObservationCommitError) -> OcppCallError {
    let code = if matches!(error, ObservationCommitError::Storage(_)) {
        OcppErrorCode::InternalError
    } else {
        OcppErrorCode::ProtocolError
    };
    call_error(protocol, code)
}

fn call_error(protocol: ProtocolEdition, code: OcppErrorCode) -> OcppCallError {
    OcppCallError {
        protocol,
        code,
        description: "Charging operation could not be completed",
        field_path: None,
    }
}
