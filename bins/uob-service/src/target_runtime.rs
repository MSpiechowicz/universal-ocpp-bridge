//! Selected target host: one canonical charging store, bounded target session and durable outbox.
use std::{collections::BTreeMap, io, sync::Arc, time::Duration};

use serde_json::Value;
use tokio::sync::oneshot;
use uob_application::{
    AccessGrant, AccessPermission, AccessPolicy, AccessResourceScope, Application,
    CanonicalQuerySource, DeliveryAttempt, DeliveryId, DeliveryReport, DiagnosticDrop,
    OperationalStore, PageLimit, PendingDelivery, PendingDeliveryQuery, RecordedDeliveryAttempt,
    ScheduledDelivery, ScopedTargetQueryPort, SnapshotQuery, StationEvent, StorageError,
    StorageErrorCode, StorageFuture, TargetDelivery, TargetDeliveryClass, TargetDeliveryStore,
    TargetDiagnostic, TargetDiagnosticPort, TargetMessage, TargetPortFuture,
    TargetQueryAuthorization, TargetQueryPermission, TargetReportPort, TargetResourceScope,
    TargetRuntimeLimits,
};
use uob_contracts::{
    AuthenticatedCommandOrigin, PrincipalId, ResourceRef, TransactionSnapshot, UtcTimestamp,
};
use uob_target_adapter::{
    DeliveryRetryPolicy, StoredTargetMessage, TargetDeliveryIngress, TargetDeliveryWorkerOptions,
    TargetSessionOptions, TargetSessionPorts, ValidatedTargetSelection,
    spawn_target_delivery_worker, spawn_target_session_with_diagnostics, target_delivery_reports,
};

use crate::{charging::ChargingState, management_source::ManagementSource};

struct CanonicalDeliveries(crate::charging::ChargingStore);

impl TargetDeliveryStore<StoredTargetMessage<Value>> for CanonicalDeliveries {
    fn read_pending_deliveries(
        &self,
        query: PendingDeliveryQuery,
    ) -> StorageFuture<'_, Vec<ScheduledDelivery<StoredTargetMessage<Value>>>> {
        Box::pin(async move {
            let entries: Vec<ScheduledDelivery<TransactionSnapshot>> =
                self.0.read_pending_deliveries(query).await?;
            let mut result = Vec::with_capacity(entries.len());
            for entry in entries {
                let ScheduledDelivery {
                    delivery,
                    attempt_count,
                } = entry;
                let station = ResourceRef {
                    resource: None,
                    native_protocol_reference: None,
                    ..delivery.ordering_key.clone()
                };
                let event = self
                    .0
                    .journal_event_by_id(delivery.event_id.clone(), station)
                    .await?
                    .filter(|event| {
                        event.resource == delivery.ordering_key
                            && event.payload == StationEvent::Transaction(delivery.payload.clone())
                    })
                    .ok_or_else(|| {
                        StorageError::new(
                            StorageErrorCode::IntegrityFailure,
                            "target.delivery_event_mismatch",
                        )
                    })?;
                let uob_contracts::EventEnvelope {
                    event_id,
                    schema_version,
                    runtime,
                    resource,
                    source_time,
                    observed_at,
                    event_type,
                    origin,
                    sequence,
                    correlation_id,
                    causation_id,
                    provenance,
                    payload: source_payload,
                } = event;
                let payload = serde_json::to_value(source_payload).map_err(|_| {
                    StorageError::new(
                        StorageErrorCode::IntegrityFailure,
                        "target.delivery_encoding",
                    )
                })?;
                let event = uob_contracts::EventEnvelope {
                    event_id,
                    schema_version,
                    runtime,
                    resource,
                    source_time,
                    observed_at,
                    event_type,
                    origin,
                    sequence,
                    correlation_id,
                    causation_id,
                    provenance,
                    payload,
                };
                result.push(ScheduledDelivery {
                    delivery: PendingDelivery {
                        delivery_id: delivery.delivery_id,
                        event_id: delivery.event_id,
                        target_instance_id: delivery.target_instance_id,
                        target_configuration_revision: delivery.target_configuration_revision,
                        ordering_key: delivery.ordering_key,
                        deadline: delivery.deadline,
                        durability: delivery.durability,
                        payload: StoredTargetMessage::DomainEvent(event),
                    },
                    attempt_count,
                });
            }
            Ok(result)
        })
    }

    fn record_delivery_attempt(&self, attempt: DeliveryAttempt) -> StorageFuture<'_, ()> {
        self.0.record_delivery_attempt(attempt)
    }

    fn delivery_attempts(
        &self,
        delivery_id: DeliveryId,
        limit: PageLimit,
    ) -> StorageFuture<'_, Vec<RecordedDeliveryAttempt>> {
        self.0.delivery_attempts(delivery_id, limit)
    }
}

struct Reports(Arc<dyn TargetReportPort>);

impl TargetReportPort for Reports {
    fn report(&self, report: DeliveryReport) -> TargetPortFuture<'_, ()> {
        if report.delivery_id.as_str().starts_with("snapshot/") {
            // Snapshots are replaceable state, not critical outbox records. The periodic
            // bounded refresh reoffers them, including after a transient transport failure.
            Box::pin(async { Ok(()) })
        } else {
            self.0.report(report)
        }
    }
}

struct Diagnostics(uob_application::HealthMonitor);

impl TargetDiagnosticPort for Diagnostics {
    fn try_emit(&self, diagnostic: TargetDiagnostic) -> Result<(), DiagnosticDrop> {
        if let TargetDiagnostic::Health(health) = diagnostic {
            self.0.report_target(&health);
        }
        Ok(())
    }
}

fn command_policy(
    selection: &ValidatedTargetSelection<Value, Value>,
    roster: &[ResourceRef],
) -> io::Result<AccessPolicy> {
    if selection.catalog.kind.as_str()
        == uob_ems_scada_http_target_adapter::EMS_SCADA_HTTP_TARGET_KIND
    {
        let reference = match selection
            .configuration()
            .configuration()
            .setting("credentials_file")
        {
            Some(uob_application::ConfigurationValue::CredentialReference(reference)) => {
                Some(reference)
            }
            None => None,
            _ => return Err(io::Error::other("target.credentials_invalid")),
        };
        if reference.is_none() {
            let origin = AuthenticatedCommandOrigin::Target {
                target_instance_id: selection.target_id.clone(),
                principal_id: PrincipalId::new("target-host-read-only").expect("static principal"),
            };
            let scopes = roster
                .iter()
                .map(|resource| AccessResourceScope::Station {
                    bridge_id: resource.bridge_id.clone(),
                    station_id: resource.station_id.clone(),
                })
                .collect();
            let grant = AccessGrant::new(origin, vec![AccessPermission::Read], scopes)
                .map_err(|_| io::Error::other("target.command_scope_invalid"))?;
            return Ok(AccessPolicy::single(grant));
        }
        return uob_ems_scada_http_target_adapter::resolve_command_policy(
            reference,
            &selection.target_id,
        )
        .map_err(io::Error::other);
    }
    if selection.catalog.kind.as_str() != uob_mqtt_target_adapter::MQTT_TARGET_KIND {
        return Err(io::Error::other("target.kind_unsupported"));
    }
    let principal_id = PrincipalId::new(format!("mqtt-target:{}", selection.target_id.as_str()))
        .map_err(io::Error::other)?;
    let scopes = roster
        .iter()
        .map(|resource| AccessResourceScope::Station {
            bridge_id: resource.bridge_id.clone(),
            station_id: resource.station_id.clone(),
        })
        .collect();
    let grant = AccessGrant::new(
        AuthenticatedCommandOrigin::Target {
            target_instance_id: selection.target_id.clone(),
            principal_id,
        },
        vec![AccessPermission::Control],
        scopes,
    )
    .map_err(|_| io::Error::other("target.command_scope_invalid"))?;
    Ok(AccessPolicy::single(grant))
}

pub(crate) struct TargetRuntime {
    session: uob_target_adapter::TargetSessionTask,
    worker: uob_target_adapter::TargetDeliveryWorkerTask,
    ingress: TargetDeliveryIngress<Value>,
    store: crate::charging::ChargingStore,
    roster: Vec<ResourceRef>,
}

impl TargetRuntime {
    pub(crate) fn start(
        selection: &ValidatedTargetSelection<Value, Value>,
        state: &ChargingState,
        application: &Application,
    ) -> io::Result<Self> {
        let scopes = state
            .roster
            .iter()
            .map(|resource| TargetResourceScope::Station {
                bridge_id: resource.bridge_id.clone(),
                station_id: resource.station_id.clone(),
            })
            .collect();
        let authorization = TargetQueryAuthorization::new(
            selection.target_id.clone(),
            vec![
                TargetQueryPermission::StationSnapshots,
                TargetQueryPermission::DataPoints,
                TargetQueryPermission::Capabilities,
                TargetQueryPermission::CommandStatus,
                TargetQueryPermission::RetainedEvents,
            ],
            scopes,
        );
        let source: Arc<dyn CanonicalQuerySource<Value>> =
            Arc::new(ManagementSource::new(state.store.clone()));
        let queries = Arc::new(ScopedTargetQueryPort::new(source, authorization));
        let policy = command_policy(selection, &state.roster)?;
        let budget = Arc::new(application.health().resources().clone());
        let (reports, receiver) = target_delivery_reports(32).map_err(io::Error::other)?;
        let options = TargetSessionOptions {
            delivery_capacity: 32,
            critical_report_capacity: 32,
            runtime_limits: TargetRuntimeLimits {
                maximum_in_flight_deliveries: 16,
                maximum_in_flight_commands: 8,
                maximum_command_bytes: 64 * 1024,
            },
            // The session can live indefinitely; the host enforces the actual shutdown
            // duration through TargetSessionTask::shutdown rather than this startup timestamp.
            shutdown_deadline: UtcTimestamp::new(
                time::OffsetDateTime::now_utc() + time::Duration::days(3650),
            ),
        };
        let (ingress, session) = spawn_target_session_with_diagnostics(
            selection,
            TargetSessionPorts {
                queries,
                commands: state.command_port(application),
                command_authorization: policy,
                critical_reports: Arc::new(Reports(reports)),
                diagnostics: Arc::new(Diagnostics(application.health().clone())),
            },
            budget,
            options,
            application.diagnostics().clone(),
        )
        .map_err(io::Error::other)?;
        let worker = spawn_target_delivery_worker(
            Arc::new(CanonicalDeliveries(state.store.clone())),
            ingress.clone(),
            receiver,
            TargetDeliveryWorkerOptions {
                poll_interval: Duration::from_millis(250),
                page_limit: PageLimit::new(32).expect("valid bound"),
                durable: DeliveryRetryPolicy {
                    completion: if selection.catalog.kind.as_str()
                        == uob_mqtt_target_adapter::MQTT_TARGET_KIND
                    {
                        uob_application::DeliverySemantic::NamedPeerAcknowledgement
                    } else {
                        uob_application::DeliverySemantic::LocalExposure
                    },
                    initial_backoff: Duration::from_secs(1),
                    maximum_backoff: Duration::from_secs(30),
                },
                replaceable_latest_state: DeliveryRetryPolicy {
                    completion: uob_application::DeliverySemantic::LocalExposure,
                    initial_backoff: Duration::from_secs(1),
                    maximum_backoff: Duration::from_secs(30),
                },
            },
        )
        .map_err(io::Error::other)?;
        Ok(Self {
            session,
            worker,
            ingress,
            store: state.store.clone(),
            roster: state.roster.clone(),
        })
    }

    pub(crate) async fn run(
        self,
        stop: oneshot::Receiver<()>,
        deadline: Duration,
    ) -> io::Result<()> {
        let Self {
            session,
            worker,
            ingress,
            store,
            roster,
        } = self;
        let mut session = Some(session);
        let mut worker = Some(worker);
        let mut current = BTreeMap::new();
        let mut scans = 0_u64;
        let mut interval = tokio::time::interval(Duration::from_secs(1));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        tokio::pin!(stop);
        let outcome = loop {
            tokio::select! {
                biased;
                _ = &mut stop => break Ok(()),
                _ = interval.tick() => {
                    if session.as_ref().is_some_and(uob_target_adapter::TargetSessionTask::is_finished) {
                        break session.take().expect("session exists").wait().await.map_err(io::Error::other);
                    }
                    if worker.as_ref().is_some_and(uob_target_adapter::TargetDeliveryWorkerTask::is_finished) {
                        break Err(io::Error::other("target delivery worker stopped"));
                    }
                    scans = scans.wrapping_add(1);
                    if let Err(error) = publish_snapshots(&store, &roster, &ingress, &mut current, scans.is_multiple_of(10)).await {
                        break Err(error);
                    }
                }
            }
        };
        let session_result = if let Some(session) = session {
            session.shutdown(deadline).await
        } else {
            Ok(())
        };
        let worker_result = worker
            .take()
            .expect("worker exists")
            .shutdown_with_deadline(deadline)
            .await;
        outcome
            .and(session_result.map_err(io::Error::other))
            .and(worker_result.map_err(io::Error::other))
    }
}

async fn publish_snapshots(
    store: &crate::charging::ChargingStore,
    roster: &[ResourceRef],
    ingress: &TargetDeliveryIngress<Value>,
    current: &mut BTreeMap<uob_contracts::StationId, uob_contracts::StationSnapshot>,
    refresh: bool,
) -> io::Result<()> {
    let mut after = None;
    loop {
        let page = store
            .read_scoped_snapshots(
                SnapshotQuery {
                    after: after.take(),
                    limit: PageLimit::new(32).expect("valid bound"),
                },
                roster.to_vec(),
            )
            .await
            .map_err(io::Error::other)?;
        for snapshot in page.items {
            if !refresh && current.get(&snapshot.station.station_id) == Some(&snapshot) {
                continue;
            }
            let encoded_bytes = serde_json::to_vec(&snapshot)
                .map_err(io::Error::other)?
                .len();
            let delivery = TargetDelivery {
                delivery_id: DeliveryId::new(format!("snapshot/{}", uuid::Uuid::new_v4()))
                    .map_err(io::Error::other)?,
                target_instance_id: ingress.destination().target_instance_id.clone(),
                target_configuration_revision: ingress.destination().configuration_revision,
                station_ordering_key: snapshot.station.clone(),
                deadline: UtcTimestamp::new(
                    time::OffsetDateTime::now_utc() + time::Duration::minutes(1),
                ),
                class: TargetDeliveryClass::ReplaceableLatestState,
                message: Arc::new(TargetMessage::StationSnapshot(snapshot.clone())),
            };
            if ingress.try_deliver(delivery, encoded_bytes).is_ok() {
                current.insert(snapshot.station.station_id.clone(), snapshot);
            }
        }
        match page.next_cursor {
            Some(cursor) => after = Some(cursor),
            None => return Ok(()),
        }
    }
}
