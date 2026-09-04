use axum::http::StatusCode;
use serde_json::Value;

use crate::test_support::{
    CONTROLLER_TOKEN, CanonicalFixtures, READER_TOKEN, STATION_SCOPED_TOKEN, get, router_with,
    scoped_credentials,
};

fn router() -> axum::Router {
    router_with(
        scoped_credentials(),
        CanonicalFixtures::both_resource_models(),
        16,
    )
}

fn single_station_router() -> axum::Router {
    router_with(
        scoped_credentials(),
        CanonicalFixtures::single_station(),
        16,
    )
}

fn point<'a>(body: &'a Value, point_id: &str) -> &'a Value {
    body["items"]
        .as_array()
        .expect("point page")
        .iter()
        .find(|item| item["point_id"] == point_id)
        .unwrap_or_else(|| panic!("{point_id} missing from {body}"))
}

#[tokio::test]
async fn a_point_keeps_its_exact_decimal_unit_phase_and_context() {
    let (status, body) = get(
        router(),
        "/bridge/v1/points?station_id=station-a",
        Some(READER_TOKEN),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let energy = point(&body, "energy.active.import.register");
    // The exact decimal is rendered digit for digit, never as a binary float.
    assert_eq!(energy["value"]["value"]["type"], "decimal");
    assert_eq!(energy["value"]["value"]["value"], "123456.789");
    assert_eq!(energy["descriptor"]["unit"], "watt_hour");
    assert_eq!(energy["descriptor"]["access"], "read_only");
    assert_eq!(energy["value"]["measurement"]["phase"], "L1-N");
    assert_eq!(energy["value"]["measurement"]["context"], "Sample.Periodic");
    assert_eq!(energy["value"]["source_time"], "1970-01-01T00:00:29Z");
    assert_eq!(energy["value"]["observed_at"], "1970-01-01T00:00:30Z");
    assert_eq!(energy["value"]["quality"]["level"], "good");
    assert_eq!(energy["value"]["freshness"]["status"], "fresh");
}

#[tokio::test]
async fn an_absent_source_timestamp_and_its_quality_are_reported_as_such() {
    let (_, body) = get(
        router(),
        "/bridge/v1/points?station_id=station-a",
        Some(READER_TOKEN),
    )
    .await;

    let meter = point(&body, "station.meter.active.import");
    // Absence is explicit: no source timestamp, no value, and an uncertain quality with a reason.
    assert!(meter["value"].get("source_time").is_none());
    assert!(meter["value"].get("value").is_none());
    assert_eq!(meter["value"]["quality"]["level"], "uncertain");
    assert_eq!(meter["value"]["quality"]["reason"], "meter.not_reported");
    // A station-level meter belongs to the station itself, not to a connector.
    assert!(meter["resource"].get("resource").is_none());
}

#[tokio::test]
async fn both_ocpp_resource_models_keep_their_own_identity() {
    let (_, body) = get(
        router(),
        "/bridge/v1/points?station_id=station-a&connector_id=1",
        Some(READER_TOKEN),
    )
    .await;
    let connector = point(&body, "energy.active.import.register");
    assert_eq!(connector["resource"]["resource"]["connector_id"], "1");
    assert!(connector["resource"]["resource"].get("evse_id").is_none());

    let (_, body) = get(
        router(),
        "/bridge/v1/points?station_id=station-b&evse_id=1",
        Some(READER_TOKEN),
    )
    .await;
    let evse = point(&body, "energy.active.import.register");
    assert_eq!(evse["resource"]["resource"]["evse_id"], "1");
    assert_eq!(evse["resource"]["resource"]["connector_id"], "1");
}

#[tokio::test]
async fn a_bounded_point_page_resumes_inside_one_station() {
    let (status, body) = get(
        single_station_router(),
        "/bridge/v1/points?limit=1",
        Some(READER_TOKEN),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["items"].as_array().expect("page").len(), 1);
    let first = body["items"][0]["point_id"].as_str().expect("point id");
    let cursor = body["next_cursor"].as_str().expect("next cursor");

    let (status, body) = get(
        single_station_router(),
        &format!("/bridge/v1/points?limit=1&after={cursor}"),
        Some(READER_TOKEN),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let second = body["items"][0]["point_id"].as_str().expect("point id");
    assert_ne!(first, second);
    // Both points of the one station have now been delivered exactly once.
    assert!(body["next_cursor"].is_null(), "{body}");
}

#[tokio::test]
async fn a_station_filtered_page_resumes_without_repeating_or_dropping_a_point() {
    let mut seen = Vec::new();
    let mut cursor: Option<String> = None;
    for _ in 0..8 {
        let path = cursor.as_ref().map_or_else(
            || "/bridge/v1/points?station_id=station-a&limit=1".to_owned(),
            |cursor| format!("/bridge/v1/points?station_id=station-a&limit=1&after={cursor}"),
        );
        let (status, body) = get(router(), &path, Some(READER_TOKEN)).await;
        assert_eq!(status, StatusCode::OK);
        for item in body["items"].as_array().expect("page") {
            assert_eq!(item["resource"]["station_id"], "station-a", "{item}");
            seen.push(item["point_id"].as_str().expect("point").to_owned());
        }
        match body["next_cursor"].as_str() {
            Some(next) => cursor = Some(next.to_owned()),
            None => break,
        }
    }

    assert_eq!(
        seen,
        [
            "station.meter.active.import",
            "energy.active.import.register"
        ]
    );
}

#[tokio::test]
async fn a_point_page_walks_every_station_the_caller_can_read() {
    let mut seen = Vec::new();
    let mut cursor: Option<String> = None;
    for _ in 0..8 {
        let path = cursor.as_ref().map_or_else(
            || "/bridge/v1/points?limit=1".to_owned(),
            |cursor| format!("/bridge/v1/points?limit=1&after={cursor}"),
        );
        let (status, body) = get(router(), &path, Some(READER_TOKEN)).await;
        assert_eq!(status, StatusCode::OK);
        for item in body["items"].as_array().expect("page") {
            seen.push(format!(
                "{}/{}",
                item["resource"]["station_id"].as_str().expect("station"),
                item["point_id"].as_str().expect("point")
            ));
        }
        match body["next_cursor"].as_str() {
            Some(next) => cursor = Some(next.to_owned()),
            None => break,
        }
    }

    assert_eq!(
        seen,
        [
            // A station's own meters are listed before the points of its connectors.
            "station-a/station.meter.active.import",
            "station-a/energy.active.import.register",
            "station-b/energy.active.import.register",
            "station-unscoped/station.meter.active.import",
        ]
    );
}

#[tokio::test]
async fn a_point_page_never_leaves_the_callers_own_scope() {
    let (status, body) = get(router(), "/bridge/v1/points", Some(STATION_SCOPED_TOKEN)).await;
    assert_eq!(status, StatusCode::OK);
    for item in body["items"].as_array().expect("page") {
        assert_eq!(item["resource"]["station_id"], "station-a", "{item}");
    }

    // A station outside the scope, one granted but empty, and one that does not exist are all
    // indistinguishable to a caller holding nothing inside them.
    for station_id in ["station-b", "station-empty", "station-absent"] {
        let (status, body) = get(
            router(),
            &format!("/bridge/v1/points?station_id={station_id}"),
            Some(STATION_SCOPED_TOKEN),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{station_id}");
        assert_eq!(body["items"], serde_json::json!([]), "{station_id}");
    }
}

#[tokio::test]
async fn one_point_is_resolved_through_the_hosts_explicit_descriptor_and_value_queries() {
    let (status, body) = get(
        router(),
        "/bridge/v1/points/energy.active.import.register?station_id=station-b&evse_id=1&connector_id=1",
        Some(READER_TOKEN),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["point_id"], "energy.active.import.register");
    assert_eq!(
        body["descriptor"]["semantic_name"],
        "energy.active.import.register.v1"
    );
    assert_eq!(body["value"]["value"]["value"], "123456.789");
    assert_eq!(body["resource"]["resource"]["evse_id"], "1");
}

#[tokio::test]
async fn an_unknown_point_inside_the_scope_is_reported_as_a_missing_resource() {
    let (status, body) = get(
        router(),
        "/bridge/v1/points/nothing.here?station_id=station-a&connector_id=1",
        Some(READER_TOKEN),
    )
    .await;

    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["error"], "ems_scada_http.resource_not_found");
}

#[tokio::test]
async fn a_point_outside_the_callers_scope_is_refused_before_the_source_is_read() {
    let (status, body) = get(
        router(),
        "/bridge/v1/points/energy.active.import.register?station_id=station-b&evse_id=1&connector_id=1",
        Some(STATION_SCOPED_TOKEN),
    )
    .await;

    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["error"], "ems_scada_http.permission_denied");
}

#[tokio::test]
async fn a_point_request_states_which_station_and_page_it_means() {
    for path in [
        // An EVSE or connector filter is meaningless without a station.
        "/bridge/v1/points?evse_id=1",
        // The item resource is addressed inside one station.
        "/bridge/v1/points/energy.active.import.register",
        // A station-page cursor is not a point-page position.
        "/bridge/v1/points?after=snapshot:1",
        "/bridge/v1/points?limit=0",
        "/bridge/v1/points?unknown=1",
    ] {
        let (status, body) = get(router(), path, Some(READER_TOKEN)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{path}");
        assert_eq!(body["error"], "ems_scada_http.invalid_request", "{path}");
    }
}

#[tokio::test]
async fn a_credential_without_the_reader_role_reads_no_points() {
    for path in [
        "/bridge/v1/points",
        "/bridge/v1/points/energy.active.import.register?station_id=station-a",
    ] {
        let (status, body) = get(router(), path, Some(CONTROLLER_TOKEN)).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{path}");
        assert_eq!(body["error"], "ems_scada_http.permission_denied", "{path}");
    }
}
