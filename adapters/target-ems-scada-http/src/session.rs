use std::{
    future::Future,
    net::SocketAddr,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};

use time::OffsetDateTime;
use tokio::{net::TcpListener, sync::oneshot, task::JoinHandle};
use uob_application::{
    CredentialReference, DeliveryId, DeliveryOutcome, DeliveryReport, ErrorRetryClassification,
    TargetContext, TargetDelivery, TargetDeliveryReceiver, TargetDiagnostic, TargetError,
    TargetErrorCode, TargetHealth, TargetHealthState, TargetReportPort, TargetShutdown,
};
use uob_contracts::{TargetInstanceId, UtcTimestamp};

use crate::{
    configuration::{INTEGRATION_PATH_PREFIX, IntegrationCredentials, resolve_credentials},
    reads::{ReadExecutor, SupervisedReads},
    routing::{IntegrationState, integration_router},
    target::EmsScadaHttpTarget,
};

/// One supervised integration listener.
pub(crate) struct Session<E, P> {
    target: EmsScadaHttpTarget,
    context: TargetContext<E, P>,
}

impl<E, P> Session<E, P>
where
    E: Send + Sync + 'static,
    P: serde::de::DeserializeOwned + Send + 'static,
{
    pub(crate) const fn new(target: EmsScadaHttpTarget, context: TargetContext<E, P>) -> Self {
        Self { target, context }
    }

    pub(crate) async fn run(mut self) -> Result<(), TargetError> {
        self.emit_health(TargetHealthState::Starting, "ems_scada_http.starting", 0);
        let credentials = resolve_integration_credentials(
            self.target.settings.credentials_file.clone(),
            self.target.settings.target_instance_id.clone(),
        )
        .await?;
        let listener = bind(self.target.settings.listen_address).await?;
        // Canonical reads are answered by the host's scoped query port. The listener opens no
        // database, keeps no snapshot of its own, and duplicates no business handler.
        let reads = ReadExecutor::new(
            Arc::new(SupervisedReads::new(Arc::clone(&self.context.queries))),
            self.target.runtime.query_deadline,
        );
        let state = IntegrationState::new(
            self.target.descriptor(),
            self.target.listener_limits(),
            credentials,
            reads,
        )
        .with_commands(crate::commands::CommandExecutor::new(
            Arc::new(crate::commands::SupervisedCommands::new(Arc::clone(
                &self.context.commands,
            ))),
            self.target.runtime.query_deadline,
            self.target.runtime.maximum_in_flight_commands,
        ));

        let (stop, stopped) = oneshot::channel::<()>();
        let mut server = tokio::spawn(
            axum::serve(listener, integration_router(state))
                .with_graceful_shutdown(async move {
                    let _ = stopped.await;
                })
                .into_future(),
        );
        self.emit_health(TargetHealthState::Ready, "ems_scada_http.listening", 1);

        let outcome = self.serve(&mut server).await;
        let _ = stop.send(());
        self.emit_health(TargetHealthState::Stopped, "ems_scada_http.stopped", 0);
        outcome
    }

    /// Drains host deliveries until supervision requests shutdown.
    ///
    /// Every delivery is reported as local exposure: the canonical record is available on this
    /// bridge's integration surface, and no EMS client consumption is claimed. Draining keeps a
    /// slow or absent integration client from filling the host outbox or blocking charging.
    async fn serve(
        &mut self,
        server: &mut JoinHandle<std::io::Result<()>>,
    ) -> Result<(), TargetError> {
        loop {
            let reports = Arc::clone(&self.context.critical_reports);
            let delivery_id = tokio::select! {
                biased;
                () = Shutdown(&mut self.context.shutdown) => return Ok(()),
                delivery = Deliveries(&mut self.context.deliveries) => match delivery {
                    Some(delivery) => delivery.delivery_id,
                    None => return Ok(()),
                },
                result = &mut *server => return Err(listener_stopped(&result)),
            };
            report_local_exposure(&reports, delivery_id).await;
        }
    }

    fn emit_health(&self, state: TargetHealthState, reason: &'static str, connections: usize) {
        let _ = self
            .context
            .diagnostics
            .try_emit(TargetDiagnostic::Health(TargetHealth {
                state,
                delivery_backlog: self.context.deliveries.backlog(),
                in_flight_deliveries: 0,
                active_connections: connections,
                reason: Some(reason.to_owned()),
            }));
    }
}

fn listener_stopped(result: &Result<std::io::Result<()>, tokio::task::JoinError>) -> TargetError {
    let context = if matches!(result, Ok(Ok(()))) {
        "ems_scada_http.listener_stopped"
    } else {
        "ems_scada_http.listener_failed"
    };
    TargetError::new(
        TargetErrorCode::ConnectionUnavailable,
        ErrorRetryClassification::Retryable,
        context,
    )
}

async fn report_local_exposure(reports: &Arc<dyn TargetReportPort>, delivery_id: DeliveryId) {
    let _ = reports
        .report(DeliveryReport {
            delivery_id,
            outcome: DeliveryOutcome::LocallyExposed {
                surface: INTEGRATION_PATH_PREFIX.to_owned(),
            },
            reported_at: UtcTimestamp::new(OffsetDateTime::now_utc()),
        })
        .await;
}

/// Binds the configured listener.
///
/// A non-loopback address is refused here even though offline validation already required
/// explicit enablement, TLS references, and credentials: this build terminates no TLS, so binding
/// a public address would serve the integration API in cleartext.
async fn bind(address: SocketAddr) -> Result<TcpListener, TargetError> {
    if !address.ip().is_loopback() {
        return Err(permanent("ems_scada_http.remote_tls_not_available"));
    }
    TcpListener::bind(address)
        .await
        .map_err(|_| permanent("ems_scada_http.listener_bind_failed"))
}

/// Reads the scoped integration credential file off the runtime's reactor threads.
async fn resolve_integration_credentials(
    reference: Option<CredentialReference>,
    target_instance_id: TargetInstanceId,
) -> Result<IntegrationCredentials, TargetError> {
    tokio::task::spawn_blocking(move || {
        resolve_credentials(reference.as_ref(), &target_instance_id)
    })
    .await
    .map_err(|_| permanent("ems_scada_http.credentials_task_failed"))?
    .map_err(permanent)
}

fn permanent(context: &'static str) -> TargetError {
    TargetError::new(
        TargetErrorCode::InvalidConfiguration,
        ErrorRetryClassification::Permanent,
        context,
    )
}

/// Adapts the poll-based supervision signal into an awaitable future.
struct Shutdown<'a>(&'a mut Pin<Box<dyn TargetShutdown>>);

impl Future for Shutdown<'_> {
    type Output = ();

    fn poll(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Self::Output> {
        self.get_mut().0.as_mut().poll_shutdown(context)
    }
}

/// Adapts the poll-based bounded delivery receiver into an awaitable future.
struct Deliveries<'a, E>(&'a mut Pin<Box<dyn TargetDeliveryReceiver<E>>>);

impl<E> Future for Deliveries<'_, E> {
    type Output = Option<TargetDelivery<E>>;

    fn poll(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Self::Output> {
        self.get_mut().0.as_mut().poll_receive(context)
    }
}
