use axum::response::Html;

/// Startup-only routing choices that do not change the management API surface.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ManagementRouterOptions {
    /// Whether the optional browser entry asset is mounted.
    pub static_assets: bool,
}

impl Default for ManagementRouterOptions {
    fn default() -> Self {
        Self {
            static_assets: true,
        }
    }
}

pub(crate) async fn browser_entry() -> Html<&'static str> {
    Html(
        "<!doctype html><html lang=\"en\"><title>Universal OCPP Bridge</title><body><main><h1>Universal OCPP Bridge</h1><p>Management API is available.</p></main></body></html>",
    )
}

#[cfg(test)]
mod tests {
    use axum::{body::Body, http::Request};
    use tower::ServiceExt;
    use uob_application::Application;
    use uob_contracts::{
        ArtifactDigest, BridgeId, Environment, ProcessInstanceId, ReleaseId, RuntimeIdentity,
        ServiceIdentity,
    };

    use super::ManagementRouterOptions;

    fn application() -> Application {
        Application::new(ServiceIdentity {
            bridge_id: BridgeId::new("asset-test").unwrap(),
            runtime: RuntimeIdentity {
                environment: Environment::Demo,
                release_id: ReleaseId::new("asset-test").unwrap(),
                release_digest: ArtifactDigest::new("sha256:asset-test").unwrap(),
                process_instance_id: ProcessInstanceId::new("asset-test").unwrap(),
            },
            selected_target_id: None,
        })
    }

    #[tokio::test]
    async fn no_ui_removes_only_static_assets() {
        let router = crate::router_with_options(
            application(),
            ManagementRouterOptions {
                static_assets: false,
            },
        );
        let root = router
            .clone()
            .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
            .await
            .unwrap();
        let identity = router
            .oneshot(
                Request::builder()
                    .uri("/api/v1/identity")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(root.status(), axum::http::StatusCode::NOT_FOUND);
        assert_eq!(identity.status(), axum::http::StatusCode::OK);
    }
}
