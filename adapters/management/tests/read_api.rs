use std::{sync::Arc, time::Duration};

use axum::{
    body::{Body, to_bytes},
    http::Request,
};
use tower::ServiceExt;
use uob_application::{
    Application, CanonicalQuerySource, CoreLoopState, Page, SnapshotCursor, StorageHealthState,
    TargetPortError, TargetPortErrorCode, TargetPortFuture, TargetQuery, TargetQueryAuthorization,
    TargetQueryPermission, TargetQueryResult, TargetResourceScope, TargetRetainedEventStream,
};
use uob_contracts::{
    ArtifactDigest, BridgeId, Connectivity, ContractVersion, Environment, ProcessInstanceId,
    ReleaseId, ResourceCapabilities, ResourceRef, RuntimeIdentity, ServiceIdentity, StationId,
    StationSnapshot, TargetInstanceId, UtcTimestamp,
};
use uob_management_adapter::{ManagementReadLimits, ManagementRouterOptions, router_with_queries};

struct SnapshotSource {
    items: Vec<StationSnapshot>,
    delay: Duration,
}

impl CanonicalQuerySource<serde_json::Value> for SnapshotSource {
    fn query<'a>(
        &'a self,
        authorization: &'a TargetQueryAuthorization,
        query: TargetQuery,
    ) -> TargetPortFuture<'a, TargetQueryResult<serde_json::Value>> {
        Box::pin(async move {
            tokio::time::sleep(self.delay).await;
            match query {
                TargetQuery::StationSnapshots(query) => {
                    Ok(TargetQueryResult::StationSnapshots(Page {
                        items: self
                            .items
                            .iter()
                            .filter(|item| authorization.permits_resource(&item.station))
                            .take(usize::from(query.limit.get()))
                            .cloned()
                            .collect(),
                        next_cursor: Some(SnapshotCursor::new("snapshot:next").unwrap()),
                    }))
                }
                TargetQuery::StationSnapshot(resource) => Ok(TargetQueryResult::StationSnapshot(
                    self.items
                        .iter()
                        .find(|item| item.station == resource)
                        .cloned(),
                )),
                _ => Err(TargetPortError::new(
                    TargetPortErrorCode::Unsupported,
                    "query.unsupported",
                )),
            }
        })
    }

    fn subscribe_retained_events<'a>(
        &'a self,
        _: &'a TargetQueryAuthorization,
        _: uob_application::RetainedEventQuery,
    ) -> TargetPortFuture<'a, TargetRetainedEventStream<serde_json::Value>> {
        Box::pin(async {
            Err(TargetPortError::new(
                TargetPortErrorCode::Unsupported,
                "query.unsupported",
            ))
        })
    }
}

fn application() -> Application {
    let application = Application::new(ServiceIdentity {
        bridge_id: BridgeId::new("bridge-api").unwrap(),
        runtime: RuntimeIdentity {
            environment: Environment::Production,
            release_id: ReleaseId::new("release-api").unwrap(),
            release_digest: ArtifactDigest::new("sha256:api").unwrap(),
            process_instance_id: ProcessInstanceId::new("process-api").unwrap(),
        },
        selected_target_id: None,
    });
    application.health().report_core_loop(CoreLoopState::Ready);
    application
        .health()
        .report_storage(StorageHealthState::Safe, None);
    application
}

fn snapshot(station_id: &str) -> StationSnapshot {
    StationSnapshot {
        schema_version: ContractVersion::V1_INITIAL,
        station: ResourceRef {
            bridge_id: BridgeId::new("bridge-api").unwrap(),
            station_id: StationId::new(station_id).unwrap(),
            resource: None,
            native_protocol_reference: None,
        },
        observed_at: serde_json::from_str::<UtcTimestamp>("\"1970-01-01T00:00:00Z\"").unwrap(),
        connectivity: Connectivity::Disconnected,
        capabilities: ResourceCapabilities::default(),
        resources: vec![],
        transactions: vec![],
        current_values: vec![],
    }
}

fn query_router(source: SnapshotSource, limits: ManagementReadLimits) -> axum::Router {
    router_with_queries(
        application(),
        Arc::new(source),
        TargetQueryAuthorization::new(
            TargetInstanceId::new("management-api").unwrap(),
            vec![TargetQueryPermission::StationSnapshots],
            vec![TargetResourceScope::Station {
                bridge_id: BridgeId::new("bridge-api").unwrap(),
                station_id: StationId::new("station-a").unwrap(),
            }],
        ),
        limits,
        ManagementRouterOptions::default(),
    )
}

#[tokio::test]
async fn inventory_is_canonical_paginated_and_resource_scoped() {
    let response = query_router(
        SnapshotSource {
            items: vec![snapshot("station-a"), snapshot("station-b")],
            delay: Duration::ZERO,
        },
        ManagementReadLimits::default(),
    )
    .oneshot(
        Request::builder()
            .uri("/api/v1/stations?limit=1")
            .body(Body::empty())
            .unwrap(),
    )
    .await
    .unwrap();
    assert_eq!(response.status(), axum::http::StatusCode::OK);
    let value: serde_json::Value =
        serde_json::from_slice(&to_bytes(response.into_body(), 65_536).await.unwrap()).unwrap();
    assert_eq!(value["items"].as_array().unwrap().len(), 1);
    assert_eq!(value["items"][0]["station"]["station_id"], "station-a");
    assert_eq!(value["next_cursor"], "snapshot:next");
}

#[tokio::test]
async fn invalid_bounds_and_slow_queries_return_sanitized_failures() {
    let invalid = query_router(
        SnapshotSource {
            items: vec![],
            delay: Duration::ZERO,
        },
        ManagementReadLimits::default(),
    )
    .oneshot(
        Request::builder()
            .uri("/api/v1/stations?limit=101")
            .body(Body::empty())
            .unwrap(),
    )
    .await
    .unwrap();
    assert_eq!(invalid.status(), axum::http::StatusCode::BAD_REQUEST);

    let timed_out = query_router(
        SnapshotSource {
            items: vec![],
            delay: Duration::from_millis(20),
        },
        ManagementReadLimits {
            maximum_concurrent_queries: 1,
            query_timeout: Duration::from_millis(1),
        },
    )
    .oneshot(
        Request::builder()
            .uri("/api/v1/stations")
            .body(Body::empty())
            .unwrap(),
    )
    .await
    .unwrap();
    assert_eq!(timed_out.status(), axum::http::StatusCode::GATEWAY_TIMEOUT);
    let value: serde_json::Value =
        serde_json::from_slice(&to_bytes(timed_out.into_body(), 1024).await.unwrap()).unwrap();
    assert_eq!(value["error"], "query.deadline_exceeded");
}
