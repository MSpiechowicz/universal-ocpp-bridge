//! Maintenance admission policy consumed by the production activation coordinator.
use std::{
    future::Future,
    pin::Pin,
    sync::Arc,
    time::{Duration, Instant},
};
use uob_application::{
    StorageError, StorageErrorCode, StorageFuture,
    release_drain::{DrainId, DrainObservation, ReleaseDrainPort},
};

/// Trusted service-control adapter. Stop the complete staging slice and confirm it is empty.
/// An error, timeout, or merely queued stop request must never count as stopped.
pub trait StagingStopPort: Send + Sync {
    fn stop_and_confirm(
        &self,
    ) -> Pin<Box<dyn Future<Output = Result<(), StorageError>> + Send + '_>>;
}

/// Process-local frozen boundary. This is neither qualification nor promotion authorization.
/// The activation owner must validate it immediately before stopping production and
/// bound that stop by `remaining()`. Never serialize it, restore it after restart, or switch
/// artifacts on an expired boundary. Dropping it defers promotion and reopens admissions.
pub struct IdleBoundary {
    port: Arc<dyn ReleaseDrainPort>,
    window: DrainId,
    deadline: Instant,
    observation: Option<DrainObservation>,
}
impl IdleBoundary {
    #[must_use]
    pub fn remaining(&self) -> Duration {
        self.deadline.saturating_duration_since(Instant::now())
    }

    /// # Errors
    /// Rejects a cancelled/expired window, busy storage, or changed durable inventory.
    pub async fn validate(&self) -> Result<(), StorageError> {
        if self.remaining().is_zero() {
            return Err(expired());
        }
        self.port
            .seal_drain(self.observation.clone().ok_or_else(expired)?)
            .await
    }

    /// Explicitly defers promotion; existing work is preserved.
    #[must_use]
    pub fn cancel(&self) -> StorageFuture<'_, ()> {
        self.port.cancel_drain(self.window.clone())
    }
}
impl Drop for IdleBoundary {
    fn drop(&mut self) {
        // The port queues cancellation synchronously; completion is best-effort here.
        // A full/stopped queue still leaves the independently enforced worker deadline.
        drop(self.port.cancel_drain(self.window.clone()));
    }
}

/// Waits for durable idle, stops staging, then rechecks the exact idle revision and freezes
/// operational writes on the same worker. A late transaction/job defers this attempt.
/// No charger stop command, artifact switch, or database restoration occurs here.
///
/// # Errors
/// Rejects invalid windows, competing owners, late state, staging failures, and deadlines.
/// Cancellation of this future leaves a worker-enforced deadline, never permanent drain.
pub async fn wait_for_idle_boundary(
    port: Arc<dyn ReleaseDrainPort>,
    staging: &dyn StagingStopPort,
    maintenance_window: Duration,
) -> Result<IdleBoundary, StorageError> {
    if maintenance_window.is_zero() || maintenance_window > Duration::from_hours(24) {
        return Err(StorageError::new(
            StorageErrorCode::InvalidRequest,
            "invalid maintenance window",
        ));
    }
    let deadline = Instant::now() + maintenance_window;
    let window = tokio::time::timeout(maintenance_window, port.begin_drain(maintenance_window))
        .await
        .map_err(|_| expired())??;
    let mut boundary = IdleBoundary {
        port,
        window,
        deadline,
        observation: None,
    };
    let wait = async {
        loop {
            let observed = boundary.port.observe_drain(boundary.window.clone()).await?;
            if observed.is_idle() {
                staging.stop_and_confirm().await?;
                boundary.port.seal_drain(observed.clone()).await?;
                return Ok::<_, StorageError>(observed);
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    };
    boundary.observation = Some(
        tokio::time::timeout(boundary.remaining(), wait)
            .await
            .map_err(|_| expired())??,
    );
    Ok(boundary)
}
fn expired() -> StorageError {
    StorageError::new(
        StorageErrorCode::Busy,
        "maintenance deadline expired; promotion deferred",
    )
}
