//! Behavior of one supervised EMS/SCADA HTTP session bound to a real socket.
//!
//! These tests run the shipped target against the shared fake target host, so the listener sees
//! exactly the bounded ports a supervised session receives inside the service.

use std::{net::TcpListener as StdTcpListener, sync::Arc, time::Duration};

use uob_application::{
    BridgeTarget, BridgeTargetFactory, ConfigurationValue, DeliveryId, DeliveryOutcome,
    TargetConfiguration, TargetDelivery, TargetDeliveryClass, TargetMessage, TargetRuntimeLimits,
};
use uob_contracts::{
    BridgeId, ContractVersion, Environment, ResourceCapabilities, ResourceRef, StationId,
    StationSnapshot, TargetInstanceId, UtcTimestamp,
};
use uob_ems_scada_http_target_adapter::EmsScadaHttpTargetFactory;
use uob_target_conformance::{
    DescriptorViolation, FakeTargetHost, HostCapacities, UnsupportedQueryPort, inspect_descriptor,
};

type Target = Box<dyn BridgeTarget<(), ()>>;

fn target(listen_addr: &str) -> Result<Target, uob_application::ConfigurationError> {
    let factory = EmsScadaHttpTargetFactory::new(Environment::Demo);
    let configuration =
        TargetConfiguration::new(TargetInstanceId::new("main").expect("target instance"), 1)
            .with_setting(
                "listen_addr".to_owned(),
                ConfigurationValue::Text(listen_addr.to_owned()),
            );
    let validated = <EmsScadaHttpTargetFactory as BridgeTargetFactory<(), ()>>::validate(
        &factory,
        &configuration,
    )?;
    <EmsScadaHttpTargetFactory as BridgeTargetFactory<(), ()>>::create(&factory, validated)
}

/// Reserves a free loopback port and releases it so the target can bind the same address.
fn free_loopback_address() -> String {
    let probe = StdTcpListener::bind("127.0.0.1:0").expect("probe listener");
    let address = probe.local_addr().expect("probe address");
    drop(probe);
    format!("127.0.0.1:{}", address.port())
}

fn snapshot_delivery(delivery_id: &str) -> TargetDelivery<()> {
    let station = ResourceRef {
        bridge_id: BridgeId::new("site-01").expect("bridge identity"),
        station_id: StationId::new("station-a").expect("station identity"),
        resource: None,
        native_protocol_reference: None,
    };
    TargetDelivery {
        delivery_id: DeliveryId::new(delivery_id).expect("delivery identity"),
        target_instance_id: TargetInstanceId::new("main").expect("target instance"),
        target_configuration_revision: 1,
        station_ordering_key: station.clone(),
        deadline: UtcTimestamp::new(time::OffsetDateTime::UNIX_EPOCH),
        class: TargetDeliveryClass::ReplaceableLatestState,
        message: Arc::new(TargetMessage::StationSnapshot(StationSnapshot {
            schema_version: ContractVersion::V1_INITIAL,
            station,
            observed_at: UtcTimestamp::new(time::OffsetDateTime::UNIX_EPOCH),
            connectivity: uob_contracts::Connectivity::Disconnected,
            capabilities: ResourceCapabilities::default(),
            resources: vec![],
            transactions: vec![],
            current_values: vec![],
        })),
    }
}

#[test]
fn the_descriptor_satisfies_the_shared_target_contract() {
    let target = target("127.0.0.1:9080").expect("configured target");
    assert_eq!(
        inspect_descriptor(&target.descriptor()),
        Vec::<DescriptorViolation>::new()
    );
}

#[tokio::test]
async fn a_started_listener_answers_only_its_versioned_integration_prefix() {
    let address = free_loopback_address();
    let target = target(&address).expect("configured target");
    let hosted = FakeTargetHost::<(), ()>::build(
        HostCapacities {
            deliveries: 4,
            commands: 2,
            reports: 4,
            diagnostics: 4,
        },
        Arc::new(UnsupportedQueryPort),
        TargetRuntimeLimits {
            maximum_in_flight_deliveries: 4,
            maximum_in_flight_commands: 2,
            maximum_command_bytes: 4096,
        },
        UtcTimestamp::new(time::OffsetDateTime::UNIX_EPOCH),
    )
    .expect("fake host");
    let host = hosted.host;
    let session = tokio::spawn(target.run(hosted.context));

    let capabilities = await_response(&address, "GET /bridge/v1/capabilities HTTP/1.1").await;
    assert!(capabilities.starts_with("HTTP/1.1 200"), "{capabilities}");
    assert!(
        capabilities.contains("\"kind\":\"ems-scada.http\""),
        "{capabilities}"
    );

    // The independent management API's own paths are not mounted on this listener.
    let management = await_response(&address, "GET /api/v1/identity HTTP/1.1").await;
    assert!(management.starts_with("HTTP/1.1 404"), "{management}");
    assert!(
        management.contains("ems_scada_http.unknown_resource"),
        "{management}"
    );

    host.request_shutdown();
    session
        .await
        .expect("session task")
        .expect("graceful shutdown");
}

#[tokio::test]
async fn a_delivery_is_reported_as_local_exposure_and_never_as_peer_consumption() {
    let address = free_loopback_address();
    let target = target(&address).expect("configured target");
    let hosted = FakeTargetHost::<(), ()>::build(
        HostCapacities {
            deliveries: 4,
            commands: 2,
            reports: 4,
            diagnostics: 4,
        },
        Arc::new(UnsupportedQueryPort),
        TargetRuntimeLimits {
            maximum_in_flight_deliveries: 4,
            maximum_in_flight_commands: 2,
            maximum_command_bytes: 4096,
        },
        UtcTimestamp::new(time::OffsetDateTime::UNIX_EPOCH),
    )
    .expect("fake host");
    let mut host = hosted.host;
    let session = tokio::spawn(target.run(hosted.context));

    host.try_deliver(snapshot_delivery("delivery-1"))
        .expect("queued delivery");
    let report = tokio::time::timeout(Duration::from_secs(5), host.next_report())
        .await
        .expect("report before timeout")
        .expect("delivery report");

    assert_eq!(report.delivery_id.as_str(), "delivery-1");
    match report.outcome {
        DeliveryOutcome::LocallyExposed { surface } => assert_eq!(surface, "/bridge/v1"),
        other => panic!("integration delivery must be local exposure, got {other:?}"),
    }

    host.request_shutdown();
    session
        .await
        .expect("session task")
        .expect("graceful shutdown");
}

#[tokio::test]
async fn no_integration_client_leaves_latest_state_waiting_in_the_target_outbox() {
    let address = free_loopback_address();
    let target = target(&address).expect("configured target");
    let hosted = FakeTargetHost::<(), ()>::build(
        HostCapacities {
            deliveries: 2,
            commands: 2,
            reports: 2,
            diagnostics: 4,
        },
        Arc::new(UnsupportedQueryPort),
        TargetRuntimeLimits {
            maximum_in_flight_deliveries: 2,
            maximum_in_flight_commands: 2,
            maximum_command_bytes: 4096,
        },
        UtcTimestamp::new(time::OffsetDateTime::UNIX_EPOCH),
    )
    .expect("fake host");
    let mut host = hosted.host;
    let session = tokio::spawn(target.run(hosted.context));

    // No client ever connects. Far more replaceable latest-state deliveries than the outbox can
    // hold are still accepted and reported, so nothing accumulates behind an absent consumer.
    let total = 12;
    let mut reported = Vec::with_capacity(total);
    for index in 0..total {
        let delivery = snapshot_delivery(&format!("delivery-{index}"));
        while host.try_deliver(clone_delivery(&delivery)).is_err() {
            let report = tokio::time::timeout(Duration::from_secs(5), host.next_report())
                .await
                .expect("report before timeout")
                .expect("delivery report");
            reported.push(report);
        }
    }
    while reported.len() < total {
        let report = tokio::time::timeout(Duration::from_secs(5), host.next_report())
            .await
            .expect("report before timeout")
            .expect("delivery report");
        reported.push(report);
    }

    for report in &reported {
        assert!(
            matches!(report.outcome, DeliveryOutcome::LocallyExposed { .. }),
            "{:?}",
            report.outcome
        );
    }

    host.request_shutdown();
    session
        .await
        .expect("session task")
        .expect("graceful shutdown");
}

/// Copies one delivery so the same canonical message can be queued repeatedly.
fn clone_delivery(delivery: &TargetDelivery<()>) -> TargetDelivery<()> {
    TargetDelivery {
        delivery_id: delivery.delivery_id.clone(),
        target_instance_id: delivery.target_instance_id.clone(),
        target_configuration_revision: delivery.target_configuration_revision,
        station_ordering_key: delivery.station_ordering_key.clone(),
        deadline: delivery.deadline,
        class: delivery.class,
        message: Arc::clone(&delivery.message),
    }
}

#[tokio::test]
async fn a_public_listen_address_is_refused_before_any_socket_is_opened() {
    // Offline validation already refuses the configuration, so no session can be constructed
    // and no socket is ever opened on a public address.
    let Err(error) = target("0.0.0.0:0") else {
        panic!("a public listener must not be constructible without TLS termination")
    };
    assert_eq!(
        error.code(),
        uob_application::ConfigurationErrorCode::MissingField
    );
}

/// Sends one minimal HTTP/1.1 request and returns the raw response text.
async fn await_response(address: &str, request_line: &str) -> String {
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

    for _ in 0..50 {
        let Ok(mut stream) = tokio::net::TcpStream::connect(address).await else {
            tokio::time::sleep(Duration::from_millis(20)).await;
            continue;
        };
        let request = format!("{request_line}\r\nHost: localhost\r\nConnection: close\r\n\r\n");
        stream
            .write_all(request.as_bytes())
            .await
            .expect("write request");
        let mut response = Vec::new();
        stream
            .read_to_end(&mut response)
            .await
            .expect("read response");
        return String::from_utf8_lossy(&response).into_owned();
    }
    panic!("integration listener never accepted a connection at {address}");
}
