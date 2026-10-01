use std::{collections::BTreeMap, net::TcpListener, path::PathBuf, sync::Arc, time::Duration};

use serde_json::Value;
use uob_application::{
    BridgeTargetFactory, CanonicalQuerySource, ConfigurationValue, CredentialReference,
    RetainedEventQuery, ScopedTargetQueryPort, TargetConfiguration, TargetPortError,
    TargetPortErrorCode, TargetPortFuture, TargetQuery, TargetQueryAuthorization,
    TargetQueryPermission, TargetQueryResult, TargetResourceScope, TargetRetainedEventStream,
    TargetRuntimeLimits,
};
use uob_contracts::{
    BridgeId, CommandResult, Environment, StationId, TargetInstanceId, UtcTimestamp,
};
use uob_ems_scada_http_target_adapter::EmsScadaHttpTargetFactory;
use uob_target_conformance::{FakeTargetHost, HostCapacities};

pub const OPERATOR: &str = "device-ems-operator";
pub const READER: &str = "device-ems-reader";
pub const STATION_OPERATOR: &str = "device-ems-station-operator";

/// Explicit test-only canonical evidence, not a command implementation or management rewrite.
/// The production scoped query port and credential checks surround this bounded source.
struct Evidence(BTreeMap<String, CommandResult>);

impl CanonicalQuerySource<Value> for Evidence {
    fn query<'a>(
        &'a self,
        _authorization: &'a TargetQueryAuthorization,
        query: TargetQuery,
    ) -> TargetPortFuture<'a, TargetQueryResult<Value>> {
        Box::pin(async move {
            match query {
                TargetQuery::CommandResult(id) => Ok(TargetQueryResult::CommandResult(
                    self.0.get(id.as_str()).cloned(),
                )),
                _ => Err(TargetPortError::new(
                    TargetPortErrorCode::Unsupported,
                    "fixture.unsupported_query",
                )),
            }
        })
    }

    fn subscribe_retained_events<'a>(
        &'a self,
        _authorization: &'a TargetQueryAuthorization,
        _query: RetainedEventQuery,
    ) -> TargetPortFuture<'a, TargetRetainedEventStream<Value>> {
        Box::pin(async {
            Err(TargetPortError::new(
                TargetPortErrorCode::Unsupported,
                "fixture.unsupported_subscription",
            ))
        })
    }
}

fn credentials_file(path: &std::path::Path) -> CredentialReference {
    std::fs::write(
        path,
        format!(
            "[[principals]]\nid = 'operator'\ntoken = '{OPERATOR}'\n\
             permissions = ['read', 'control']\nbridges = ['site-01']\n\n\
             [[principals]]\nid = 'reader'\ntoken = '{READER}'\n\
             permissions = ['read']\nbridges = ['site-01']\n\n\
             [[principals]]\nid = 'station-operator'\ntoken = '{STATION_OPERATOR}'\n\
             permissions = ['control']\n\
             stations = [{{bridge_id = 'site-01', station_id = 'station-a'}}]\n"
        ),
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    CredentialReference::new(path.to_str().unwrap()).unwrap()
}

pub struct Host {
    pub base: String,
    pub client: reqwest::Client,
    pub driver: FakeTargetHost<Value, Value>,
    folder: PathBuf,
    session: tokio::task::JoinHandle<Result<(), uob_application::TargetError>>,
}

impl Host {
    pub async fn start(results: Vec<CommandResult>, command_status_granted: bool) -> Self {
        // rustls installs the process provider atomically; concurrent fixture starts may reuse it.
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
        assert!(results.len() <= 16, "fixture result count is bounded");
        for result in &results {
            assert!(serde_json::to_vec(result).unwrap().len() < 1024 * 1024);
        }
        let state = Evidence(
            results
                .into_iter()
                .map(|result| (result.return_route.request_id.as_str().to_owned(), result))
                .collect(),
        );
        let folder = std::env::temp_dir().join(format!("uob-device-ems-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&folder).unwrap();
        let credentials = credentials_file(&folder.join("integration.toml"));
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap().to_string();
        drop(listener);
        let target_id = TargetInstanceId::new("main").unwrap();
        let factory = EmsScadaHttpTargetFactory::new(Environment::Demo);
        let configuration = TargetConfiguration::new(target_id.clone(), 1)
            .with_setting("listen_addr", ConfigurationValue::Text(address.clone()))
            .with_setting(
                "credentials_file",
                ConfigurationValue::CredentialReference(credentials),
            );
        let validated = <EmsScadaHttpTargetFactory as BridgeTargetFactory<Value, Value>>::validate(
            &factory,
            &configuration,
        )
        .unwrap();
        let target = <EmsScadaHttpTargetFactory as BridgeTargetFactory<Value, Value>>::create(
            &factory, validated,
        )
        .unwrap();
        let permissions = if command_status_granted {
            vec![TargetQueryPermission::CommandStatus]
        } else {
            vec![]
        };
        let queries = Arc::new(ScopedTargetQueryPort::new(
            Arc::new(state),
            TargetQueryAuthorization::new(
                target_id,
                permissions,
                ["station-a", "station-b"]
                    .into_iter()
                    .map(|station| TargetResourceScope::Station {
                        bridge_id: BridgeId::new("site-01").unwrap(),
                        station_id: StationId::new(station).unwrap(),
                    })
                    .collect(),
            ),
        ));
        let hosted = FakeTargetHost::<Value, Value>::build(
            HostCapacities {
                deliveries: 2,
                commands: 2,
                reports: 4,
                diagnostics: 2,
            },
            queries,
            TargetRuntimeLimits {
                maximum_in_flight_deliveries: 2,
                maximum_in_flight_commands: 2,
                maximum_command_bytes: 64 * 1024,
            },
            UtcTimestamp::new(time::OffsetDateTime::UNIX_EPOCH),
        )
        .unwrap();
        let session = tokio::spawn(target.run(hosted.context));
        let host = Self {
            base: format!("http://{address}"),
            client: reqwest::Client::builder()
                .timeout(Duration::from_secs(5))
                .build()
                .unwrap(),
            driver: hosted.host,
            folder,
            session,
        };
        host.wait_ready().await;
        host
    }

    async fn wait_ready(&self) {
        for _ in 0..100 {
            if let Ok(response) = self
                .client
                .get(format!("{}/bridge/v1/capabilities", self.base))
                .bearer_auth(OPERATOR)
                .send()
                .await
            {
                assert_eq!(response.status(), 200);
                return;
            }
            assert!(
                !self.session.is_finished(),
                "listener exited during startup"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("supervised listener did not start");
    }

    pub async fn get(&self, path: &str, token: &str) -> (u16, Value) {
        let response = self
            .client
            .get(format!("{}{path}", self.base))
            .bearer_auth(token)
            .send()
            .await
            .unwrap();
        let status = response.status().as_u16();
        (status, response.json().await.unwrap())
    }

    pub async fn stop(&mut self) {
        self.driver.request_shutdown();
        tokio::time::timeout(Duration::from_secs(5), &mut self.session)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
    }
}

impl Drop for Host {
    fn drop(&mut self) {
        self.session.abort();
        std::fs::remove_dir_all(&self.folder).unwrap();
    }
}
