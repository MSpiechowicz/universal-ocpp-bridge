#![doc = "Axum-based management adapter."]

mod assets;
mod health_view;
mod security;

use std::{fmt::Write, io, net::SocketAddr};

use axum::{
    Json, Router,
    extract::State,
    http::{StatusCode, header},
    response::IntoResponse,
    routing::get,
};
use uob_application::{
    Application, AuxiliaryProcess, ComponentKind, HealthSnapshot, ProcessResourceMetrics,
    ReadinessState,
};

use health_view::health_json;

pub use assets::ManagementRouterOptions;
pub use security::{
    DEFAULT_MANAGEMENT_LISTEN_ADDRESS, ManagementCredentialConfiguration,
    ManagementListenerConfiguration, ManagementListenerPolicyError, ManagementTlsConfiguration,
};

/// Builds the management router while keeping framework types out of the application.
pub fn router(application: Application) -> Router {
    router_with_options(application, ManagementRouterOptions::default())
}

/// Builds the same API router with optional static browser assets.
pub fn router_with_options(application: Application, options: ManagementRouterOptions) -> Router {
    let router = Router::new()
        .route("/health", get(health))
        .route("/api/v1/health", get(detailed_health))
        .route("/metrics", get(metrics))
        .route("/api/v1/identity", get(identity));
    let router = if options.static_assets {
        router.route("/", get(assets::browser_entry))
    } else {
        router
    };
    router.with_state(application)
}

async fn health(State(application): State<Application>) -> impl IntoResponse {
    health_response(&application)
}

async fn detailed_health(State(application): State<Application>) -> impl IntoResponse {
    health_response(&application)
}

fn health_response(application: &Application) -> (StatusCode, Json<serde_json::Value>) {
    sample_daemon_process(application);
    let snapshot = application.health().snapshot();
    let status = if snapshot.readiness == ReadinessState::Ready {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };
    (status, Json(health_json(&snapshot)))
}

async fn metrics(State(application): State<Application>) -> impl IntoResponse {
    sample_daemon_process(&application);
    let body = encode_metrics(&application.health().snapshot());
    ([(header::CONTENT_TYPE, "text/plain; version=0.0.4")], body)
}

async fn identity(State(application): State<Application>) -> Json<uob_contracts::ServiceIdentity> {
    Json(application.identity().clone())
}

fn sample_daemon_process(application: &Application) {
    if let Some(metrics) = linux_process_metrics() {
        application.health().report_daemon_process(metrics);
    }
}

fn linux_process_metrics() -> Option<ProcessResourceMetrics> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    let rss_kib = status
        .lines()
        .find_map(|line| line.strip_prefix("VmRSS:"))?
        .split_whitespace()
        .next()?
        .parse::<u64>()
        .ok()?;
    let schedstat = std::fs::read_to_string("/proc/self/schedstat").ok()?;
    let cpu_nanoseconds = schedstat.split_whitespace().next()?.parse::<u64>().ok()?;
    Some(ProcessResourceMetrics {
        rss_bytes: rss_kib.saturating_mul(1_024),
        cpu_time_milliseconds: cpu_nanoseconds / 1_000_000,
    })
}

fn encode_metrics(snapshot: &HealthSnapshot) -> String {
    let mut output = String::new();
    metric(
        &mut output,
        "uob_core_ready",
        u64::from(snapshot.readiness == ReadinessState::Ready),
    );
    metric(
        &mut output,
        "uob_accepts_new_sessions",
        u64::from(snapshot.accepts_new_sessions),
    );
    metric(
        &mut output,
        "uob_runtime_connected_stations",
        snapshot.runtime.connected_stations as u64,
    );
    metric(
        &mut output,
        "uob_runtime_queued_payload_bytes",
        snapshot.runtime.queued_payload_bytes as u64,
    );
    metric(
        &mut output,
        "uob_runtime_trace_ring_bytes",
        snapshot.runtime.trace_ring_bytes as u64,
    );
    metric(
        &mut output,
        "uob_dropped_diagnostics_total",
        snapshot.runtime.dropped_diagnostics,
    );
    metric(
        &mut output,
        "uob_dropped_telemetry_total",
        snapshot.dropped_telemetry,
    );
    metric(
        &mut output,
        "uob_uncertain_commands_total",
        snapshot.uncertain_commands,
    );
    latency_metrics(
        &mut output,
        "local_response",
        snapshot.local_response_latency,
    );
    latency_metrics(&mut output, "storage", snapshot.storage_latency);
    if let Some(storage) = snapshot.storage_retention {
        metric(&mut output, "uob_storage_used_bytes", storage.used_bytes);
        metric(
            &mut output,
            "uob_storage_budget_bytes",
            storage.budget_bytes,
        );
        metric(
            &mut output,
            "uob_storage_required_delivery_backlog",
            storage.retained_required_deliveries,
        );
    }
    if let Some(process) = snapshot.daemon_process {
        process_metrics(&mut output, "daemon", process);
    }
    for (process, resources) in &snapshot.auxiliary_processes {
        process_metrics(&mut output, auxiliary_name(*process), *resources);
    }
    for (component, health) in &snapshot.components {
        let name = component_name(*component);
        labeled_metric(
            &mut output,
            "uob_component_reconnects_total",
            "component",
            name,
            health.reconnects,
        );
        labeled_metric(
            &mut output,
            "uob_component_backlog_items",
            "component",
            name,
            health.backlog_items as u64,
        );
        labeled_metric(
            &mut output,
            "uob_component_in_flight_items",
            "component",
            name,
            health.in_flight_items as u64,
        );
    }
    queue_metrics(&mut output, snapshot);
    output
}

fn queue_metrics(output: &mut String, snapshot: &HealthSnapshot) {
    let queues = snapshot.runtime.queues;
    for (name, value) in [
        ("charger_requests", queues.charger_requests),
        ("database_work", queues.database_work),
        ("subscribers", queues.subscribers),
        ("pending_requests", queues.pending_requests),
        ("multipart_assemblies", queues.multipart_assemblies),
        ("target_ingress", queues.target_ingress),
        ("target_egress", queues.target_egress),
        ("target_retries", queues.target_retries),
        ("critical_reports", queues.critical_reports),
        ("diagnostics", queues.diagnostics),
        ("exporter_batches", queues.exporter_batches),
        ("capture_records", queues.capture_records),
    ] {
        labeled_metric(
            output,
            "uob_runtime_queue_items",
            "queue",
            name,
            value as u64,
        );
    }
}

fn latency_metrics(output: &mut String, class: &str, latency: uob_application::LatencySnapshot) {
    labeled_metric(
        output,
        "uob_latency_samples_total",
        "class",
        class,
        latency.samples,
    );
    labeled_metric(
        output,
        "uob_latency_max_milliseconds",
        "class",
        class,
        latency.maximum_ms,
    );
    labeled_metric(
        output,
        "uob_latency_overflow_total",
        "class",
        class,
        latency.overflow_samples,
    );
    if let Some(p95) = latency.p95_upper_bound_ms {
        labeled_metric(
            output,
            "uob_latency_p95_upper_bound_milliseconds",
            "class",
            class,
            p95,
        );
    }
}

fn process_metrics(output: &mut String, process: &str, resources: ProcessResourceMetrics) {
    labeled_metric(
        output,
        "uob_process_rss_bytes",
        "process",
        process,
        resources.rss_bytes,
    );
    labeled_metric(
        output,
        "uob_process_cpu_time_milliseconds_total",
        "process",
        process,
        resources.cpu_time_milliseconds,
    );
}

fn metric(output: &mut String, name: &str, value: u64) {
    let _ = writeln!(output, "{name} {value}");
}
fn labeled_metric(output: &mut String, name: &str, label: &str, label_value: &str, value: u64) {
    let _ = writeln!(output, "{name}{{{label}=\"{label_value}\"}} {value}");
}

const fn component_name(kind: ComponentKind) -> &'static str {
    match kind {
        ComponentKind::Target => "target",
        ComponentKind::ExternalBroker => "external_broker",
        ComponentKind::ExternalClient => "external_client",
        ComponentKind::ExternalExporter => "external_exporter",
    }
}

const fn auxiliary_name(process: AuxiliaryProcess) -> &'static str {
    match process {
        AuxiliaryProcess::Broker => "broker",
        AuxiliaryProcess::ExternalDatabase => "external_database",
        AuxiliaryProcess::Browser => "browser",
        AuxiliaryProcess::Simulator => "simulator",
        AuxiliaryProcess::Provider => "provider",
        AuxiliaryProcess::ReleaseManager => "release_manager",
    }
}

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
    if !address.ip().is_loopback() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "management listener requires validated remote TLS configuration",
        ));
    }
    let listener = tokio::net::TcpListener::bind(address).await?;
    axum::serve(listener, router_with_options(application, options)).await
}

#[cfg(test)]
mod tests {
    use axum::{
        body::{Body, to_bytes},
        http::Request,
    };
    use tower::ServiceExt;
    use uob_application::{Application, CoreLoopState, StorageHealthState};
    use uob_contracts::{
        ArtifactDigest, BridgeId, Environment, ProcessInstanceId, ReleaseId, RuntimeIdentity,
        ServiceIdentity,
    };

    use super::router;

    fn application() -> Application {
        let application = Application::new(ServiceIdentity {
            bridge_id: BridgeId::new("bridge-api").expect("bridge ID"),
            runtime: RuntimeIdentity {
                environment: Environment::Production,
                release_id: ReleaseId::new("release-api").expect("release ID"),
                release_digest: ArtifactDigest::new("sha256:api").expect("release digest"),
                process_instance_id: ProcessInstanceId::new("process-api").expect("process ID"),
            },
            selected_target_id: None,
        });
        application.health().report_core_loop(CoreLoopState::Ready);
        application
            .health()
            .report_storage(StorageHealthState::Safe, None);
        application
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
    async fn failed_core_returns_service_unavailable() {
        let application = application();
        application.health().report_core_loop(CoreLoopState::Failed);
        let response = router(application)
            .oneshot(
                Request::builder()
                    .uri("/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(
            response.status(),
            axum::http::StatusCode::SERVICE_UNAVAILABLE
        );
    }

    #[tokio::test]
    async fn external_export_outage_is_visible_without_failing_core_readiness() {
        let application = application();
        application.health().report_component(
            uob_application::ComponentKind::ExternalExporter,
            uob_application::ComponentHealth {
                state: uob_application::ComponentHealthState::Reconnecting,
                reconnects: 3,
                backlog_items: 5,
                in_flight_items: 1,
                active_connections: 0,
                reason: Some("export.connection_unavailable".into()),
            },
        );
        let response = router(application)
            .oneshot(
                Request::builder()
                    .uri("/api/v1/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), axum::http::StatusCode::OK);
        let body = to_bytes(response.into_body(), 64 * 1024).await.unwrap();
        let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(value["readiness"], "ready");
        assert_eq!(
            value["components"]["external_exporter"]["state"],
            "reconnecting"
        );
        assert_eq!(value["components"]["external_exporter"]["backlog_items"], 5);
    }

    #[tokio::test]
    async fn metrics_separate_daemon_and_auxiliary_processes() {
        let application = application();
        application.health().report_auxiliary_process(
            uob_application::AuxiliaryProcess::Broker,
            uob_application::ProcessResourceMetrics {
                rss_bytes: 42,
                cpu_time_milliseconds: 7,
            },
        );
        let response = router(application)
            .oneshot(
                Request::builder()
                    .uri("/metrics")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = to_bytes(response.into_body(), 64 * 1024).await.unwrap();
        let text = std::str::from_utf8(&body).unwrap();

        assert!(text.contains("uob_process_rss_bytes{process=\"daemon\"}"));
        assert!(text.contains("uob_process_rss_bytes{process=\"broker\"} 42"));
        assert!(text.contains("uob_runtime_queue_items{queue=\"target_retries\"}"));
        assert!(text.contains("uob_uncertain_commands_total"));
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

    #[tokio::test]
    async fn low_level_server_cannot_bypass_remote_listener_policy() {
        let error = super::serve(std::net::SocketAddr::from(([0, 0, 0, 0], 0)), application())
            .await
            .expect_err("unvalidated remote listener");

        assert_eq!(error.kind(), std::io::ErrorKind::PermissionDenied);
    }
}
