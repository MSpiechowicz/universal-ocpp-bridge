//! Opt-in simulator control server; never linked into the production daemon.
mod configuration;
mod http;
mod runs;

pub use configuration::ControlConfiguration;

use crate::scenario::ScenarioRunner;
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{net::TcpListener, sync::Semaphore};

#[derive(Clone)]
pub struct ControlServer {
    configuration: Arc<ControlConfiguration>,
    runner: Arc<ScenarioRunner>,
    runs: Arc<Mutex<runs::Runs>>,
    requests: Arc<Semaphore>,
}

impl ControlServer {
    #[must_use]
    pub fn new(configuration: ControlConfiguration, runner: ScenarioRunner) -> Self {
        Self {
            configuration: Arc::new(configuration),
            runner: Arc::new(runner),
            runs: Arc::new(Mutex::new(runs::Runs {
                next_id: 1,
                entries: BTreeMap::new(),
                stopping: false,
            })),
            requests: Arc::new(Semaphore::new(16)),
        }
    }

    pub fn router(&self) -> axum::Router {
        http::router(self.clone())
    }

    /// Stops all tracked runs and waits for their bounded socket cleanup.
    ///
    /// # Panics
    /// Panics if an internal worker poisoned the run registry.
    pub async fn shutdown(&self) {
        let tasks: Vec<_> = {
            let mut runs = self.runs.lock().expect("runs lock");
            runs.stopping = true;
            runs.entries
                .values_mut()
                .filter_map(|run| {
                    run.stop.cancel();
                    run.task.take()
                })
                .collect()
        };
        for task in tasks {
            let _ = task.await;
        }
    }

    /// Binds only the validated loopback address and shuts down on Ctrl-C.
    ///
    /// # Errors
    /// Returns a safe startup/serving diagnostic.
    pub async fn serve(self) -> Result<(), &'static str> {
        let listener = TcpListener::bind(self.configuration.bind)
            .await
            .map_err(|_| "control_bind_failed")?;
        let shutdown = self.clone();
        let result = axum::serve(listener, self.router())
            .with_graceful_shutdown(async move {
                let _ = tokio::signal::ctrl_c().await;
                shutdown.shutdown().await;
            })
            .await;
        self.shutdown().await;
        result.map_err(|_| "control_server_failed")
    }
}

/// Parses the deliberately separate `serve` entry point.
///
/// # Errors
/// Rejects missing, duplicate, unknown or invalid arguments and unsafe configuration.
pub async fn serve_arguments(
    arguments: impl IntoIterator<Item = String>,
) -> Result<(), &'static str> {
    let mut arguments = arguments.into_iter();
    if arguments.next().as_deref() != Some("serve") {
        return Err("invalid_serve_arguments");
    }
    let mut configuration = None;
    let mut bind = None;
    while let Some(argument) = arguments.next() {
        let value = arguments.next().ok_or("invalid_serve_arguments")?;
        match argument.as_str() {
            "--config" if configuration.is_none() => configuration = Some(value),
            "--control-bind" if bind.is_none() => {
                bind = Some(value.parse().map_err(|_| "invalid_control_bind")?);
            }
            _ => return Err("invalid_serve_arguments"),
        }
    }
    let configuration = configuration.ok_or("explicit_control_configuration_required")?;
    let configuration = ControlConfiguration::load(
        std::path::Path::new(&configuration),
        bind.ok_or("explicit_control_bind_required")?,
    )?;
    ControlServer::new(configuration, ScenarioRunner::default())
        .serve()
        .await
}

const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);
