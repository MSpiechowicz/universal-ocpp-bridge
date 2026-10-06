//! Bridge-wide target configuration API: credentials, start-up resolution, and the host port.
//!
//! Configuration permissions are explicit management access-policy grants scoped to the whole
//! bridge. Apply persists a private next-start section; a running target is never replaced.
//! Archive/discard dispositions are authorized here and executed by the next start, before any
//! target session reads the outbox.
use std::{
    io,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use serde_json::Value;
use time::{OffsetDateTime, format_description::well_known::Rfc3339};
use uob_application::{
    Application, DeliveryDispositionAction, DeliveryDispositionRecord, DeliveryDispositionRequest,
    DeliveryDispositionState, StorageErrorCode, TargetBacklogFact, TargetDeliveryDestination,
    TargetDispositionStore,
};
use uob_contracts::{
    BridgeId, Environment, PrincipalId, ServiceIdentity, TargetInstanceId, UtcTimestamp,
};
use uob_management_adapter::{
    ConfigurationApiError, ConfigurationFuture, DispositionAction, ManagementConfigurationPort,
    ManagementConfigurationSetup, TargetApplyOutcome, TargetApplyRequest, TargetCatalogView,
    TargetConfigurationCandidate, TargetConfigurationView, TargetDispositionRequest,
    TargetDispositionView, TargetValidationReport,
};
use uob_target_adapter::ValidatedTargetSelection;

use crate::{
    charging::ChargingRuntime,
    configuration::targets::{self, DurableFacts, Sections},
};

mod credentials;
pub(crate) use credentials::{Configuration, Validated};

const SETTLEMENT_DEADLINE: Duration = Duration::from_secs(30);

impl Validated {
    /// Settles audited dispositions, then builds the routes' host port when credentials exist.
    ///
    /// Call before any target session starts: executed dispositions then never race the
    /// delivery worker. Credential files are read here, once.
    pub(crate) async fn start(
        self,
        application: &Application,
        selection: Option<&ValidatedTargetSelection<Value, Value>>,
        charging: Option<&ChargingRuntime>,
    ) -> io::Result<Option<ManagementConfigurationSetup>> {
        let store = charging.map(|runtime| {
            Arc::new(runtime.state.store.clone()) as Arc<dyn TargetDispositionStore>
        });
        start(self, application.identity(), selection, store).await
    }
}

async fn start(
    configuration: Validated,
    identity: &ServiceIdentity,
    selection: Option<&ValidatedTargetSelection<Value, Value>>,
    store: Option<Arc<dyn TargetDispositionStore>>,
) -> io::Result<Option<ManagementConfigurationSetup>> {
    let running = selection.map(|selection| {
        let destination = selection.destination();
        TargetDeliveryDestination {
            target_instance_id: destination.target_instance_id,
            configuration_revision: destination.configuration_revision,
        }
    });
    if let Some(store) = &store {
        settle_dispositions(store.as_ref(), running.clone()).await?;
    }
    let (Some(authenticator), Some(configuration_path)) = (
        configuration.resolve()?,
        configuration.configuration_path().map(Path::to_path_buf),
    ) else {
        return Ok(None);
    };
    Ok(Some(ManagementConfigurationSetup {
        port: Arc::new(ConfigurationPort {
            configuration_path,
            bridge_id: identity.bridge_id.clone(),
            environment: identity.runtime.environment,
            running,
            store,
            apply: tokio::sync::Mutex::new(()),
        }),
        authenticator,
    }))
}

async fn settle_dispositions(
    store: &dyn TargetDispositionStore,
    selected: Option<TargetDeliveryDestination>,
) -> io::Result<()> {
    let settled = tokio::time::timeout(
        SETTLEMENT_DEADLINE,
        store.settle_target_dispositions(selected, now()),
    )
    .await
    .map_err(|_| {
        io::Error::new(
            io::ErrorKind::TimedOut,
            "target disposition settlement stalled",
        )
    })?
    .map_err(io::Error::other)?;
    for record in settled {
        eprintln!(
            "target disposition {} {} action={} destination={}@{} critical_deliveries={} deliveries={}",
            record.audit_event_id,
            state_name(record.state),
            action_name(record.action),
            record.destination.target_instance_id.as_str(),
            record.destination.configuration_revision,
            record.critical_deliveries.unwrap_or(0),
            record.deliveries.unwrap_or(0),
        );
    }
    Ok(())
}

struct ConfigurationPort {
    configuration_path: PathBuf,
    bridge_id: BridgeId,
    environment: Environment,
    running: Option<TargetDeliveryDestination>,
    store: Option<Arc<dyn TargetDispositionStore>>,
    /// Serializes read-compare-write of the staged section.
    apply: tokio::sync::Mutex<()>,
}

impl ConfigurationPort {
    fn sections(&self) -> Result<Sections, ConfigurationApiError> {
        Sections::read(&self.configuration_path).map_err(|error| match error {
            crate::configuration::ConfigurationLoadError::StagedTargetsConflict
            | crate::configuration::ConfigurationLoadError::InvalidStagedTargets
            | crate::configuration::ConfigurationLoadError::InvalidDocument => {
                ConfigurationApiError::Unreadable(error.code())
            }
            _ => ConfigurationApiError::Unavailable,
        })
    }

    async fn facts(
        &self,
    ) -> Result<(Vec<TargetBacklogFact>, Vec<DeliveryDispositionRecord>), ConfigurationApiError>
    {
        let Some(store) = &self.store else {
            return Ok((vec![], vec![]));
        };
        let backlog = store
            .target_delivery_backlog()
            .await
            .map_err(|_| ConfigurationApiError::Unavailable)?;
        let dispositions = store
            .pending_target_dispositions()
            .await
            .map_err(|_| ConfigurationApiError::Unavailable)?;
        Ok((backlog, dispositions))
    }

    async fn evaluate(
        &self,
        candidate: &TargetConfigurationCandidate,
    ) -> Result<targets::Evaluation, ConfigurationApiError> {
        let (backlog, dispositions) = self.facts().await?;
        let facts = DurableFacts {
            running: self.running.as_ref(),
            backlog: &backlog,
            dispositions: &dispositions,
        };
        Ok(targets::evaluate(self.sections()?, candidate, &facts))
    }
}

impl ManagementConfigurationPort for ConfigurationPort {
    fn catalog(&self) -> ConfigurationFuture<'_, TargetCatalogView> {
        Box::pin(async move {
            targets::catalog_view(&self.bridge_id, self.environment)
                .map_err(|_| ConfigurationApiError::Unavailable)
        })
    }

    fn current(&self) -> ConfigurationFuture<'_, TargetConfigurationView> {
        Box::pin(async move {
            let (backlog, dispositions) = self.facts().await?;
            let sections = self.sections()?;
            let next = sections.effective().destination();
            Ok(TargetConfigurationView {
                bridge_id: self.bridge_id.as_str().to_owned(),
                environment: self.environment,
                configuration_digest: sections.effective().digest(),
                running_destination: self.running.as_ref().map(targets::destination_view),
                next_destination: next.as_ref().map(targets::destination_view),
                restart_required: next.is_some() && self.running != next,
                next_start: sections.view(),
                backlog: backlog.iter().map(targets::backlog_view).collect(),
                dispositions: dispositions.iter().map(disposition_view).collect(),
            })
        })
    }

    fn validate(
        &self,
        candidate: TargetConfigurationCandidate,
    ) -> ConfigurationFuture<'_, TargetValidationReport> {
        Box::pin(async move { Ok(self.evaluate(&candidate).await?.report) })
    }

    fn apply(
        &self,
        request: TargetApplyRequest,
        actor: PrincipalId,
    ) -> ConfigurationFuture<'_, TargetApplyOutcome> {
        Box::pin(async move {
            let _serialized = self.apply.lock().await;
            let evaluation = self.evaluate(&request.configuration).await?;
            let Some(path) = evaluation.staged_path().map(Path::to_path_buf) else {
                return Err(ConfigurationApiError::ApplyUnavailable);
            };
            if evaluation.report.configuration_digest != request.expected_digest {
                return Err(ConfigurationApiError::Conflict);
            }
            let report = evaluation.report.clone();
            let Some(next_destination) = report.next_destination.clone().filter(|_| report.valid)
            else {
                return Err(ConfigurationApiError::Invalid(Box::new(report)));
            };
            let actor_name = actor.as_str().to_owned();
            let applied_at = format_timestamp(now());
            let digest = tokio::task::spawn_blocking(move || {
                evaluation.persist(&path, &actor_name, &applied_at)
            })
            .await
            .map_err(|_| ConfigurationApiError::Unavailable)?
            .map_err(|_| ConfigurationApiError::Unavailable)?
            .ok_or(ConfigurationApiError::Unavailable)?;
            eprintln!(
                "target configuration applied for next start by {} destination={}@{} restart_required={}",
                actor.as_str(),
                next_destination.target_id,
                next_destination.configuration_revision,
                report.restart_required,
            );
            Ok(TargetApplyOutcome {
                configuration_digest: digest,
                running_destination: report.running_destination,
                next_destination,
                restart_required: report.restart_required,
                dispositions: report.dispositions,
            })
        })
    }

    fn authorize_disposition(
        &self,
        request: TargetDispositionRequest,
        actor: PrincipalId,
    ) -> ConfigurationFuture<'_, TargetDispositionView> {
        Box::pin(async move {
            let not_required = ConfigurationApiError::DispositionRejected(
                "configuration.disposition_not_required",
            );
            // Without a charging store there is no outbox, so no destination has pending work.
            let Some(store) = &self.store else {
                return Err(not_required);
            };
            let Ok(target_instance_id) = TargetInstanceId::new(request.target_id) else {
                return Err(not_required);
            };
            let destination = TargetDeliveryDestination {
                target_instance_id,
                configuration_revision: request.configuration_revision,
            };
            let (backlog, dispositions) = self.facts().await?;
            if !backlog.iter().any(|fact| {
                fact.destination == destination && fact.pending_critical_deliveries != 0
            }) {
                return Err(not_required);
            }
            if dispositions
                .iter()
                .any(|record| record.destination == destination)
            {
                return Err(ConfigurationApiError::DispositionRejected(
                    "configuration.disposition_exists",
                ));
            }
            let record = store
                .authorize_target_disposition(DeliveryDispositionRequest {
                    destination,
                    action: match request.action {
                        DispositionAction::Archive => DeliveryDispositionAction::Archive,
                        DispositionAction::Discard => DeliveryDispositionAction::Discard,
                    },
                    principal_id: actor,
                    authorized_at: now(),
                })
                .await
                .map_err(|error| match error.code() {
                    StorageErrorCode::Conflict => ConfigurationApiError::DispositionRejected(
                        "configuration.disposition_conflict",
                    ),
                    StorageErrorCode::CapacityExhausted => {
                        ConfigurationApiError::DispositionRejected(
                            "configuration.disposition_limit",
                        )
                    }
                    _ => ConfigurationApiError::Unavailable,
                })?;
            eprintln!(
                "target disposition {} authorized by {} action={} destination={}@{}",
                record.audit_event_id,
                record.principal_id.as_str(),
                action_name(record.action),
                record.destination.target_instance_id.as_str(),
                record.destination.configuration_revision,
            );
            Ok(disposition_view(&record))
        })
    }
}

fn disposition_view(record: &DeliveryDispositionRecord) -> TargetDispositionView {
    TargetDispositionView {
        audit_event_id: record.audit_event_id.clone(),
        target_id: record.destination.target_instance_id.as_str().to_owned(),
        configuration_revision: record.destination.configuration_revision,
        action: match record.action {
            DeliveryDispositionAction::Archive => DispositionAction::Archive,
            DeliveryDispositionAction::Discard => DispositionAction::Discard,
        },
        state: state_name(record.state),
        authorized_by: record.principal_id.as_str().to_owned(),
        authorized_at: record.authorized_at,
    }
}

const fn action_name(action: DeliveryDispositionAction) -> &'static str {
    match action {
        DeliveryDispositionAction::Archive => "archive",
        DeliveryDispositionAction::Discard => "discard",
    }
}

const fn state_name(state: DeliveryDispositionState) -> &'static str {
    match state {
        DeliveryDispositionState::Authorized => "authorized",
        DeliveryDispositionState::Executed => "executed",
        DeliveryDispositionState::Superseded => "superseded",
    }
}

fn now() -> UtcTimestamp {
    UtcTimestamp::new(OffsetDateTime::now_utc())
}

fn format_timestamp(value: UtcTimestamp) -> String {
    value
        .into_inner()
        .format(&Rfc3339)
        .unwrap_or_else(|_| "unknown".to_owned())
}

#[cfg(test)]
#[path = "configuration_api_tests.rs"]
mod tests;
