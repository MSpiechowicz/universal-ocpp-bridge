use std::{
    future::poll_fn,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

use time::OffsetDateTime;
use tokio::time::Duration;
use uob_application::{
    BridgeTarget, BridgeTargetFactory, ConfigurationError, ConfigurationSchema, DeliverySemantic,
    TargetConfiguration, TargetContext, TargetDescriptor, TargetLimits, TargetMessageClass,
    TargetPortError, TargetPortErrorCode, TargetPortFuture, TargetQuery, TargetQueryPort,
    TargetQueryResult, TargetRetainedEventStream, TargetRuntimeLimits, TargetTask,
    ValidatedTargetConfiguration,
};
use uob_contracts::{
    BridgeId, ContractVersion, Environment, ResourceRef, StationId, TargetInstanceId, TargetKind,
    UtcTimestamp,
};
use uob_target_adapter::{
    BridgeTargetSelection, ConfiguredTarget, TargetDisplayFamily, TargetRegistration,
    TargetRegistry,
};
use uob_target_conformance::{
    FakeTargetHost, HostCapacities, UnsupportedQueryPort, inspect_descriptor,
};

struct AlternateFactory {
    runs: Arc<AtomicUsize>,
}

impl BridgeTargetFactory<(), ()> for AlternateFactory {
    fn kind(&self) -> &'static str {
        "future.memory-map"
    }

    fn configuration_schema(&self) -> ConfigurationSchema {
        ConfigurationSchema::default()
    }

    fn validate(
        &self,
        configuration: &TargetConfiguration,
    ) -> Result<ValidatedTargetConfiguration, ConfigurationError> {
        Ok(ValidatedTargetConfiguration::new(configuration.clone()))
    }

    fn create(
        &self,
        configuration: ValidatedTargetConfiguration,
    ) -> Result<Box<dyn BridgeTarget<(), ()>>, ConfigurationError> {
        Ok(Box::new(AlternateTarget {
            instance_id: configuration.configuration().target_instance_id.clone(),
            runs: Arc::clone(&self.runs),
        }))
    }
}

struct AlternateTarget {
    instance_id: TargetInstanceId,
    runs: Arc<AtomicUsize>,
}

impl BridgeTarget<(), ()> for AlternateTarget {
    fn descriptor(&self) -> TargetDescriptor {
        TargetDescriptor {
            kind: text(TargetKind::new, "future.memory-map"),
            instance_id: self.instance_id.clone(),
            contract_version: ContractVersion::V1_INITIAL,
            outbound_message_classes: vec![TargetMessageClass::StationSnapshot],
            inbound_operations: vec![],
            limits: TargetLimits {
                maximum_message_bytes: 1024,
                maximum_in_flight_deliveries: 1,
                maximum_in_flight_commands: 0,
            },
            delivery_semantics: vec![DeliverySemantic::LocalExposure],
            optional_capabilities: vec![],
        }
    }

    fn run(self: Box<Self>, mut context: TargetContext<(), ()>) -> TargetTask {
        Box::pin(async move {
            let _ = context
                .queries
                .query(TargetQuery::StationSnapshot(station()))
                .await;
            self.runs.fetch_add(1, Ordering::SeqCst);
            poll_fn(|cx| context.shutdown.as_mut().poll_shutdown(cx)).await;
            Ok(())
        })
    }
}

struct CountingQueries {
    calls: Arc<AtomicUsize>,
}

impl TargetQueryPort<()> for CountingQueries {
    fn query(&self, _query: TargetQuery) -> TargetPortFuture<'_, TargetQueryResult<()>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Box::pin(async {
            Err(TargetPortError::new(
                TargetPortErrorCode::Unsupported,
                "fixture.no_state",
            ))
        })
    }

    fn subscribe_retained_events(
        &self,
        _query: uob_application::RetainedEventQuery,
    ) -> TargetPortFuture<'_, TargetRetainedEventStream<()>> {
        Box::pin(async {
            Err(TargetPortError::new(
                TargetPortErrorCode::Unsupported,
                "fixture.no_events",
            ))
        })
    }
}

#[tokio::test]
async fn alternate_target_registers_queries_and_shuts_down_through_shared_contracts() {
    let runs = Arc::new(AtomicUsize::new(0));
    let mut registry = TargetRegistry::new();
    registry
        .register(
            AlternateFactory {
                runs: Arc::clone(&runs),
            },
            TargetRegistration {
                display_family: TargetDisplayFamily {
                    id: "future".to_owned(),
                    display_name: "Future target".to_owned(),
                },
                presets: vec![],
                capabilities: vec![],
                transport_policy: None,
            },
        )
        .expect("alternate registration");

    let target_id = text(TargetInstanceId::new, "alternate-main");
    let target = registry
        .validate(BridgeTargetSelection {
            bridge_id: text(BridgeId::new, "bridge-1"),
            environment: Environment::Demo,
            target_id: target_id.clone(),
            targets: vec![ConfiguredTarget {
                kind: "future.memory-map".to_owned(),
                enabled: true,
                configuration: TargetConfiguration::new(target_id, 1),
                transport_security: None,
            }],
        })
        .expect("validated alternate")
        .create()
        .expect("constructed alternate");
    assert!(inspect_descriptor(&target.descriptor()).is_empty());

    let query_calls = Arc::new(AtomicUsize::new(0));
    let fixture = FakeTargetHost::build(
        HostCapacities {
            deliveries: 1,
            commands: 1,
            reports: 1,
            diagnostics: 0,
        },
        Arc::new(CountingQueries {
            calls: Arc::clone(&query_calls),
        }),
        TargetRuntimeLimits {
            maximum_in_flight_deliveries: 1,
            maximum_in_flight_commands: 0,
            maximum_command_bytes: 1,
        },
        timestamp(),
    )
    .expect("fake host");
    let host = fixture.host;
    let task = tokio::spawn(target.run(fixture.context));

    tokio::time::timeout(Duration::from_secs(1), async {
        while runs.load(Ordering::SeqCst) == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("target queried host");
    assert_eq!(query_calls.load(Ordering::SeqCst), 1);

    host.request_shutdown();
    tokio::time::timeout(Duration::from_secs(1), task)
        .await
        .expect("shutdown timeout")
        .expect("join")
        .expect("target result");
}

fn station() -> ResourceRef {
    ResourceRef {
        bridge_id: text(BridgeId::new, "bridge-1"),
        station_id: text(StationId::new, "station-a"),
        resource: None,
        native_protocol_reference: None,
    }
}

fn timestamp() -> UtcTimestamp {
    UtcTimestamp::new(OffsetDateTime::UNIX_EPOCH)
}

fn text<T, E: std::fmt::Debug>(constructor: impl FnOnce(String) -> Result<T, E>, value: &str) -> T {
    constructor(value.to_owned()).expect("valid fixture text")
}

#[test]
fn unsupported_query_port_remains_available_to_read_only_adapter_tests() {
    let _port: Arc<dyn TargetQueryPort<()>> = Arc::new(UnsupportedQueryPort);
}
