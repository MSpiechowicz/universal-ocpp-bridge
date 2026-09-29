use std::{future::Future, io, pin::Pin, time::Duration};

use tokio::sync::oneshot;
use uob_application::Application;

use crate::{charging::ChargingStore, deployment::DeploymentState, export_runtime::ExportRuntime};

pub(crate) struct DrainState {
    pub stop: oneshot::Sender<()>,
    pub charge_stop: oneshot::Sender<()>,
    pub early_result: Option<io::Result<()>>,
    pub management_finished: bool,
    pub charging_enabled: bool,
    pub charging_finished: bool,
    pub deadline: Duration,
    pub charging_store: Option<ChargingStore>,
    pub deployment: Option<DeploymentState>,
    pub exporter: Option<ExportRuntime>,
    pub export_application: Application,
}

pub(crate) async fn drain(
    mut server: Pin<&mut impl Future<Output = io::Result<()>>>,
    mut charging_server: Pin<&mut impl Future<Output = io::Result<()>>>,
    state: DrainState,
) -> io::Result<()> {
    let DrainState {
        stop,
        charge_stop,
        early_result,
        management_finished,
        charging_enabled,
        charging_finished,
        deadline,
        charging_store,
        exporter,
        export_application,
        deployment,
    } = state;
    let started = tokio::time::Instant::now();
    let end = started + deadline;
    // The exporter is optional; reserve the latter half for local store and
    // deployment drains, which begin after the listeners have stopped.
    let export_end = started + deadline / 2;
    let _ = stop.send(());
    let _ = charge_stop.send(());

    // Listeners and exporter start draining together, but only local work owns the full deadline.
    let management = async {
        if management_finished {
            Ok(())
        } else {
            tokio::time::timeout_at(end, &mut server)
                .await
                .unwrap_or_else(|_| {
                    Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "management shutdown deadline exceeded",
                    ))
                })
        }
    };
    let charging = async {
        if !charging_enabled || charging_finished {
            Ok(())
        } else {
            tokio::time::timeout_at(end, &mut charging_server)
                .await
                .unwrap_or_else(|_| {
                    Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "charging shutdown deadline exceeded",
                    ))
                })
        }
    };
    let export = async {
        if let Some(runtime) = exporter {
            runtime.shutdown(&export_application, export_end).await;
        }
    };
    let (management_result, charging_result, ()) = tokio::join!(management, charging, export);
    let mut result = early_result
        .unwrap_or(Ok(()))
        .and(management_result)
        .and(charging_result);

    if let Some(store) = charging_store {
        result = result.and(
            store
                .shutdown(deadline.saturating_sub(started.elapsed()))
                .await
                .map_err(io::Error::other),
        );
    }
    if let Some(deployment) = deployment {
        result = result.and(
            deployment
                .shutdown(deadline.saturating_sub(started.elapsed()))
                .await
                .map_err(io::Error::other),
        );
    }
    result
}
