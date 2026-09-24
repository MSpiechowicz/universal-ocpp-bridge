use crate::{ManagementRouterOptions, router_with_authenticated_events, router_with_options};
use axum::Router;
use serde_json::Value;
use std::{future::Future, io, net::SocketAddr, sync::Arc};
use uob_application::{Application, CanonicalQuerySource};

/// Binds and serves the management adapter.
///
/// # Errors
///
/// Returns an I/O error when the listener cannot bind or the server fails.
pub async fn serve(address: SocketAddr, application: Application) -> io::Result<()> {
    serve_with_options(address, application, ManagementRouterOptions::default()).await
}

/// Binds and serves the management adapter with explicit static-asset routing.
///
/// # Errors
///
/// Returns an I/O error when the listener is unsafe, cannot bind, or the server fails.
pub async fn serve_with_options(
    address: SocketAddr,
    application: Application,
    options: ManagementRouterOptions,
) -> io::Result<()> {
    serve_with_shutdown(address, application, options, std::future::pending()).await
}

/// Serves until the host stops ingress, then waits for active requests to finish.
///
/// # Errors
/// Returns an I/O error for unsafe binding or listener failure. The host must enforce
/// its overall shutdown deadline and terminate the runtime if requests cannot drain.
pub async fn serve_with_shutdown(
    address: SocketAddr,
    application: Application,
    options: ManagementRouterOptions,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> io::Result<()> {
    serve_with_readiness(address, application, options, shutdown, || Ok(())).await
}

/// Serves with a host readiness hook after listener binding and router construction.
///
/// # Errors
/// Returns listener, readiness-hook, or server errors; never calls the hook on bind failure.
pub async fn serve_with_readiness(
    address: SocketAddr,
    application: Application,
    options: ManagementRouterOptions,
    shutdown: impl Future<Output = ()> + Send + 'static,
    ready: impl FnOnce() -> io::Result<()>,
) -> io::Result<()> {
    serve_with_capture_and_release_readiness(
        address,
        application,
        options,
        None,
        None,
        shutdown,
        ready,
    )
    .await
}

/// Serves with optional explicit capture and release-read credentials.
///
/// The release bridge is separately authenticated and mounted only when the host deliberately
/// supplies its fixed supervisor-socket configuration.
///
/// # Errors
/// Returns an I/O error for unsafe binding, readiness failure, or listener failure.
pub async fn serve_with_capture_and_release_readiness(
    address: SocketAddr,
    application: Application,
    options: ManagementRouterOptions,
    capture: Option<crate::ManagementCaptureConfiguration>,
    release_read: Option<crate::ManagementReleaseReadConfiguration>,
    shutdown: impl Future<Output = ()> + Send + 'static,
    ready: impl FnOnce() -> io::Result<()>,
) -> io::Result<()> {
    let identity = application.identity().clone();
    let router = router_with_options(application, options);
    serve_configured_router(
        address,
        identity,
        router,
        capture,
        release_read,
        shutdown,
        ready,
    )
    .await
}

/// Serves bearer-authenticated canonical reads and events alongside the existing capture and
/// release-read routes, using the same listener and readiness policy as the unconfigured host.
///
/// # Errors
/// Returns an I/O error for unsafe binding, readiness failure, or listener failure.
#[allow(clippy::too_many_arguments)]
pub async fn serve_with_authenticated_events_and_capture_and_release_readiness(
    address: SocketAddr,
    application: Application,
    source: Arc<dyn CanonicalQuerySource<Value>>,
    read_limits: crate::ManagementReadLimits,
    event_configuration: crate::ManagementEventConfiguration,
    options: ManagementRouterOptions,
    capture: Option<crate::ManagementCaptureConfiguration>,
    release_read: Option<crate::ManagementReleaseReadConfiguration>,
    shutdown: impl Future<Output = ()> + Send + 'static,
    ready: impl FnOnce() -> io::Result<()>,
) -> io::Result<()> {
    let identity = application.identity().clone();
    let router = router_with_authenticated_events(
        application,
        source,
        read_limits,
        event_configuration,
        options,
    );
    serve_configured_router(
        address,
        identity,
        router,
        capture,
        release_read,
        shutdown,
        ready,
    )
    .await
}

async fn serve_configured_router(
    address: SocketAddr,
    identity: uob_contracts::ServiceIdentity,
    mut router: Router,
    capture: Option<crate::ManagementCaptureConfiguration>,
    release_read: Option<crate::ManagementReleaseReadConfiguration>,
    shutdown: impl Future<Output = ()> + Send + 'static,
    ready: impl FnOnce() -> io::Result<()>,
) -> io::Result<()> {
    if !address.ip().is_loopback() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "management listener requires validated remote TLS configuration",
        ));
    }
    let listener = tokio::net::TcpListener::bind(address).await?;
    if let Some(capture) = capture {
        router = router.merge(crate::capture_router(identity, capture));
    }
    if let Some(release_read) = release_read {
        router = router.merge(crate::release_read_router(release_read));
    }
    ready()?;
    axum::serve(listener, router)
        .with_graceful_shutdown(shutdown)
        .await
}
