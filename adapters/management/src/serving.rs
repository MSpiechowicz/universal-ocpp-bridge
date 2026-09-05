use crate::{ManagementRouterOptions, router_with_options};
use std::{io, net::SocketAddr};
use uob_application::Application;

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
    shutdown: impl std::future::Future<Output = ()> + Send + 'static,
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
    shutdown: impl std::future::Future<Output = ()> + Send + 'static,
    ready: impl FnOnce() -> io::Result<()>,
) -> io::Result<()> {
    if !address.ip().is_loopback() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "management listener requires validated remote TLS configuration",
        ));
    }
    let listener = tokio::net::TcpListener::bind(address).await?;
    let router = router_with_options(application, options);
    ready()?;
    axum::serve(listener, router)
        .with_graceful_shutdown(shutdown)
        .await
}
