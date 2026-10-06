use std::{
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    sync::{Arc, Mutex},
};

use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Request, StatusCode, header},
};
use tower::ServiceExt;
use uob_application::{
    DeliveryDispositionRecord, DeliveryDispositionRequest, DeliveryDispositionState, StorageError,
    StorageErrorCode, StorageFuture, TargetBacklogFact, TargetDeliveryDestination,
    TargetDispositionStore,
};
use uob_contracts::{BridgeId, Environment, TargetInstanceId, UtcTimestamp};

use super::{Configuration, start};

const READER: &str = "uob1.demo.readerreaderreaderreaderreader00";
const WRITER: &str = "uob1.demo.writerwriterwriterwriterwriter00";
const ADMIN: &str = "uob1.demo.adminadminadminadminadminadmin00";

#[derive(Default)]
struct Store {
    backlog: Mutex<Vec<TargetBacklogFact>>,
    records: Mutex<Vec<DeliveryDispositionRecord>>,
    settled: Mutex<Vec<Option<TargetDeliveryDestination>>>,
}

impl TargetDispositionStore for Store {
    fn target_delivery_backlog(&self) -> StorageFuture<'_, Vec<TargetBacklogFact>> {
        let backlog = self.backlog.lock().unwrap().clone();
        Box::pin(async move { Ok(backlog) })
    }

    fn pending_target_dispositions(&self) -> StorageFuture<'_, Vec<DeliveryDispositionRecord>> {
        let records = self.records.lock().unwrap().clone();
        Box::pin(async move { Ok(records) })
    }

    fn authorize_target_disposition(
        &self,
        request: DeliveryDispositionRequest,
    ) -> StorageFuture<'_, DeliveryDispositionRecord> {
        let mut records = self.records.lock().unwrap();
        if records
            .iter()
            .any(|record| record.destination == request.destination)
        {
            return Box::pin(async {
                Err(StorageError::new(StorageErrorCode::Conflict, "duplicate"))
            });
        }
        let record = DeliveryDispositionRecord {
            audit_event_id: format!("disposition-{}", records.len() + 1),
            destination: request.destination,
            action: request.action,
            principal_id: request.principal_id,
            authorized_at: request.authorized_at,
            state: DeliveryDispositionState::Authorized,
            settled_at: None,
            critical_deliveries: None,
            deliveries: None,
        };
        records.push(record.clone());
        Box::pin(async move { Ok(record) })
    }

    fn settle_target_dispositions(
        &self,
        selected: Option<TargetDeliveryDestination>,
        _settled_at: UtcTimestamp,
    ) -> StorageFuture<'_, Vec<DeliveryDispositionRecord>> {
        self.settled.lock().unwrap().push(selected);
        Box::pin(async { Ok(vec![]) })
    }
}

struct Fixture(PathBuf);

impl Fixture {
    fn new(staged: bool) -> Self {
        let root = std::env::temp_dir().join(format!("uob-config-api-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        for (name, token) in [("reader", READER), ("writer", WRITER), ("admin", ADMIN)] {
            let path = root.join(name);
            std::fs::write(&path, token).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        let staged = if staged {
            format!(
                "staged_targets_file = '{}'\n",
                root.join("staged.toml").display()
            )
        } else {
            String::new()
        };
        std::fs::write(
            root.join("bridge.toml"),
            format!(
                "[bridge]\nid = 'site-01'\nenvironment = 'demo'\ntarget_id = 'main'\n\n\
                 [configuration_api]\n{staged}{}\n\
                 [[targets]]\nid = 'main'\nkind = 'mqtt'\nenabled = true\n\n\
                 [targets.settings]\nbroker_url = 'mqtt://127.0.0.1:1883'\n\
                 allow_plaintext = true\ncredentials_file = '/etc/uob/secrets/mqtt.toml'\n",
                credentials(&root)
            ),
        )
        .unwrap();
        Self(root)
    }

    fn config(&self) -> PathBuf {
        self.0.join("bridge.toml")
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn credentials(root: &std::path::Path) -> String {
    [
        ("reader", "'configuration:read'"),
        ("writer", "'configuration:read', 'configuration:write'"),
        (
            "admin",
            "'configuration:read', 'configuration:write', 'configuration:discard'",
        ),
    ]
    .map(|(name, permissions)| {
        format!(
            "[[configuration_api.credentials]]\nprincipal = '{name}'\ntoken_file = '{}'\n\
             permissions = [{permissions}]\n",
            root.join(name).display()
        )
    })
    .join("")
}

fn running() -> TargetDeliveryDestination {
    TargetDeliveryDestination {
        target_instance_id: TargetInstanceId::new("main").unwrap(),
        configuration_revision: 1,
    }
}

async fn router(fixture: &Fixture, store: Arc<Store>) -> Router {
    let loaded = crate::configuration::load(&fixture.config()).unwrap();
    let identity = loaded.service.application.identity().clone();
    let setup = start(
        loaded.configuration_api,
        &identity,
        loaded.service.target_selection.as_ref(),
        Some(store as Arc<dyn TargetDispositionStore>),
    )
    .await
    .unwrap()
    .expect("credentials mount the routes");
    uob_management_adapter::configuration_router(identity, setup)
}

async fn send(
    router: &Router,
    method: &str,
    path: &str,
    token: &str,
    body: Option<serde_json::Value>,
) -> (StatusCode, serde_json::Value) {
    let request = Request::builder()
        .method(method)
        .uri(format!("/api/v1/configuration/targets{path}"))
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .header(header::CONTENT_TYPE, "application/json");
    let request = request
        .body(body.map_or_else(Body::empty, |body| Body::from(body.to_string())))
        .unwrap();
    let response = router.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let body = to_bytes(response.into_body(), 256 * 1024).await.unwrap();
    let text = String::from_utf8(body.to_vec()).unwrap();
    for token in [READER, WRITER, ADMIN] {
        assert!(!text.contains(token), "credential reflected in {text}");
    }
    (status, serde_json::from_str(&text).unwrap_or_default())
}

fn switch_to_http() -> serde_json::Value {
    serde_json::json!({
        "target_id": "api",
        "targets": [
            {"id": "main", "kind": "mqtt", "enabled": false, "settings": {
                "broker_url": "mqtt://127.0.0.1:1883", "allow_plaintext": true,
                "credentials_file": {"credential_reference": "/etc/uob/secrets/mqtt.toml"}}},
            {"id": "api", "kind": "ems-scada.http", "enabled": true,
             "settings": {"listen_addr": "127.0.0.1:9080"}}
        ]
    })
}

#[tokio::test]
async fn start_settles_dispositions_for_the_selected_destination_before_serving() {
    let fixture = Fixture::new(true);
    let store = Arc::new(Store::default());
    let _ = router(&fixture, store.clone()).await;
    assert_eq!(*store.settled.lock().unwrap(), [Some(running())]);
}

#[tokio::test]
async fn backlog_blocks_apply_until_an_audited_archive_then_apply_stages_next_start() {
    let fixture = Fixture::new(true);
    let store = Arc::new(Store::default());
    store.backlog.lock().unwrap().push(TargetBacklogFact {
        destination: running(),
        pending_critical_deliveries: 2,
        pending_deliveries: 2,
    });
    let router = router(&fixture, store.clone()).await;

    let (status, view) = send(&router, "GET", "", READER, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(view["next_start"]["source"], "base");
    assert_eq!(view["restart_required"], false);
    assert_eq!(
        view["next_start"]["targets"][0]["settings"]["credentials_file"]["credential_reference"],
        "/etc/uob/secrets/mqtt.toml"
    );
    let digest = view["configuration_digest"].as_str().unwrap().to_owned();
    let apply = serde_json::json!({"expected_digest": digest, "configuration": switch_to_http()});

    let (status, report) = send(&router, "POST", "/validate", READER, Some(switch_to_http())).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(report["valid"], false);
    assert_eq!(
        report["errors"][0]["code"],
        "target.pending_destination_change"
    );
    assert_eq!(report["blocking_destinations"][0]["target_id"], "main");

    let (status, _) = send(&router, "POST", "/apply", READER, Some(apply.clone())).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, body) = send(&router, "POST", "/apply", WRITER, Some(apply.clone())).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(
        body["validation"]["blocking_destinations"][0]["configuration_revision"],
        1
    );
    assert!(!fixture.0.join("staged.toml").exists());

    let discard =
        serde_json::json!({"target_id": "main", "configuration_revision": 1, "action": "discard"});
    let (status, _) = send(&router, "POST", "/dispositions", WRITER, Some(discard)).await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "discard needs the destructive permission"
    );
    let archive =
        serde_json::json!({"target_id": "main", "configuration_revision": 1, "action": "archive"});
    let (status, record) = send(
        &router,
        "POST",
        "/dispositions",
        WRITER,
        Some(archive.clone()),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(record["authorized_by"], "writer");
    let (status, body) = send(&router, "POST", "/dispositions", ADMIN, Some(archive)).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["error"], "configuration.disposition_exists");

    let (status, outcome) = send(&router, "POST", "/apply", WRITER, Some(apply.clone())).await;
    assert_eq!(status, StatusCode::OK, "{outcome}");
    assert_eq!(outcome["restart_required"], true);
    assert_eq!(outcome["running_destination"]["target_id"], "main");
    assert_eq!(outcome["next_destination"]["target_id"], "api");
    assert_eq!(outcome["dispositions"][0], record["audit_event_id"]);
    // The same digest is now stale: a second writer cannot overwrite blindly.
    let (status, body) = send(&router, "POST", "/apply", WRITER, Some(apply)).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["error"], "configuration.conflict");

    let (_, view) = send(&router, "GET", "", READER, None).await;
    assert_eq!(view["next_start"]["source"], "staged");
    assert_eq!(view["running_destination"]["target_id"], "main");
    assert_eq!(view["restart_required"], true);
    assert_eq!(view["dispositions"][0]["state"], "authorized");
    let staged = std::fs::read_to_string(fixture.0.join("staged.toml")).unwrap();
    assert!(staged.contains("applied_by = \"writer\""));
    let loaded = crate::configuration::load(&fixture.config()).unwrap();
    assert_eq!(
        loaded.service.target_selection.unwrap().target_id.as_str(),
        "api"
    );
}

#[tokio::test]
async fn dispositions_require_pending_critical_work_and_apply_requires_a_staged_path() {
    let fixture = Fixture::new(false);
    let store = Arc::new(Store::default());
    let router = router(&fixture, store).await;
    let archive =
        serde_json::json!({"target_id": "main", "configuration_revision": 1, "action": "archive"});
    let (status, body) = send(&router, "POST", "/dispositions", ADMIN, Some(archive)).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["error"], "configuration.disposition_not_required");

    let (_, view) = send(&router, "GET", "", READER, None).await;
    let apply = serde_json::json!({
        "expected_digest": view["configuration_digest"],
        "configuration": switch_to_http(),
    });
    let (status, body) = send(&router, "POST", "/apply", ADMIN, Some(apply)).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(body["error"], "configuration.apply_unavailable");
}

#[test]
fn credential_configuration_is_explicit_unique_and_bridge_scoped() {
    let bridge = BridgeId::new("site-01").unwrap();
    let parse = |document: &str| toml::from_str::<Configuration>(document);
    let valid = "[[credentials]]\nprincipal = 'a'\ntoken_file = '/etc/uob/a'\n\
                 permissions = ['configuration:read']\n";
    assert!(
        parse(valid)
            .unwrap()
            .validate(&bridge, Environment::Demo)
            .is_ok()
    );
    assert!(parse(&valid.replace("configuration:read", "read")).is_err());
    for invalid in [
        valid.replace("/etc/uob/a", "relative/a"),
        valid.replace("/etc/uob/a", "/etc/uob/../a"),
        valid.replace("['configuration:read']", "[]"),
        valid.replace(
            "['configuration:read']",
            "['configuration:read', 'configuration:read']",
        ),
        format!("{valid}{}", valid.replace("/etc/uob/a", "/etc/uob/b")),
        format!(
            "{valid}{}",
            valid.replace("principal = 'a'", "principal = 'b'")
        ),
        format!("staged_targets_file = 'staged.toml'\n{valid}"),
    ] {
        assert!(
            parse(&invalid)
                .unwrap()
                .validate(&bridge, Environment::Demo)
                .is_err(),
            "{invalid}"
        );
    }
    let staging = format!("staged_targets_file = '/var/lib/uob/staged.toml'\n{valid}");
    assert!(
        parse(&staging)
            .unwrap()
            .validate(&bridge, Environment::Staging)
            .is_err()
    );
}

#[test]
fn credential_files_must_be_private_distinct_and_bound_to_the_environment() {
    let fixture = Fixture::new(true);
    let loaded = crate::configuration::load(&fixture.config()).unwrap();
    assert!(loaded.configuration_api.resolve().unwrap().is_some());

    std::fs::set_permissions(
        fixture.0.join("reader"),
        std::fs::Permissions::from_mode(0o644),
    )
    .unwrap();
    assert!(loaded.configuration_api.resolve().is_err());
    std::fs::set_permissions(
        fixture.0.join("reader"),
        std::fs::Permissions::from_mode(0o600),
    )
    .unwrap();
    std::fs::write(fixture.0.join("reader"), WRITER).unwrap();
    assert!(
        loaded.configuration_api.resolve().is_err(),
        "duplicate secrets"
    );
    std::fs::write(
        fixture.0.join("reader"),
        READER.replace("demo", "production"),
    )
    .unwrap();
    assert!(
        loaded.configuration_api.resolve().is_err(),
        "wrong audience"
    );
}
