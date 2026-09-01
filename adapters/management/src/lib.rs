#![doc = "Axum-based management adapter."]

use std::{io, net::SocketAddr};

use axum::{Router, extract::State, routing::get};
use uob_application::Application;

/// Builds the management router while keeping framework types out of the application.
pub fn router(application: Application) -> Router {
    Router::new()
        .route("/health", get(health))
        .with_state(application)
}

async fn health(State(application): State<Application>) -> &'static str {
    debug_assert_eq!(application.contract_version().major, 1);
    "starting"
}

/// Binds and serves the management adapter.
///
/// # Errors
///
/// Returns an I/O error when the listener cannot bind or the server fails.
pub async fn serve(address: SocketAddr, application: Application) -> io::Result<()> {
    let listener = tokio::net::TcpListener::bind(address).await?;
    axum::serve(listener, router(application)).await
}

#[cfg(test)]
mod tests {
    use axum::{body::Body, http::Request};
    use tower::ServiceExt;
    use uob_application::Application;

    use super::router;

    #[tokio::test]
    async fn health_route_is_wired_without_leaking_axum_into_application() {
        let response = router(Application::new())
            .oneshot(
                Request::builder()
                    .uri("/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .expect("health response");

        assert!(response.status().is_success());
    }
}
