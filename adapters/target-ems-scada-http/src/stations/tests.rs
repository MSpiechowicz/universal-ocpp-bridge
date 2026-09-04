use axum::http::StatusCode;

use crate::test_support::{
    CONTROLLER_TOKEN, CanonicalFixtures, MULTI_BRIDGE_TOKEN, READER_TOKEN, STATION_SCOPED_TOKEN,
    get, router_with, scoped_credentials,
};

fn router() -> axum::Router {
    router_with(
        scoped_credentials(),
        CanonicalFixtures::both_resource_models(),
        16,
    )
}

#[tokio::test]
async fn a_bridge_scoped_reader_sees_every_station_inside_its_own_scope() {
    let (status, body) = get(router(), "/bridge/v1/stations", Some(READER_TOKEN)).await;

    assert_eq!(status, StatusCode::OK);
    let items = body["items"].as_array().expect("station page");
    let stations: Vec<&str> = items
        .iter()
        .map(|item| item["station"]["station_id"].as_str().expect("station id"))
        .collect();
    assert_eq!(stations, ["station-a", "station-b", "station-unscoped"]);
}

#[tokio::test]
async fn enumeration_hides_every_station_outside_the_callers_own_scope() {
    let (status, body) = get(router(), "/bridge/v1/stations", Some(STATION_SCOPED_TOKEN)).await;

    assert_eq!(status, StatusCode::OK);
    let items = body["items"].as_array().expect("station page");
    let stations: Vec<&str> = items
        .iter()
        .map(|item| item["station"]["station_id"].as_str().expect("station id"))
        .collect();
    // The configured target instance may read all three; this credential may read one.
    assert_eq!(stations, ["station-a"]);
}

#[tokio::test]
async fn direct_identifier_access_outside_the_callers_scope_is_refused_the_same_way() {
    for station_id in ["station-b", "station-unscoped", "station-absent"] {
        let (status, body) = get(
            router(),
            &format!("/bridge/v1/stations/{station_id}"),
            Some(STATION_SCOPED_TOKEN),
        )
        .await;

        assert_eq!(status, StatusCode::FORBIDDEN, "{station_id}");
        assert_eq!(
            body["error"], "ems_scada_http.permission_denied",
            "{station_id}"
        );
    }
}

#[tokio::test]
async fn an_in_scope_station_that_does_not_exist_is_reported_as_a_missing_resource() {
    // `station-empty` is inside the configured target's grant and this credential's bridge, but
    // no snapshot exists for it.
    let (status, body) = get(
        router(),
        "/bridge/v1/stations/station-empty",
        Some(READER_TOKEN),
    )
    .await;

    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["error"], "ems_scada_http.resource_not_found");
}

#[tokio::test]
async fn one_station_is_served_as_the_canonical_snapshot_itself() {
    let (status, body) = get(
        router(),
        "/bridge/v1/stations/station-b",
        Some(READER_TOKEN),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["station"]["station_id"], "station-b");
    assert_eq!(body["schema_version"]["major"], 1);
    // EVSE identity and the retained native address both survive the canonical rendering.
    let resource = &body["resources"][0]["resource"];
    assert_eq!(resource["resource"]["evse_id"], "1");
    assert_eq!(resource["resource"]["connector_id"], "1");
    assert_eq!(resource["native_protocol_reference"]["protocol"], "ocpp201");
}

#[tokio::test]
async fn a_page_limit_above_the_application_maximum_cannot_be_requested() {
    let maximum = crate::request::maximum_page_size();
    for limit in [0, maximum + 1, u16::MAX] {
        let (status, body) = get(
            router(),
            &format!("/bridge/v1/stations?limit={limit}"),
            Some(READER_TOKEN),
        )
        .await;

        assert_eq!(status, StatusCode::BAD_REQUEST, "limit {limit}");
        assert_eq!(
            body["error"], "ems_scada_http.invalid_request",
            "limit {limit}"
        );
    }

    let (status, body) = get(
        router(),
        &format!("/bridge/v1/stations?limit={maximum}"),
        Some(READER_TOKEN),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(body["items"].as_array().expect("station page").len() <= usize::from(maximum));
}

#[tokio::test]
async fn a_bounded_page_is_resumed_only_through_its_own_cursor() {
    let (status, body) = get(router(), "/bridge/v1/stations?limit=1", Some(READER_TOKEN)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["items"].as_array().expect("page").len(), 1);
    let cursor = body["next_cursor"].as_str().expect("next cursor");

    let (status, body) = get(
        router(),
        &format!("/bridge/v1/stations?limit=1&after={cursor}"),
        Some(READER_TOKEN),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["items"][0]["station"]["station_id"], "station-b");

    let (status, body) = get(
        router(),
        "/bridge/v1/stations?after=uob:point:0:",
        Some(READER_TOKEN),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"], "ems_scada_http.invalid_request");
}

#[tokio::test]
async fn a_credential_without_the_reader_role_reads_no_canonical_state() {
    for path in ["/bridge/v1/stations", "/bridge/v1/stations/station-a"] {
        let (status, body) = get(router(), path, Some(CONTROLLER_TOKEN)).await;

        assert_eq!(status, StatusCode::FORBIDDEN, "{path}");
        assert_eq!(body["error"], "ems_scada_http.permission_denied", "{path}");
    }
}

#[tokio::test]
async fn a_credential_spanning_several_bridges_must_name_the_one_it_means() {
    let (status, body) = get(
        router(),
        "/bridge/v1/stations/station-a",
        Some(MULTI_BRIDGE_TOKEN),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"], "ems_scada_http.bridge_required");

    let (status, body) = get(
        router(),
        "/bridge/v1/stations/station-a?bridge_id=site-01",
        Some(MULTI_BRIDGE_TOKEN),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["station"]["bridge_id"], "site-01");
}

#[tokio::test]
async fn a_station_snapshot_is_not_narrowed_by_a_point_filter() {
    let (status, body) = get(
        router(),
        "/bridge/v1/stations/station-a?connector_id=1",
        Some(READER_TOKEN),
    )
    .await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"], "ems_scada_http.invalid_request");
}
