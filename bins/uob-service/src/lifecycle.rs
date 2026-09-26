use std::{future::Future, io, net::SocketAddr, pin::Pin, sync::Arc, time::Duration};

use serde::Deserialize;
use tokio::sync::oneshot;
use uob_application::{Application, OperationalStore};
use uob_contracts::{TargetInstanceId, UtcTimestamp};
use uob_management_adapter::{
    ManagementCommandConfiguration, ManagementEventConfiguration, ManagementEventLimits,
    ManagementReadLimits, ManagementRouterOptions,
};

use crate::{
    charging::{ChargingRuntime, ChargingStore},
    deployment::DeploymentState,
    management_auth::ManagementEventAuthenticator,
    management_source::ManagementSource,
    watchdog::Notifier,
};

#[derive(Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct LifecycleConfiguration {
    shutdown_timeout_seconds: u64,
}

impl Default for LifecycleConfiguration {
    fn default() -> Self {
        Self {
            shutdown_timeout_seconds: 20,
        }
    }
}

impl LifecycleConfiguration {
    pub(crate) fn validate(&self) -> Option<Duration> {
        (1..=300)
            .contains(&self.shutdown_timeout_seconds)
            .then(|| Duration::from_secs(self.shutdown_timeout_seconds))
    }
}

pub(crate) struct ServeSettings {
    pub address: SocketAddr,
    pub diagnostics: uob_management_adapter::ManagementCaptureConfiguration,
    pub release_read: Option<uob_management_adapter::ManagementReleaseReadConfiguration>,
    pub options: ManagementRouterOptions,
    pub deadline: Duration,
    pub deployment: Option<DeploymentState>,
    pub charging: Option<ChargingRuntime>,
}

struct ChargingManagement {
    source: Arc<ManagementSource>,
    events: ManagementEventConfiguration,
    commands: Option<ManagementCommandConfiguration>,
}

struct ManagementListener {
    address: SocketAddr,
    diagnostics: uob_management_adapter::ManagementCaptureConfiguration,
    release_read: Option<uob_management_adapter::ManagementReleaseReadConfiguration>,
    options: ManagementRouterOptions,
    charging: Option<ChargingManagement>,
}

pub(crate) async fn serve(application: Application, settings: ServeSettings) -> io::Result<()> {
    let ServeSettings {
        address,
        diagnostics,
        release_read,
        options,
        deadline,
        deployment,
        charging,
    } = settings;
    let signal = stop_signal()?;
    tokio::pin!(signal);
    let notifier = Notifier::from_environment()?;
    let charging_store = charging.as_ref().map(|runtime| runtime.state.store.clone());
    let charging_enabled = charging.is_some();
    let management = charging_management(&application, charging.as_ref())?;
    probe_startup(&notifier, deployment.as_ref(), charging_store.as_ref()).await?;
    let (stop, stopped) = oneshot::channel();
    let (charge_stop, charge_stopped) = oneshot::channel();
    let charging_application = application.clone();
    let charging_server = async move {
        match charging {
            Some(runtime) => {
                runtime
                    .serve(charging_application, async {
                        let _ = charge_stopped.await;
                    })
                    .await
            }
            None => std::future::pending::<io::Result<()>>().await,
        }
    };
    tokio::pin!(charging_server);
    let server = serve_management(
        application,
        ManagementListener {
            address,
            diagnostics,
            release_read,
            options,
            charging: management,
        },
        stopped,
        &notifier,
    );
    tokio::pin!(server);
    let SupervisionResult {
        early_result,
        management_finished,
        charging_finished,
    } = supervise(
        server.as_mut(),
        charging_server.as_mut(),
        signal.as_mut(),
        &notifier,
        deployment.as_ref(),
        charging_store.as_ref(),
        charging_enabled,
    )
    .await;
    let _ = notifier.send("STOPPING=1\nSTATUS=Draining local service");
    drain(
        server.as_mut(),
        charging_server.as_mut(),
        DrainState {
            stop,
            charge_stop,
            early_result,
            management_finished,
            charging_enabled,
            charging_finished,
            deadline,
            charging_store,
            deployment,
        },
    )
    .await
}

async fn serve_management(
    application: Application,
    listener: ManagementListener,
    stopped: oneshot::Receiver<()>,
    notifier: &Notifier,
) -> io::Result<()> {
    let ManagementListener {
        address,
        diagnostics,
        release_read,
        options,
        charging,
    } = listener;
    let shutdown = async move {
        let _ = stopped.await;
    };
    let ready = || notifier.send("READY=1\nSTATUS=Local storage and management initialized");
    if let Some(ChargingManagement {
        source,
        events,
        commands,
    }) = charging
    {
        if let Some(commands) = commands {
            let identity = application.identity().clone();
            let router = uob_management_adapter::router_with_commands_and_authenticated_events(
                application,
                source,
                ManagementReadLimits::default(),
                commands,
                events,
                options,
            );
            uob_management_adapter::serve_router_with_capture_and_release_readiness(
                address,
                identity,
                router,
                Some(diagnostics),
                release_read,
                shutdown,
                ready,
            )
            .await
        } else {
            uob_management_adapter::serve_with_authenticated_events_and_capture_and_release_readiness(
                address, application, source, ManagementReadLimits::default(), events,
                options, Some(diagnostics), release_read, shutdown, ready,
            ).await
        }
    } else {
        uob_management_adapter::serve_with_capture_and_release_readiness(
            address,
            application,
            options,
            Some(diagnostics),
            release_read,
            shutdown,
            ready,
        )
        .await
    }
}

fn charging_management(
    application: &Application,
    charging: Option<&ChargingRuntime>,
) -> io::Result<Option<ChargingManagement>> {
    charging
        .map(|runtime| {
            let roster = &runtime.state.roster;
            let default_resource = roster
                .first()
                .cloned()
                .ok_or_else(|| io::Error::other("charging management roster unavailable"))?;
            // Charging reads belong to the local demo view, never an outbound target selection.
            let owner = TargetInstanceId::new("management-demo").map_err(io::Error::other)?;
            let authenticator = ManagementEventAuthenticator::new(
                application.runtime_identity().environment,
                application.identity().bridge_id.clone(),
                owner,
                roster.clone(),
                default_resource,
                runtime.state.read_grant.token(),
            )
            .map_err(|_| io::Error::other("charging management read grant unavailable"))?;
            Ok(ChargingManagement {
                source: Arc::new(ManagementSource::new(runtime.state.store.clone())),
                events: ManagementEventConfiguration {
                    authenticator: Arc::new(authenticator),
                    limits: ManagementEventLimits::default(),
                },
                commands: runtime.state.command_configuration(application)?,
            })
        })
        .transpose()
}

async fn probe_startup(
    notifier: &Notifier,
    deployment: Option<&DeploymentState>,
    charging_store: Option<&ChargingStore>,
) -> io::Result<()> {
    if notifier.enabled() {
        let storage = deployment.ok_or_else(|| {
            io::Error::other("notified service requires initialized deployment storage")
        })?;
        tokio::time::timeout(Duration::from_secs(5), storage.probe_progress())
            .await
            .map_err(|_| {
                io::Error::new(
                    io::ErrorKind::TimedOut,
                    "storage initialization probe stalled",
                )
            })??;
    }
    if let Some(store) = charging_store {
        tokio::time::timeout(Duration::from_secs(5), store.probe_progress())
            .await
            .map_err(|_| {
                io::Error::new(
                    io::ErrorKind::TimedOut,
                    "charging storage initialization stalled",
                )
            })?
            .map_err(io::Error::other)?;
        tokio::time::timeout(
            Duration::from_secs(30),
            store.maintain_storage_retention(UtcTimestamp::new(time::OffsetDateTime::now_utc())),
        )
        .await
        .map_err(|_| {
            io::Error::new(
                io::ErrorKind::TimedOut,
                "charging retention startup stalled",
            )
        })?
        .map_err(io::Error::other)?;
    }
    Ok(())
}

async fn watchdog_progress(
    notifier: &Notifier,
    deployment: Option<&DeploymentState>,
    charging_store: Option<&ChargingStore>,
) -> io::Result<()> {
    if let Some(interval) = notifier.interval {
        if let Some(storage) = deployment {
            crate::watchdog::progress(storage.probe_progress(), interval).await?;
        }
        if let Some(store) = charging_store {
            crate::watchdog::progress(
                async { store.probe_progress().await.map_err(io::Error::other) },
                interval,
            )
            .await?;
        }
        Ok(())
    } else {
        std::future::pending::<io::Result<()>>().await
    }
}

struct SupervisionResult {
    early_result: Option<io::Result<()>>,
    management_finished: bool,
    charging_finished: bool,
}

async fn supervise(
    mut server: Pin<&mut impl Future<Output = io::Result<()>>>,
    mut charging_server: Pin<&mut impl Future<Output = io::Result<()>>>,
    mut signal: Pin<&mut impl Future<Output = ()>>,
    notifier: &Notifier,
    deployment: Option<&DeploymentState>,
    charging_store: Option<&ChargingStore>,
    charging_enabled: bool,
) -> SupervisionResult {
    let mut management_finished = false;
    // Retention runs on the charging store's bounded SQLite worker, not a detached task.
    // A failed maintenance request stops supervision instead of silently filling the quota.
    let period = Duration::from_secs(60);
    let mut maintenance = tokio::time::interval_at(tokio::time::Instant::now() + period, period);
    let mut charging_finished = false;
    let early_result = loop {
        let progress = watchdog_progress(notifier, deployment, charging_store);
        let maintain = async {
            maintenance.tick().await;
            if let Some(store) = charging_store {
                store
                    .maintain_storage_retention(UtcTimestamp::new(time::OffsetDateTime::now_utc()))
                    .await
                    .map_err(io::Error::other)?;
            }
            Ok::<(), io::Error>(())
        };
        tokio::select! {
            biased;
            result = &mut server => {
                management_finished = true;
                break Some(result);
            },
            result = &mut charging_server, if charging_enabled => {
                charging_finished = true;
                break Some(result);
            },
            () = &mut signal => break None,
            result = progress => {
                if let Err(error) = result {
                    let _ = notifier.send("STATUS=Storage progress failed");
                    break Some(Err(error));
                }
                if let Err(error) = notifier.send("WATCHDOG=1") {
                    break Some(Err(error));
                }
            }
            result = maintain, if charging_store.is_some() => {
                if let Err(error) = result {
                    let _ = notifier.send("STATUS=Charging storage retention failed");
                    break Some(Err(error));
                }
            },
        }
    };
    SupervisionResult {
        early_result,
        management_finished,
        charging_finished,
    }
}

struct DrainState {
    stop: oneshot::Sender<()>,
    charge_stop: oneshot::Sender<()>,
    early_result: Option<io::Result<()>>,
    management_finished: bool,
    charging_enabled: bool,
    charging_finished: bool,
    deadline: Duration,
    charging_store: Option<ChargingStore>,
    deployment: Option<DeploymentState>,
}

async fn drain(
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
        deployment,
    } = state;
    let started = std::time::Instant::now();
    let _ = stop.send(());
    let _ = charge_stop.send(());
    let management_result = if management_finished {
        Ok(())
    } else {
        tokio::time::timeout(deadline, &mut server)
            .await
            .unwrap_or_else(|_| {
                Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "management shutdown deadline exceeded",
                ))
            })
    };
    let charging_result = if !charging_enabled || charging_finished {
        Ok(())
    } else {
        tokio::time::timeout(
            deadline.saturating_sub(started.elapsed()),
            &mut charging_server,
        )
        .await
        .unwrap_or_else(|_| {
            Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "charging shutdown deadline exceeded",
            ))
        })
    };
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

#[cfg(unix)]
fn stop_signal() -> io::Result<impl Future<Output = ()>> {
    use tokio::signal::unix::{SignalKind, signal};
    let mut terminate = signal(SignalKind::terminate())?;
    let mut interrupt = signal(SignalKind::interrupt())?;
    Ok(async move {
        tokio::select! {
            _ = terminate.recv() => {},
            _ = interrupt.recv() => {},
        }
    })
}

#[cfg(not(unix))]
fn stop_signal() -> io::Result<impl Future<Output = ()>> {
    Ok(async {
        let _ = tokio::signal::ctrl_c().await;
    })
}

#[cfg(test)]
mod tests {
    use super::LifecycleConfiguration;

    #[test]
    fn shutdown_deadlines_are_finite_and_validated_offline() {
        for seconds in [0, 301, u64::MAX] {
            assert!(
                LifecycleConfiguration {
                    shutdown_timeout_seconds: seconds
                }
                .validate()
                .is_none()
            );
        }
        assert_eq!(
            LifecycleConfiguration::default()
                .validate()
                .unwrap()
                .as_secs(),
            20
        );
    }
}
