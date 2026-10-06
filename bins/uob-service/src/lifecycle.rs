use std::{future::Future, io, net::SocketAddr, pin::Pin, sync::Arc, time::Duration};

use crate::{
    charging::{ChargingRuntime, ChargingStore},
    deployment::DeploymentState,
    export_runtime::ExportRuntime,
    export_runtime_drain::{DrainState, drain},
    management_auth::ManagementEventAuthenticator,
    management_source::ManagementSource,
    target_runtime::TargetRuntime,
    watchdog::Notifier,
};
use serde::Deserialize;
use tokio::{sync::oneshot, task::JoinHandle};
use uob_application::{Application, OperationalStore};
use uob_contracts::{TargetInstanceId, UtcTimestamp};
use uob_management_adapter::{
    ManagementCommandConfiguration, ManagementEventConfiguration, ManagementEventLimits,
    ManagementReadLimits, ManagementRouterOptions,
};
use uob_target_adapter::ValidatedTargetSelection;

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
    pub target_selection: Option<ValidatedTargetSelection<serde_json::Value, serde_json::Value>>,
    pub exporter: Option<ExportRuntime>,
    pub configuration_api: crate::configuration_api::Validated,
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
    configuration: Option<uob_management_adapter::ManagementConfigurationSetup>,
}

pub(crate) async fn serve(application: Application, settings: ServeSettings) -> io::Result<()> {
    serve_until(application, settings, stop_signal()?).await
}

pub(crate) async fn serve_until(
    application: Application,
    settings: ServeSettings,
    signal: impl Future<Output = ()>,
) -> io::Result<()> {
    let ServeSettings {
        address,
        diagnostics,
        release_read,
        options,
        deadline,
        deployment,
        charging,
        target_selection,
        exporter,
        configuration_api,
    } = settings;
    tokio::pin!(signal);
    let notifier = Notifier::from_environment()?;
    let charging_store = charging.as_ref().map(|runtime| runtime.state.store.clone());
    let charging_enabled = charging.is_some();
    let management = charging_management(&application, charging.as_ref())?;
    probe_startup(&notifier, deployment.as_ref(), charging_store.as_ref()).await?;
    let configuration = configuration_api
        .start(&application, target_selection.as_ref(), charging.as_ref())
        .await?;
    let target = start_target(target_selection.as_ref(), charging.as_ref(), &application)?;
    let (target_stop, target_stopped) = oneshot::channel();
    let mut target_task = target.map(|runtime| tokio::spawn(runtime.run(target_stopped, deadline)));
    let (stop, stopped) = oneshot::channel();
    let (charge_stop, charge_stopped) = oneshot::channel();
    let mut exporter = exporter;
    let export_application = application.clone();
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
            configuration,
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
        &mut target_task,
        signal.as_mut(),
        SupervisionInputs {
            notifier: &notifier,
            deployment: deployment.as_ref(),
            charging_store: charging_store.as_ref(),
            charging_enabled,
            exporter: exporter.as_mut(),
            application: &export_application,
        },
    )
    .await;
    let _ = notifier.send("STOPPING=1\nSTATUS=Draining local service");
    drain(
        server.as_mut(),
        charging_server.as_mut(),
        DrainState {
            stop,
            target_stop: target_task.as_ref().map(|_| target_stop),
            target_task,
            charge_stop,
            early_result,
            management_finished,
            charging_enabled,
            charging_finished,
            deadline,
            charging_store,
            deployment,
            exporter,
            export_application,
        },
    )
    .await
}

fn start_target(
    selection: Option<&ValidatedTargetSelection<serde_json::Value, serde_json::Value>>,
    charging: Option<&ChargingRuntime>,
    application: &Application,
) -> io::Result<Option<TargetRuntime>> {
    match (selection, charging) {
        (Some(selection), Some(charging)) => Ok(Some(TargetRuntime::start(
            selection,
            &charging.state,
            application,
        )?)),
        (None, _) => Ok(None),
        (Some(_), None) => Err(io::Error::other("selected target requires charging")),
    }
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
        configuration,
    } = listener;
    let shutdown = async move {
        let _ = stopped.await;
    };
    let ready = || notifier.send("READY=1\nSTATUS=Local storage and management initialized");
    let identity = application.identity().clone();
    let mut router = match charging {
        Some(ChargingManagement {
            source,
            events,
            commands: Some(commands),
        }) => uob_management_adapter::router_with_commands_and_authenticated_events(
            application,
            source,
            ManagementReadLimits::default(),
            commands,
            events,
            options,
        ),
        Some(ChargingManagement {
            source,
            events,
            commands: None,
        }) => uob_management_adapter::router_with_authenticated_events(
            application,
            source,
            ManagementReadLimits::default(),
            events,
            options,
        ),
        None => uob_management_adapter::router_with_options(application, options),
    };
    if let Some(configuration) = configuration {
        router = router.merge(uob_management_adapter::configuration_router(
            identity.clone(),
            configuration,
        ));
    }
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

struct SupervisionInputs<'a> {
    notifier: &'a Notifier,
    deployment: Option<&'a DeploymentState>,
    charging_store: Option<&'a ChargingStore>,
    charging_enabled: bool,
    exporter: Option<&'a mut ExportRuntime>,
    application: &'a Application,
}

async fn supervise(
    mut server: Pin<&mut impl Future<Output = io::Result<()>>>,
    mut charging_server: Pin<&mut impl Future<Output = io::Result<()>>>,
    target_task: &mut Option<JoinHandle<io::Result<()>>>,
    mut signal: Pin<&mut impl Future<Output = ()>>,
    inputs: SupervisionInputs<'_>,
) -> SupervisionResult {
    let SupervisionInputs {
        notifier,
        deployment,
        charging_store,
        charging_enabled,
        exporter,
        application,
    } = inputs;
    let mut management_finished = false;
    // Retention runs on the charging store's bounded SQLite worker, not a detached task.
    // A failed maintenance request stops supervision instead of silently filling the quota.
    let period = Duration::from_secs(60);
    let mut maintenance = tokio::time::interval_at(tokio::time::Instant::now() + period, period);
    let mut charging_finished = false;
    let mut exporter = exporter;
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
            result = async {
                match target_task.as_mut() {
                    Some(task) => task.await.map_err(io::Error::other).and_then(|result| result),
                    None => std::future::pending().await,
                }
            } => {
                *target_task = None;
                break Some(result.and(Err(io::Error::other("selected target stopped"))));
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
            () = async {
                match exporter.as_deref_mut() {
                    Some(runtime) => runtime.observe(application).await,
                    None => std::future::pending().await,
                }
            } => {},
        }
    };
    SupervisionResult {
        early_result,
        management_finished,
        charging_finished,
    }
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
#[path = "export_runtime_tests.rs"]
mod tests;
