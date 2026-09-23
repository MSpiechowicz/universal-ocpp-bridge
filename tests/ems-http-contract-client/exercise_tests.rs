use super::{
    queries::{point_matches, station_matches},
    run_with_deadline,
};
use axum::{Json, Router, extract::State, routing::get};
use serde_json::json;
use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

#[test]
fn unknown_optional_station_and_point_fields_do_not_hide_required_observations() {
    let station = json!({
        "connectivity":{"status":"connected","future_connected_detail":true},
        "resources":[{"resource":{"native_protocol_reference":{"protocol":"ocpp201",
            "future_protocol_detail":{"extension":"new"}}},
            "future_resource_detail":[1,2]}],
        "future_station_detail":"opaque"
    });
    assert!(station_matches(&station, "ocpp201"));
    assert!(!station_matches(&station, "ocpp16"));
    let point = json!({"point_id":"power.active","value":{"quality":{"level":"good"},
        "future_quality_detail":true},"future_point_detail":"opaque"});
    assert!(point_matches(&point, "power.active"));
    assert!(!point_matches(&point, "another-point"));
}

#[tokio::test]
async fn aggregate_deadline_cancels_slow_inventory_polling_without_exposing_tokens() {
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    let requests = Arc::new(AtomicUsize::new(0));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let app = Router::new()
        .route(
            "/bridge/v1/capabilities",
            get(|| async { Json(json!({"target":{"kind":"ems-scada.http"}})) }),
        )
        .route(
            "/bridge/v1/stations/station-a",
            get(|State(requests): State<Arc<AtomicUsize>>| async move {
                requests.fetch_add(1, Ordering::SeqCst);
                tokio::time::sleep(Duration::from_millis(80)).await;
                Json(json!({"connectivity":{"status":"disconnected"}}))
            }),
        )
        .with_state(requests.clone());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let demo: super::super::Demo = toml::from_str(include_str!("demo.toml")).unwrap();
    let start = Instant::now();
    let result = run_with_deadline(
        &base,
        "reader-private-token",
        "operator-private-token",
        &demo,
        false,
        Duration::from_millis(400),
    )
    .await;
    assert!(start.elapsed() < Duration::from_secs(2));
    match result {
        Err(error) => assert_eq!(error.to_string(), "exercise deadline exceeded"),
        Ok(_) => panic!("slow inventory should exceed the aggregate deadline"),
    }
    let before = requests.load(Ordering::SeqCst);
    assert!(
        before >= 2,
        "inventory must have polled before the deadline"
    );
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        requests.load(Ordering::SeqCst),
        before,
        "polling was not cancelled"
    );
    server.abort();
}
