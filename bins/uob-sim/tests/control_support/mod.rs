#![allow(dead_code)]
use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use serde_json::{Value, json};
use std::{
    fs,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};
use tower::ServiceExt;
use uob_sim::{
    control::{ControlConfiguration, ControlServer},
    scenario::ScenarioRunner,
};

pub const TOKEN: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
pub const HOST: &str = "127.0.0.1:9001";
static NEXT: AtomicU64 = AtomicU64::new(0);

pub struct Fixture {
    pub directory: PathBuf,
}
impl Fixture {
    pub fn new(scenario: &str) -> Self {
        let directory = std::env::temp_dir().join(format!(
            "uob-control-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&directory).unwrap();
        let fixture = Self { directory };
        fixture.write("token", TOKEN);
        fixture.write("simulator.toml", &configuration("ws://127.0.0.1:19000"));
        fixture.write("scenario.toml", scenario);
        fixture.write("control.toml", &control_document("demo"));
        fixture
    }
    pub fn write(&self, name: &str, content: &str) {
        fs::write(self.directory.join(name), content).unwrap();
    }
    pub fn load(&self) -> Result<ControlConfiguration, &'static str> {
        ControlConfiguration::load(&self.directory.join("control.toml"), HOST.parse().unwrap())
    }
    pub fn server(&self) -> ControlServer {
        ControlServer::new(self.load().unwrap(), ScenarioRunner::default())
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.directory);
    }
}

pub fn control_document(environment: &str) -> String {
    format!(
        r#"schema_version = 1
environment = "{environment}"
token_file = "token"
simulator_file = "simulator.toml"
[[scenarios]]
id = "sample"
path = "scenario.toml"
"#
    )
}

pub fn configuration(endpoint: &str) -> String {
    format!(
        r#"schema_version = 1
[[stations]]
id = "demo-alpha"
endpoint = "{endpoint}/demo-alpha"
ocpp_version = "1.6"
[[stations]]
id = "demo-beta"
endpoint = "{endpoint}/demo-beta"
ocpp_version = "2.0.1"
"#
    )
}

pub fn wait_scenario(station: &str, duration: u64) -> String {
    format!(
        r#"schema_version = 1
seed = 42
[[steps]]
id = "wait"
station = "{station}"
action = "wait"
duration_ms = {duration}
timeout_ms = 30000
expect_event = "delay_elapsed"
"#
    )
}

pub async fn request(
    router: &Router,
    method: &str,
    path: &str,
    input: Value,
) -> (StatusCode, Value) {
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method(method)
                .uri(path)
                .header("Host", HOST)
                .header("Authorization", format!("Bearer {TOKEN}"))
                .header("Content-Type", "application/json")
                .body(Body::from(input.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let body = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
    (status, serde_json::from_slice(&body).unwrap_or(Value::Null))
}

pub async fn start(router: &Router) -> u64 {
    let (status, value) =
        request(router, "POST", "/api/v1/runs", json!({"scenario":"sample"})).await;
    assert_eq!(status, StatusCode::OK, "{value}");
    value["run_id"].as_u64().unwrap()
}

pub async fn finished(router: &Router, id: u64) -> Value {
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let (_, value) =
                request(router, "GET", &format!("/api/v1/runs/{id}"), Value::Null).await;
            if matches!(value["status"].as_str(), Some("passed" | "failed")) {
                return value;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap()
}
