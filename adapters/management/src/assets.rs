use axum::{
    http::header,
    response::{IntoResponse, Response},
};

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

// Fixed allowlisted embedded files: no runtime filesystem, build tool, or path traversal.
// Regenerate with the isolated frontend build before committing a console change.
pub(crate) async fn browser_entry() -> Response {
    asset(
        "text/html; charset=utf-8",
        include_bytes!("../ui/index.html"),
    )
}

pub(crate) async fn browser_script() -> Response {
    asset(
        "text/javascript; charset=utf-8",
        include_bytes!("../ui/assets/console.js"),
    )
}

pub(crate) async fn browser_style() -> Response {
    asset(
        "text/css; charset=utf-8",
        include_bytes!("../ui/assets/console.css"),
    )
}

fn asset(content_type: &'static str, bytes: &'static [u8]) -> Response {
    (
        [
            (header::CONTENT_TYPE, content_type),
            (header::CACHE_CONTROL, "no-store"),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
            (header::REFERRER_POLICY, "no-referrer"),
            (header::CONTENT_SECURITY_POLICY,
                "default-src 'none'; script-src 'self'; style-src 'self'; connect-src 'self'; img-src 'self'; font-src 'self'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'"),
        ],
        bytes,
    ).into_response()
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
        for path in ["/ui/assets/console.js", "/ui/assets/console.css"] {
            let response = router
                .clone()
                .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(response.status(), axum::http::StatusCode::NOT_FOUND);
        }
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

    #[tokio::test]
    async fn compiled_assets_have_explicit_types_security_headers_and_bounded_size() {
        let router = crate::router(application());
        for (path, content_type) in [
            ("/", "text/html; charset=utf-8"),
            ("/ui/assets/console.js", "text/javascript; charset=utf-8"),
            ("/ui/assets/console.css", "text/css; charset=utf-8"),
        ] {
            let response = router
                .clone()
                .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(response.status(), axum::http::StatusCode::OK);
            assert_eq!(response.headers()["content-type"], content_type);
            assert_eq!(response.headers()["cache-control"], "no-store");
            assert_eq!(response.headers()["x-content-type-options"], "nosniff");
            assert!(
                response.headers()["content-security-policy"]
                    .to_str()
                    .unwrap()
                    .contains("connect-src 'self'")
            );
            let body = axum::body::to_bytes(response.into_body(), 300 * 1024)
                .await
                .unwrap();
            assert!(!body.is_empty());
        }
        for path in [
            "/ui/assets/unknown.js",
            "/ui/src/App.tsx",
            "/ui/package.json",
        ] {
            let response = router
                .clone()
                .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(response.status(), axum::http::StatusCode::NOT_FOUND);
        }
    }
}
