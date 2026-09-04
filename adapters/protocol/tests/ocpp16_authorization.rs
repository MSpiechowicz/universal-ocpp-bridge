use std::{
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use uob_application::{
    AuthorizationChange, AuthorizationDecision, AuthorizationDenialReason, AuthorizationProvider,
    AuthorizationProviderDescriptor, AuthorizationProviderFuture, AuthorizationReference,
    AuthorizationState, LocalAuthorizationService, OperationalStore, PageLimit,
    SensitiveAuthorizationToken,
};
use uob_contracts::{
    BridgeId, CanonicalConnectorId, CanonicalResource, NativeProtocolReference, ResourceRef,
    StationId, UtcTimestamp,
};
use uob_protocol_adapter::{DecodeErrorKind, v16};
use uob_provider_adapter::LocalAuthorizationProvider;
use uob_storage_adapter::SqliteOperationalStore;
use uuid::Uuid;

type Store = SqliteOperationalStore<String, String, String, String>;
type Authorization = LocalAuthorizationService<String, String, String, String>;

const AUTHORIZE: &[u8] =
    include_bytes!("../../../tests/ocpp-fixtures/corpus/wire/1.6/authorize.json");
const START: &[u8] = br#"[2,"transaction-start","StartTransaction",{"connectorId":1,"idTag":"LOCAL-USER-1","meterStart":1000,"timestamp":"2026-09-01T00:00:02Z"}]"#;

#[tokio::test]
async fn maps_allowed_expired_unknown_and_invalid_authorize_results() {
    let database = TestDatabase::new();
    let service = service(&database).await;
    service
        .apply_change(change(
            reference("LOCAL-USER-1").await,
            AuthorizationState::Active,
            1,
            Some(timestamp(5)),
        ))
        .await
        .expect("persist active identity");

    let allowed = v16::authorize_call(
        AUTHORIZE,
        &station_resource(),
        &service,
        &LocalAuthorizationProvider,
        timestamp(4),
    )
    .await
    .expect("authorize fixture");
    assert!(matches!(
        allowed.decision,
        AuthorizationDecision::Allowed { .. }
    ));
    assert_eq!(
        allowed
            .authorize_response_frame()
            .expect("authorize response"),
        serde_json::json!([3, "fixture-16-authorize", {
            "idTagInfo": {"expiryDate": "2026-09-01T00:00:05Z", "status": "Accepted"}
        }])
    );

    let expired = v16::authorize_call(
        AUTHORIZE,
        &station_resource(),
        &service,
        &LocalAuthorizationProvider,
        timestamp(5),
    )
    .await
    .expect("expired result");
    assert_eq!(
        expired
            .authorize_response_frame()
            .expect("expired response")[2]["idTagInfo"]["status"],
        "Expired"
    );

    let unknown = br#"[2,"unknown-auth","Authorize",{"idTag":"UNKNOWN"}]"#;
    let unknown = v16::authorize_call(
        unknown,
        &station_resource(),
        &service,
        &LocalAuthorizationProvider,
        timestamp(1),
    )
    .await
    .expect("unknown result");
    assert_eq!(
        unknown
            .authorize_response_frame()
            .expect("invalid response")[2]["idTagInfo"]["status"],
        "Invalid"
    );

    let invalid = br#"[2,"invalid-auth","Authorize",{"idTag":""}]"#;
    assert_eq!(
        v16::authorize_call(
            invalid,
            &station_resource(),
            &service,
            &LocalAuthorizationProvider,
            timestamp(1),
        )
        .await
        .expect_err("invalid idTag")
        .kind(),
        DecodeErrorKind::InvalidPayload
    );
}

#[tokio::test]
async fn waits_for_provider_decision_without_exposing_a_premature_result() {
    let database = TestDatabase::new();
    let service = service(&database).await;
    let resolved = Arc::new(AtomicBool::new(false));
    let provider = DelayedProvider {
        resolved: Arc::clone(&resolved),
        reference: reference("LOCAL-USER-1").await,
    };
    service
        .apply_change(change(
            provider.reference.clone(),
            AuthorizationState::Active,
            1,
            None,
        ))
        .await
        .expect("persist identity");

    let resource = station_resource();
    let pending = v16::authorize_call(AUTHORIZE, &resource, &service, &provider, timestamp(1));
    tokio::pin!(pending);
    assert!(
        tokio::time::timeout(Duration::from_millis(5), &mut pending)
            .await
            .is_err()
    );
    assert!(!resolved.load(Ordering::SeqCst));
    let outcome = pending.await.expect("delayed provider result");
    assert!(resolved.load(Ordering::SeqCst));
    assert!(matches!(
        outcome.decision,
        AuthorizationDecision::Allowed { .. }
    ));
}

#[tokio::test]
async fn transaction_start_rechecks_revocation_and_recovery() {
    let database = TestDatabase::new();
    let authorization = service(&database).await;
    let token_reference = reference("LOCAL-USER-1").await;
    authorization
        .apply_change(change(
            token_reference.clone(),
            AuthorizationState::Active,
            1,
            None,
        ))
        .await
        .expect("persist active identity");
    let connector = connector_resource(1);
    let accepted = v16::authorize_call(
        START,
        &connector,
        &authorization,
        &LocalAuthorizationProvider,
        timestamp(1),
    )
    .await
    .expect("authorized transaction start");
    assert!(matches!(
        accepted.decision,
        AuthorizationDecision::Allowed { .. }
    ));
    assert!(accepted.authorize_response_frame().is_none());

    authorization
        .apply_change(change(
            token_reference,
            AuthorizationState::Revoked,
            2,
            None,
        ))
        .await
        .expect("persist revocation");
    let denied = v16::authorize_call(
        START,
        &connector,
        &authorization,
        &LocalAuthorizationProvider,
        timestamp(2),
    )
    .await
    .expect("revoked transaction result");
    assert_eq!(
        denied.decision,
        AuthorizationDecision::Denied {
            reason: AuthorizationDenialReason::Revoked
        }
    );
    drop(authorization);

    let recovered = service(&database).await;
    let denied_after_reconnect = v16::authorize_call(
        START,
        &connector,
        &recovered,
        &LocalAuthorizationProvider,
        timestamp(3),
    )
    .await
    .expect("recovered revocation result");
    assert_eq!(denied_after_reconnect.decision, denied.decision);

    let wrong_connector = connector_resource(2);
    assert_eq!(
        v16::authorize_call(
            START,
            &wrong_connector,
            &recovered,
            &LocalAuthorizationProvider,
            timestamp(3),
        )
        .await
        .expect_err("native connector mismatch")
        .kind(),
        DecodeErrorKind::InvalidPayload
    );
}

struct DelayedProvider {
    resolved: Arc<AtomicBool>,
    reference: AuthorizationReference,
}

impl AuthorizationProvider for DelayedProvider {
    fn descriptor(&self) -> AuthorizationProviderDescriptor {
        AuthorizationProviderDescriptor {
            kind: "test.delayed",
            test_only: true,
        }
    }

    fn resolve<'a>(
        &'a self,
        _token: &'a SensitiveAuthorizationToken,
    ) -> AuthorizationProviderFuture<'a> {
        Box::pin(async move {
            tokio::time::sleep(Duration::from_millis(20)).await;
            self.resolved.store(true, Ordering::SeqCst);
            Ok(self.reference.clone())
        })
    }
}

async fn service(database: &TestDatabase) -> Authorization {
    let store: Arc<dyn OperationalStore<String, String, String, String>> =
        Arc::new(Store::open(database.path(), 8).expect("open SQLite store"));
    LocalAuthorizationService::recover(store, PageLimit::new(16).expect("recovery limit"))
        .await
        .expect("recover local authorization")
}

async fn reference(token: &str) -> AuthorizationReference {
    LocalAuthorizationProvider
        .resolve(&SensitiveAuthorizationToken::new(token).expect("token"))
        .await
        .expect("local reference")
}

fn change(
    reference: AuthorizationReference,
    state: AuthorizationState,
    revision: u64,
    expires_at: Option<UtcTimestamp>,
) -> AuthorizationChange {
    AuthorizationChange {
        reference,
        resource: station_resource(),
        state,
        revision,
        changed_at: timestamp(0),
        expires_at,
    }
}

fn station_resource() -> ResourceRef {
    ResourceRef {
        bridge_id: BridgeId::new("bridge-test").expect("bridge"),
        station_id: StationId::new("station-a").expect("station"),
        resource: None,
        native_protocol_reference: None,
    }
}

fn connector_resource(connector_id: u32) -> ResourceRef {
    ResourceRef {
        resource: Some(CanonicalResource::Connector {
            connector_id: CanonicalConnectorId::new(format!("connector-{connector_id}"))
                .expect("connector"),
        }),
        native_protocol_reference: Some(NativeProtocolReference::Ocpp16 { connector_id }),
        ..station_resource()
    }
}

fn timestamp(second: u8) -> UtcTimestamp {
    serde_json::from_str(&format!("\"2026-09-01T00:00:{second:02}Z\"")).expect("timestamp")
}

struct TestDatabase(PathBuf);

impl TestDatabase {
    fn new() -> Self {
        Self(std::env::temp_dir().join(format!("uob-ocpp16-auth-{}.sqlite3", Uuid::new_v4())))
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TestDatabase {
    fn drop(&mut self) {
        let _database = std::fs::remove_file(&self.0);
        let _wal = std::fs::remove_file(format!("{}-wal", self.0.display()));
        let _shared_memory = std::fs::remove_file(format!("{}-shm", self.0.display()));
    }
}
