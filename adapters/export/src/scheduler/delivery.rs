use uob_application::{
    DatabaseHealthState, DatabaseRetryClassification, ExportSpool, ExportSpoolNamespace,
};
use uob_contracts::{ExportBatch, ExportOutcome};

use crate::ValidatedProviderSelection;

use super::{
    ExportSchedulerError, SharedHealth,
    ports::{Attempt, AttemptResult, StopSignal},
    spool_operation, update, validate_descriptor,
};

pub(super) enum DeliveryOutcome {
    Stopped,
    Committed,
    CommittedWithShutdownTimeout,
    Retry,
    PermanentFailure,
}

pub(super) async fn deliver(
    selection: &ValidatedProviderSelection,
    namespace: &ExportSpoolNamespace,
    spool: &dyn ExportSpool,
    batch: ExportBatch,
    encoded_bytes: usize,
    stop: &StopSignal,
    health: &SharedHealth,
) -> Result<DeliveryOutcome, ExportSchedulerError> {
    let provider = selection
        .create()
        .map_err(ExportSchedulerError::Configuration)?;
    let descriptor = provider.descriptor();
    validate_descriptor(&descriptor, selection)?;
    if encoded_bytes > descriptor.limits.maximum_batch_bytes {
        return Err(ExportSchedulerError::InvalidProvider(
            "batch.provider_byte_limit",
        ));
    }
    descriptor
        .validate_batch(&batch)
        .map_err(|_| ExportSchedulerError::InvalidProvider("batch.unsupported"))?;
    let attempt = Attempt::new(provider, descriptor, batch);
    update(health, |state| {
        state.provider.state = DatabaseHealthState::Starting;
        state.provider.batch_backlog = 1;
        state.provider.in_flight_batches = 1;
        state.provider.reason = None;
    });
    let result = attempt.run(stop).await;
    update(health, |state| state.provider.in_flight_batches = 0);
    match result {
        AttemptResult::Reported {
            report,
            shutdown_timed_out,
        } => {
            let committed = matches!(report.outcome(), ExportOutcome::Committed);
            // Retain the exact remote result across local contention; do not
            // re-enter the provider once it has confirmed this batch.
            let Some(settled) =
                spool_operation(stop, health, "spool.settle_timeout", false, || {
                    spool.settle_delivery(namespace.clone(), report.clone())
                })
                .await?
            else {
                return Ok(DeliveryOutcome::Stopped);
            };
            if committed {
                update(health, |state| {
                    state.last_confirmed_batch = settled.last_confirmed_batch;
                    state.confirmed_records = settled.confirmed_records;
                    state.provider.batch_backlog = usize::from(settled.pending_records != 0);
                    state.provider.state = if shutdown_timed_out {
                        DatabaseHealthState::Degraded
                    } else {
                        DatabaseHealthState::Ready
                    };
                    if shutdown_timed_out {
                        state.provider.reason = Some("provider.shutdown_timeout".into());
                    }
                });
                return Ok(if shutdown_timed_out {
                    DeliveryOutcome::CommittedWithShutdownTimeout
                } else {
                    DeliveryOutcome::Committed
                });
            }
            update(health, |state| {
                state.provider.reason = Some(if shutdown_timed_out {
                    "provider.shutdown_timeout".into()
                } else {
                    "provider.unconfirmed".into()
                });
                state.provider.state = DatabaseHealthState::Degraded;
            });
        }
        AttemptResult::Failed(error) => {
            let permanent = error.retry_classification() == DatabaseRetryClassification::Permanent;
            update(health, |state| {
                state.provider.reason = Some(format!("provider.{:?}", error.code()));
                state.provider.state = DatabaseHealthState::Degraded;
            });
            if permanent {
                return Ok(DeliveryOutcome::PermanentFailure);
            }
        }
        AttemptResult::TimedOut => {
            update(health, |state| {
                state.provider.reason = Some("provider.attempt_timeout".into());
                state.provider.state = DatabaseHealthState::Degraded;
            });
        }
        AttemptResult::Stopped => return Ok(DeliveryOutcome::Stopped),
        AttemptResult::ShutdownTimedOut => {
            update(health, |state| {
                state.provider.reason = Some("provider.shutdown_timeout".into());
                state.provider.state = DatabaseHealthState::Degraded;
            });
        }
    }
    Ok(DeliveryOutcome::Retry)
}
