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
) -> io::Result<()> {
    // Install both handlers before opening ingress, including systemd's default SIGTERM.
    let signal = stop_signal()?;
    let (stop, stopped) = oneshot::channel();
    let server =
        uob_management_adapter::serve_with_shutdown(address, application, options, async move {
            let _ = stopped.await;
        });
    tokio::pin!(server);
    tokio::select! {
        result = &mut server => return result,
        () = signal => {}
    }
    let _ = stop.send(());
    match tokio::time::timeout(deadline, server).await {
        Ok(result) => result,
        Err(_) => Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "shutdown deadline exceeded",
        )),
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
