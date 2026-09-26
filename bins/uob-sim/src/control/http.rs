use super::{ControlServer, REQUEST_TIMEOUT, configuration::DOCUMENT_LIMIT};
use crate::scenario::Intervention;
use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, Path, Request, State},
    http::{Method, StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde::Deserialize;
use serde_json::{Value, json};
use subtle::ConstantTimeEq;

type ApiResult = Result<Json<Value>, (StatusCode, Json<Value>)>;
const ALLOW_HEADERS: &str = "Authorization, Content-Type";

pub(super) fn router(server: ControlServer) -> Router {
    Router::new()
        .route("/api/v1/debug/runs/{id}", get(super::debug::status))
        .route("/api/v1/scenarios", get(catalog))
        .route("/api/v1/runs", get(list).post(start))
        .route("/api/v1/runs/{id}", get(status).delete(remove))
        .route("/api/v1/runs/{id}/stop", post(stop))
        .route("/api/v1/runs/{id}/controls", post(control))
        .layer(DefaultBodyLimit::max(DOCUMENT_LIMIT))
        .layer(middleware::from_fn_with_state(server.clone(), guard))
        .with_state(server)
}

async fn guard(State(server): State<ControlServer>, request: Request, next: Next) -> Response {
    let expected_host = server.configuration.bind.to_string();
    if request
        .headers()
        .get(header::HOST)
        .and_then(|value| value.to_str().ok())
        != Some(expected_host.as_str())
    {
        return StatusCode::FORBIDDEN.into_response();
    }
    if request.uri().path().starts_with("/api/v1/debug/") {
        return super::debug::guard(server, request, next).await;
    }

    let origin = request
        .headers()
        .get(header::ORIGIN)
        .and_then(|value| value.to_str().ok());
    let browser = server
        .configuration
        .browser
        .as_ref()
        .map(|access| access.origin.as_str());
    let browser_origin = origin.is_some() && origin == browser;
    let same_listener_origin =
        origin.and_then(|value| value.strip_prefix("http://")) == Some(expected_host.as_str());
    if request.headers().contains_key(header::ORIGIN) && !same_listener_origin && !browser_origin {
        return StatusCode::FORBIDDEN.into_response();
    }
    if request.headers().contains_key(header::COOKIE) {
        return StatusCode::FORBIDDEN.into_response();
    }
    let methods = route_methods(request.uri().path());
    let is_preflight = request.method() == Method::OPTIONS;
    let cors_route = if is_preflight {
        let requested_method = request
            .headers()
            .get(header::ACCESS_CONTROL_REQUEST_METHOD)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.parse::<Method>().ok());
        let requested_headers = request
            .headers()
            .get(header::ACCESS_CONTROL_REQUEST_HEADERS)
            .and_then(|value| value.to_str().ok());
        requested_method
            .as_ref()
            .is_some_and(|method| allowed_route(request.uri().path(), method))
            && requested_headers.is_some_and(valid_preflight_headers)
    } else {
        allowed_route(request.uri().path(), request.method())
    };
    let mut response = if is_preflight {
        if browser_origin && cors_route {
            StatusCode::NO_CONTENT.into_response()
        } else {
            StatusCode::FORBIDDEN.into_response()
        }
    } else {
        let token = request
            .headers()
            .get(header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.strip_prefix("Bearer "))
            .unwrap_or("");
        if !bool::from(
            token
                .as_bytes()
                .ct_eq(server.configuration.token.as_bytes()),
        ) {
            StatusCode::UNAUTHORIZED.into_response()
        } else if !cors_route {
            // Axum still owns the response for unknown paths/methods.
            next.run(request).await
        } else if let Ok(_permit) = server.requests.clone().try_acquire_owned() {
            tokio::time::timeout(REQUEST_TIMEOUT, next.run(request))
                .await
                .unwrap_or_else(|_| StatusCode::REQUEST_TIMEOUT.into_response())
        } else {
            StatusCode::TOO_MANY_REQUESTS.into_response()
        }
    };
    decorate_response(&mut response, browser_origin, cors_route, browser, methods);
    response
}

fn decorate_response(
    response: &mut Response,
    browser_origin: bool,
    cors_route: bool,
    browser: Option<&str>,
    methods: Option<&str>,
) {
    let headers = response.headers_mut();
    headers.insert(
        header::CACHE_CONTROL,
        "no-store".parse().expect("static header"),
    );
    if browser_origin && cors_route {
        headers.insert(
            header::ACCESS_CONTROL_ALLOW_ORIGIN,
            browser
                .expect("matched browser origin")
                .parse()
                .expect("validated origin"),
        );
        headers.insert(
            header::ACCESS_CONTROL_ALLOW_METHODS,
            methods
                .expect("allowed route")
                .parse()
                .expect("static header"),
        );
        headers.insert(
            header::ACCESS_CONTROL_ALLOW_HEADERS,
            ALLOW_HEADERS.parse().expect("static header"),
        );
        headers.insert(header::VARY, "Origin".parse().expect("static header"));
    }
}

fn route_methods(path: &str) -> Option<&'static str> {
    if path == "/api/v1/scenarios" {
        return Some("GET");
    }
    if path == "/api/v1/runs" {
        return Some("GET, POST");
    }
    let rest = path.strip_prefix("/api/v1/runs/")?;
    let (id, suffix) = rest.split_once('/').unwrap_or((rest, ""));
    parse_id(id)?;
    match suffix {
        "" => Some("GET, DELETE"),
        "stop" | "controls" => Some("POST"),
        _ => None,
    }
}

fn allowed_route(path: &str, method: &Method) -> bool {
    route_methods(path).is_some_and(|methods| {
        methods
            .split(", ")
            .any(|allowed| allowed == method.as_str())
    })
}

fn valid_preflight_headers(value: &str) -> bool {
    let mut authorization = false;
    let mut content_type = false;
    for name in value.split(',').map(str::trim) {
        if name.eq_ignore_ascii_case("authorization") && !authorization {
            authorization = true;
        } else if name.eq_ignore_ascii_case("content-type") && !content_type {
            content_type = true;
        } else {
            return false;
        }
    }
    authorization
}

fn parse_id(value: &str) -> Option<u64> {
    if value.is_empty()
        || (value.len() > 1 && value.starts_with('0'))
        || !value.bytes().all(|byte| byte.is_ascii_digit())
    {
        return None;
    }
    value.parse().ok()
}

async fn catalog(State(server): State<ControlServer>) -> Json<Value> {
    let scenarios: Vec<_> = server
        .configuration
        .scenarios
        .iter()
        .map(|(id, scenario)| {
            let stations: std::collections::BTreeSet<_> =
                scenario.steps.iter().map(|step| &step.station).collect();
            let steps: Vec<_> = scenario.steps.iter().map(|step| json!({
            "step_id": step.id, "station_id": step.station, "action": step.action.name(),
            "eligible_controls": step.eligible_controls(),
            "response_delay_scope": step.response_delay_scope(),
        })).collect();
            json!({ "id": id, "seed": scenario.seed.to_string(), "stations": stations,
            "steps": steps })
        })
        .collect();
    Json(json!({ "environment": server.configuration.environment, "scenarios": scenarios }))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Start {
    scenario: String,
    seed: Option<String>,
}

async fn start(State(server): State<ControlServer>, Json(input): Json<Start>) -> ApiResult {
    let seed = input
        .seed
        .as_deref()
        .map(|value| {
            parse_id(value).ok_or_else(|| {
                (
                    StatusCode::UNPROCESSABLE_ENTITY,
                    Json(json!({"error":"invalid_seed"})),
                )
            })
        })
        .transpose()?;
    let id = server.start(&input.scenario, seed).map_err(conflict)?;
    Ok(Json(json!({ "run_id": id.to_string() })))
}

async fn list(State(server): State<ControlServer>) -> Json<Value> {
    let runs = server.runs.lock().expect("runs lock");
    let entries: Vec<_> = runs
        .entries
        .iter()
        .map(|(id, run)| {
            json!({
                "run_id": id.to_string(), "scenario": run.scenario, "seed": run.seed.to_string(),
                "terminal": run.report.is_some(),
            })
        })
        .collect();
    Json(json!({ "environment": server.configuration.environment, "runs": entries }))
}

async fn status(State(server): State<ControlServer>, Path(id): Path<String>) -> ApiResult {
    let id = path_id(&id)?;
    let runs = server.runs.lock().expect("runs lock");
    let run = runs.entries.get(&id).ok_or_else(not_found)?;
    Ok(Json(run.status(id, &server.configuration.environment)))
}

async fn stop(State(server): State<ControlServer>, Path(id): Path<String>) -> ApiResult {
    let id = path_id(&id)?;
    let mut runs = server.runs.lock().expect("runs lock");
    let run = runs.entries.get_mut(&id).ok_or_else(not_found)?;
    run.stop.cancel();
    run.stopping = true;
    Ok(Json(
        json!({ "run_id": id.to_string(), "stop_requested": true }),
    ))
}

async fn remove(State(server): State<ControlServer>, Path(id): Path<String>) -> ApiResult {
    let id = path_id(&id)?;
    let mut runs = server.runs.lock().expect("runs lock");
    let run = runs.entries.get(&id).ok_or_else(not_found)?;
    if run.report.is_none() {
        return Err(conflict("run_active"));
    }
    runs.entries.remove(&id);
    Ok(Json(json!({ "removed": id.to_string() })))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Control {
    step_id: String,
    intervention: Intervention,
}

async fn control(
    State(server): State<ControlServer>,
    Path(id): Path<String>,
    Json(input): Json<Control>,
) -> ApiResult {
    let id = path_id(&id)?;
    let runs = server.runs.lock().expect("runs lock");
    if runs.stopping {
        return Err(conflict("server_stopping"));
    }
    let run = runs.entries.get(&id).ok_or_else(not_found)?;
    if run.report.is_some() || run.stopping {
        return Err(conflict("run_not_active"));
    }
    run.live
        .intervene(&input.step_id, input.intervention)
        .map_err(conflict)?;
    Ok(Json(
        json!({ "run_id": id.to_string(), "step_id": input.step_id, "status": "scheduled" }),
    ))
}

fn path_id(value: &str) -> Result<u64, (StatusCode, Json<Value>)> {
    parse_id(value).ok_or_else(|| {
        (
            StatusCode::BAD_REQUEST,
            Json(json!({"error":"invalid_run_id"})),
        )
    })
}

fn conflict(code: &'static str) -> (StatusCode, Json<Value>) {
    (StatusCode::CONFLICT, Json(json!({ "error": code })))
}
fn not_found() -> (StatusCode, Json<Value>) {
    (
        StatusCode::NOT_FOUND,
        Json(json!({ "error": "unknown_run" })),
    )
}
