//! Separately enabled, read-only browser evidence. No control-token fallback.
use super::{ControlServer, REQUEST_TIMEOUT};
use axum::{
    Json,
    extract::{Path, Request, State},
    http::{Method, StatusCode, header},
    middleware::Next,
    response::{IntoResponse, Response},
};
use serde_json::json;
use subtle::ConstantTimeEq;

pub(super) async fn guard(server: ControlServer, request: Request, next: Next) -> Response {
    let Some(access) = &server.configuration.debug else {
        return StatusCode::NOT_FOUND.into_response();
    };
    if request
        .headers()
        .get(header::ORIGIN)
        .and_then(|value| value.to_str().ok())
        != Some(access.origin.as_str())
    {
        return StatusCode::FORBIDDEN.into_response();
    }
    let Ok(_permit) = server.requests.clone().try_acquire_owned() else {
        return StatusCode::TOO_MANY_REQUESTS.into_response();
    };
    let mut response = if request.method() == Method::OPTIONS {
        let method = request.headers().get(header::ACCESS_CONTROL_REQUEST_METHOD);
        let headers = request
            .headers()
            .get(header::ACCESS_CONTROL_REQUEST_HEADERS);
        if method.and_then(|v| v.to_str().ok()) != Some("GET")
            || !headers
                .and_then(|v| v.to_str().ok())
                .is_some_and(|v| v.eq_ignore_ascii_case("authorization"))
        {
            return StatusCode::FORBIDDEN.into_response();
        }
        StatusCode::NO_CONTENT.into_response()
    } else if request.method() == Method::GET {
        let token = request
            .headers()
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "))
            .unwrap_or("");
        if bool::from(token.as_bytes().ct_eq(access.token.as_bytes())) {
            tokio::time::timeout(REQUEST_TIMEOUT, next.run(request))
                .await
                .unwrap_or_else(|_| StatusCode::REQUEST_TIMEOUT.into_response())
        } else {
            StatusCode::UNAUTHORIZED.into_response()
        }
    } else {
        StatusCode::METHOD_NOT_ALLOWED.into_response()
    };
    let headers = response.headers_mut();
    headers.insert(
        header::ACCESS_CONTROL_ALLOW_ORIGIN,
        access.origin.parse().expect("validated origin"),
    );
    headers.insert(
        header::ACCESS_CONTROL_ALLOW_METHODS,
        "GET".parse().expect("static header"),
    );
    headers.insert(
        header::ACCESS_CONTROL_ALLOW_HEADERS,
        "Authorization".parse().expect("static header"),
    );
    headers.insert(header::VARY, "Origin".parse().expect("static header"));
    headers.insert(
        header::CACHE_CONTROL,
        "no-store".parse().expect("static header"),
    );
    response
}

pub(super) async fn status(State(server): State<ControlServer>, Path(id): Path<u64>) -> Response {
    let runs = server.runs.lock().expect("runs lock");
    let Some(run) = runs.entries.get(&id) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let mut evidence = run.status(id, &server.configuration.environment);
    evidence["schema_version"] = json!(1);
    Json(evidence).into_response()
}
