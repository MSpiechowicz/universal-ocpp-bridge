#![doc = "Axum-based management adapter."]

use std::{io, net::SocketAddr};

use axum::{Json, Router, extract::State, routing::get};
use uob_application::Application;

/// Builds the management router while keeping framework types out of the application.
pub fn router(application: Application) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/api/v1/identity", get(identity))
        .with_state(application)
}

async fn health(State(application): State<Application>) -> &'static str {
    debug_assert_eq!(application.contract_version().major, 1);
    "starting"
}

async fn identity(State(application): State<Application>) -> Json<uob_contracts::ServiceIdentity> {
    Json(application.identity().clone())
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
    use axum::{
        body::{Body, to_bytes},
        http::Request,
    };
    use tower::ServiceExt;
    use uob_application::Application;
    use uob_contracts::{
        ArtifactDigest, BridgeId, Environment, ProcessInstanceId, ReleaseId, RuntimeIdentity,
        ServiceIdentity,
    };

    use super::router;

    fn application() -> Application {
        Application::new(ServiceIdentity {
            bridge_id: BridgeId::new("bridge-api").expect("bridge ID"),
            runtime: RuntimeIdentity {
                environment: Environment::Production,
                release_id: ReleaseId::new("release-api").expect("release ID"),
                release_digest: ArtifactDigest::new("sha256:api").expect("release digest"),
                process_instance_id: ProcessInstanceId::new("process-api").expect("process ID"),
            },
            selected_target_id: None,
        })
    }

    #[tokio::test]
    async fn health_route_is_wired_without_leaking_axum_into_application() {
        let response = router(application())
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

    #[tokio::test]
    async fn api_only_service_exposes_trusted_identity() {
        let response = router(application())
            .oneshot(
                Request::builder()
                    .uri("/api/v1/identity")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .expect("identity response");

        assert!(response.status().is_success());
        let body = to_bytes(response.into_body(), 16 * 1024)
            .await
            .expect("identity body");
        let value: serde_json::Value = serde_json::from_slice(&body).expect("identity JSON");
        assert_eq!(value["bridge_id"], "bridge-api");
        assert_eq!(value["runtime"]["environment"], "production");
        assert!(value.get("selected_target_id").is_none());
    }
}
