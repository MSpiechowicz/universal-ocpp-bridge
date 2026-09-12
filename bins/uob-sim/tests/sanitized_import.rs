mod control_support;
use axum::http::StatusCode;
use control_support::{Fixture, configuration, control_document, request};
use serde_json::{Value, json};

fn valid() -> Value {
    json!({"schema_version":1,"kind":"sanitized_capture","records":[
        {"station_slot":0,"connector_slot":0,"status":"Available"}
    ]})
}
fn fixture(value: &Value) -> Fixture {
    let fixture = Fixture::new(&value.to_string());
    fixture.write(
        "simulator.toml",
        &configuration("ws://127.0.0.1:19000").replace("demo-", "staging-"),
    );
    fixture.write(
        "control.toml",
        &(control_document("staging") + "sanitized_import = true\n"),
    );
    fixture
}

#[test]
fn accepts_only_bounded_identity_free_projection() {
    assert!(fixture(&valid()).load().is_ok());
    for kind in ["sanitized_capture", "sanitized_snapshot"] {
        let mut value = valid();
        value["kind"] = json!(kind);
        assert!(fixture(&value).load().is_ok());
    }
    for key in [
        "bridge_id",
        "station_id",
        "environment",
        "credentials",
        "pending_exports",
        "outbox",
        "payment",
        "customer",
        "source_identity",
    ] {
        let mut value = valid();
        value[key] = json!("production-sensitive");
        assert!(fixture(&value).load().is_err(), "{key}");
        let mut value = valid();
        value["records"][0][key] = json!("production-sensitive");
        assert!(fixture(&value).load().is_err(), "nested {key}");
    }
    for (key, bad) in [
        ("station_slot", json!("production")),
        ("station_slot", json!(16)),
        ("connector_slot", json!(16)),
        ("status", json!("<script>credential</script>")),
        ("status", json!("Charging")),
    ] {
        let mut value = valid();
        value["records"][0][key] = bad;
        assert!(fixture(&value).load().is_err());
    }
    for count in [0, 33] {
        let mut value = valid();
        value["records"] = json!(vec![value["records"][0].clone(); count]);
        assert!(fixture(&value).load().is_err());
    }
    let mut value = valid();
    value["schema_version"] = json!(2);
    assert!(fixture(&value).load().is_err());
    let mut value = valid();
    value["kind"] = json!("sanitized_snapshot");
    value["records"] = json!(vec![value["records"][0].clone(); 2]);
    assert!(fixture(&value).load().is_err());
    for raw in [
        "SQLite format 3\0",
        "{",
        &" ".repeat(65537),
        r#"{"schema_version":1,"schema_version":1}"#,
    ] {
        let fixture = fixture(&valid());
        fixture.write("scenario.toml", raw);
        assert!(fixture.load().is_err());
    }
}

#[test]
fn imports_require_staging_and_explicit_synthetic_peers_without_credentials() {
    for environment in ["production", "demo"] {
        let fixture = fixture(&valid());
        fixture.write(
            "control.toml",
            &(control_document(environment) + "sanitized_import = true\n"),
        );
        assert!(fixture.load().is_err());
    }
    for endpoint in [
        "ws://192.0.2.1:19000",
        "ws://localhost:19000",
        "ws://secret@127.0.0.1:19000",
    ] {
        let fixture = fixture(&valid());
        fixture.write(
            "simulator.toml",
            &configuration(endpoint).replace("demo-", "staging-"),
        );
        assert!(fixture.load().is_err());
    }
    let fixture = fixture(&valid());
    fixture.write(
        "simulator.toml",
        &(configuration("ws://127.0.0.1:19000").replace("demo-", "staging-")
            + "credentials_file = '/etc/uob/production-secret'\n"),
    );
    assert!(fixture.load().is_err());
}

#[tokio::test]
async fn host_namespace_rejects_replay_before_connecting() {
    let fixture = fixture(&valid());
    let router = fixture.server().router();
    let (status, body) = request(
        &router,
        "POST",
        "/api/v1/runs",
        json!({"scenario":"sample"}),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["error"], "import_requires_isolated_staging_network");
    let (_, body) = request(&router, "GET", "/api/v1/runs", Value::Null).await;
    assert_eq!(body["runs"], json!([]));
}
