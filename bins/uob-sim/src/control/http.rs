use super::{ControlServer, REQUEST_TIMEOUT, configuration::DOCUMENT_LIMIT};
use crate::scenario::Intervention;
use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, Path, Request, State},
    http::{StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde::Deserialize;
use serde_json::{Value, json};
use subtle::ConstantTimeEq;

type ApiResult = Result<Json<Value>, (StatusCode, Json<Value>)>;

pub(super) fn router(server: ControlServer) -> Router {
    Router::new()
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
    // No cookies or ambient browser authentication. A future console proxy must explicitly
    // inject this separate simulator token; arbitrary cross-origin browser access stays denied.
    if let Some(origin) = request.headers().get(header::ORIGIN)
        && origin.to_str().ok() != Some(format!("http://{expected_host}").as_str())
    {
        return StatusCode::FORBIDDEN.into_response();
    }
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
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let Ok(_permit) = server.requests.clone().try_acquire_owned() else {
        return StatusCode::TOO_MANY_REQUESTS.into_response();
    };
    let mut response = tokio::time::timeout(REQUEST_TIMEOUT, next.run(request))
        .await
        .unwrap_or_else(|_| StatusCode::REQUEST_TIMEOUT.into_response());
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        "no-store".parse().expect("static header"),
    );
    response
}

async fn catalog(State(server): State<ControlServer>) -> Json<Value> {
    Json(json!({ "environment": server.configuration.environment,
        "scenarios": server.configuration.scenarios.keys().collect::<Vec<_>>() }))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Start {
    scenario: String,
    seed: Option<u64>,
}

async fn start(State(server): State<ControlServer>, Json(input): Json<Start>) -> ApiResult {
    let id = server
        .start(&input.scenario, input.seed)
        .map_err(conflict)?;
    Ok(Json(json!({ "run_id": id })))
}

async fn list(State(server): State<ControlServer>) -> Json<Value> {
    let runs = server.runs.lock().expect("runs lock");
    let entries: Vec<_> = runs
        .entries
        .iter()
        .map(|(id, run)| {
            json!({
                "run_id": id, "scenario": run.scenario, "seed": run.seed,
                "terminal": run.report.is_some(),
            })
        })
        .collect();
    Json(json!({ "environment": server.configuration.environment, "runs": entries }))
}

async fn status(State(server): State<ControlServer>, Path(id): Path<u64>) -> ApiResult {
    let runs = server.runs.lock().expect("runs lock");
    let run = runs.entries.get(&id).ok_or_else(not_found)?;
    Ok(Json(run.status(id, &server.configuration.environment)))
}

async fn stop(State(server): State<ControlServer>, Path(id): Path<u64>) -> ApiResult {
    let mut runs = server.runs.lock().expect("runs lock");
    let run = runs.entries.get_mut(&id).ok_or_else(not_found)?;
    run.stop.cancel();
    run.stopping = true;
    Ok(Json(json!({ "run_id": id, "stop_requested": true })))
}

async fn remove(State(server): State<ControlServer>, Path(id): Path<u64>) -> ApiResult {
    let mut runs = server.runs.lock().expect("runs lock");
    let run = runs.entries.get(&id).ok_or_else(not_found)?;
    if run.report.is_none() {
        return Err(conflict("run_active"));
    }
    runs.entries.remove(&id);
    Ok(Json(json!({ "removed": id })))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Control {
    step_id: String,
    intervention: Intervention,
}

async fn control(
    State(server): State<ControlServer>,
    Path(id): Path<u64>,
    Json(input): Json<Control>,
) -> ApiResult {
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
        json!({ "run_id": id, "step_id": input.step_id, "status": "scheduled" }),
    ))
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
