use std::{io, net::SocketAddr, time::Duration};

use serde::Deserialize;
use tokio::sync::oneshot;
use uob_application::Application;
use uob_management_adapter::ManagementRouterOptions;

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

pub(crate) async fn serve(
    address: SocketAddr,
    application: Application,
    options: ManagementRouterOptions,
    deadline: Duration,
    deployment: Option<crate::deployment::DeploymentState>,
) -> io::Result<()> {
    // Install both handlers before opening ingress, including systemd's default SIGTERM.
    let signal = stop_signal()?;
    tokio::pin!(signal);
    let notifier = crate::watchdog::Notifier::from_environment()?;
    if notifier.enabled() {
        let storage = deployment.as_ref().ok_or_else(|| {
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
    let (stop, stopped) = oneshot::channel();
    let server = uob_management_adapter::serve_with_readiness(
        address,
        application,
        options,
        async move {
            let _ = stopped.await;
        },
        || notifier.send("READY=1\nSTATUS=Local storage and management initialized"),
    );
    tokio::pin!(server);
    let mut server_finished = false;
    let early_result = loop {
        let progress = async {
            if let Some(interval) = notifier.interval {
                let storage = deployment
                    .as_ref()
                    .ok_or_else(|| io::Error::other("watchdog storage missing"))?;
                crate::watchdog::progress(storage.probe_progress(), interval).await
            } else {
                std::future::pending::<io::Result<()>>().await
            }
        };
        tokio::select! {
            // Poll the actual service before emitting progress; no detached notifier task.
            biased;
            result = &mut server => {
                server_finished = true;
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
        }
    };
    let _ = notifier.send("STOPPING=1\nSTATUS=Draining local service");
    let started = std::time::Instant::now();
    let _ = stop.send(());
    let drain = if server_finished {
        Ok(())
    } else {
        match tokio::time::timeout(deadline, server).await {
            Ok(result) => result,
            Err(_) => Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "shutdown deadline exceeded",
            )),
        }
    };
    let result = early_result.unwrap_or(Ok(())).and(drain);
    if let Some(deployment) = deployment {
        deployment
            .shutdown(deadline.saturating_sub(started.elapsed()))
            .await
            .map_err(io::Error::other)?;
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
