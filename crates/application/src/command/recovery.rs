use std::collections::BTreeMap;

use uob_contracts::{CommandLifecycle, CommandResult, ObservedCommandEffect, RequestId};

use super::{
    CommandCoordinator, CommandRecoveryBatch, RecoveredCommand, command_result, map_storage_error,
};
use crate::{CommandAdmissionFuture, FlowEvidence, FlowStage, PageLimit, RecoveryQuery};

impl<P, E, D, R> CommandCoordinator<P, E, D, R>
where
    P: Clone + PartialEq + Send + Sync + 'static,
    E: Send + 'static,
    D: Send + 'static,
    R: Send + 'static,
{
    /// Restores unresolved command state without automatically dispatching recovered work.
    ///
    /// A persisted `Dispatched` state is conservatively converted to `TransmissionUncertain`
    /// because the process cannot prove whether the station acted before restart.
    #[must_use]
    pub fn recover_unresolved(
        &self,
        limit: PageLimit,
    ) -> CommandAdmissionFuture<'_, CommandRecoveryBatch<P>> {
        Box::pin(async move {
            let now = self.clock.now();
            let recovery = self
                .store
                .recover(RecoveryQuery { limit })
                .await
                .map_err(|error| map_storage_error(&error))?;
            let mut results = recovery
                .command_results
                .into_iter()
                .map(|result| (result.return_route.request_id.as_str().to_owned(), result))
                .collect::<BTreeMap<_, _>>();
            let mut commands = Vec::with_capacity(recovery.active_commands.len());
            for command in recovery.active_commands {
                let request_id = command.request_id.as_str();
                let mut result = results
                    .remove(request_id)
                    .unwrap_or_else(|| command_result(&command, CommandLifecycle::Admitted, now));
                if matches!(result.lifecycle, CommandLifecycle::Dispatched) {
                    result.lifecycle = CommandLifecycle::TransmissionUncertain {
                        detail: "service restarted before a charger response was recorded"
                            .to_owned(),
                    };
                    result.recorded_at = now;
                    self.persist_result(result.clone()).await?;
                }
                commands.push(RecoveredCommand { command, result });
            }
            Ok(CommandRecoveryBatch { commands })
        })
    }

    /// Links later observed state evidence without changing protocol acknowledgement state.
    #[must_use]
    pub fn reconcile_observed_effect(
        &self,
        request_id: RequestId,
        effect: ObservedCommandEffect,
    ) -> CommandAdmissionFuture<'_, Option<CommandResult>> {
        Box::pin(async move {
            let Some(command) = self
                .store
                .command_by_request_id(request_id.clone())
                .await
                .map_err(|error| map_storage_error(&error))?
            else {
                return Ok(None);
            };
            let mut result = self
                .store
                .command_result_by_request_id(request_id.clone())
                .await
                .map_err(|error| map_storage_error(&error))?
                .unwrap_or_else(|| {
                    command_result(&command, CommandLifecycle::Admitted, self.clock.now())
                });
            if !result
                .observed_effects
                .iter()
                .any(|existing| existing.event_id == effect.event_id)
            {
                let event_id = effect.event_id.clone();
                result.observed_effects.push(effect);
                self.persist_result(result.clone()).await?;
                self.diagnostics
                    .span(
                        result.correlation_id.clone(),
                        Some(result.resource.station_id.clone()),
                        None,
                    )
                    .with_request(command.request_id.clone())
                    .emit_fields(
                        FlowStage::ObservedEffect,
                        FlowEvidence::Observed,
                        vec![crate::SafeDiagnosticField::ObservedEvent(event_id)],
                    );
            }
            self.store
                .command_result_by_request_id(request_id)
                .await
                .map_err(|error| map_storage_error(&error))
        })
    }
}
