use super::*;

#[tokio::test]
async fn command_status_requires_independent_station_read_grant() {
    let state = Arc::new(CommandState::default());
    let app = router(state.clone());
    let accepted = app
        .clone()
        .oneshot(
            Request::post("/api/v1/commands")
                .header(header::AUTHORIZATION, "Bearer operator-secret")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(serde_json::to_vec(&request()).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(accepted.status(), StatusCode::ACCEPTED);

    for (authorization, expected) in [
        (None, StatusCode::UNAUTHORIZED),
        (Some("Bearer wrong-secret"), StatusCode::UNAUTHORIZED),
        (Some("Bearer other-secret"), StatusCode::UNAUTHORIZED),
        (Some("Bearer operator-secret"), StatusCode::UNAUTHORIZED),
        (Some("Bearer reader-secret"), StatusCode::OK),
    ] {
        let mut builder = Request::get("/api/v1/commands/request-a");
        if let Some(value) = authorization {
            builder = builder.header(header::AUTHORIZATION, value);
        }
        let response = app
            .clone()
            .oneshot(builder.body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), expected, "{authorization:?}");
    }

    {
        let mut result = state.result.lock();
        result.as_mut().unwrap().resource.station_id = StationId::new("station-b").unwrap();
    }
    let response = app
        .clone()
        .oneshot(
            Request::get("/api/v1/commands/request-a")
                .header(header::AUTHORIZATION, "Bearer reader-secret")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    let foreign_body = to_bytes(response.into_body(), 65_536).await.unwrap();
    let missing = app
        .oneshot(
            Request::get("/api/v1/commands/never-submitted")
                .header(header::AUTHORIZATION, "Bearer reader-secret")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(missing.status(), StatusCode::NOT_FOUND);
    assert_eq!(
        foreign_body,
        to_bytes(missing.into_body(), 65_536).await.unwrap()
    );
}
