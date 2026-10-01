use std::sync::Arc;

use uob_application::{DeliveryId, TargetDelivery, TargetDeliveryClass, TargetMessage};
use uob_contracts::{
    AuthenticatedCommandOrigin, CommandLifecycle, CommandResult, CommandReturnRoute,
    ContractVersion, CorrelationId, DeviceAttributeType201, DeviceCharacteristics201,
    DeviceComponent201, DeviceDataType201, DeviceModelQuery201, DeviceModelResult201,
    DeviceMutability201, DeviceReportAck201, DeviceReportAttribute201, DeviceReportBase201,
    DeviceReportFailure201, DeviceReportFragment201, DeviceReportItem201, DeviceReportProgress201,
    DeviceReportState201, DeviceValue201, DeviceVariable201, PrincipalId, RequestId,
};

use crate::support::fixtures::{TestEvent, resource, target_id, timestamp};

// Construct target-owned authoritative state directly. These fixtures do not invoke privileged
// OCPP ingress and must never be used as proof that production MQTT credentials have that grant.
pub(super) fn device_delivery(delivery_id: &str, request_id: &str) -> TargetDelivery<TestEvent> {
    let station = resource("bridge-a", "station-a");
    let result = CommandResult {
        schema_version: ContractVersion::V1_DEVICE_MODEL_201,
        correlation_id: None,
        resource: station.clone(),
        return_route: CommandReturnRoute {
            request_id: RequestId::new(request_id).expect("request identity"),
            origin: AuthenticatedCommandOrigin::Target {
                target_instance_id: target_id(),
                principal_id: PrincipalId::new("mqtt-target:main").expect("target principal"),
            },
        },
        lifecycle: CommandLifecycle::ProtocolResponse {
            accepted: true,
            error: None,
        },
        recorded_at: timestamp(),
        observed_effects: vec![],
        configuration: None,
        configuration_observations: vec![],
        trigger_observation: None,
        trigger_observation_201: None,
        composite_schedule_16: None,
        device_model_201: Some(DeviceModelResult201 {
            query: DeviceModelQuery201::GetBaseReport {
                request_id: i32::MIN,
                report_base: DeviceReportBase201::FullInventory,
            },
            connection: CorrelationId::new("native-connection-a").expect("native connection"),
            generation: 7,
            dispatch_recorded_at: timestamp(),
            native_ack: Some(DeviceReportAck201::Accepted),
            variables: vec![],
            report: DeviceReportState201::Incomplete {
                reason: DeviceReportFailure201::Timeout,
                progress: Some(DeviceReportProgress201 {
                    fragments: 1,
                    items: 2,
                    bytes: 320,
                }),
            },
        }),
    };
    TargetDelivery {
        delivery_id: DeliveryId::new(delivery_id).expect("delivery identity"),
        target_instance_id: target_id(),
        target_configuration_revision: 1,
        station_ordering_key: station,
        deadline: timestamp(),
        class: TargetDeliveryClass::Durable,
        message: Arc::new(TargetMessage::CommandResult(result)),
    }
}

pub(super) fn result_mut(delivery: &mut TargetDelivery<TestEvent>) -> &mut CommandResult {
    let TargetMessage::CommandResult(result) =
        Arc::get_mut(&mut delivery.message).expect("unique result delivery")
    else {
        unreachable!("device result fixture");
    };
    result
}

// Safe metadata and already-redacted values exercise rich serialization, not the native sanitizer.
pub(super) fn rich_report() -> DeviceReportState201 {
    let items = (0..32)
        .map(|index| DeviceReportItem201 {
            component: DeviceComponent201 {
                name: "VendorCtrlr".to_owned(),
                instance: Some(format!("module-{index}")),
                evse: None,
            },
            variable: DeviceVariable201 {
                name: format!("PrivateSetting{index}"),
                instance: Some("primary".to_owned()),
            },
            attributes: vec![DeviceReportAttribute201 {
                attribute_type: DeviceAttributeType201::Actual,
                value: DeviceValue201 {
                    present: true,
                    redacted: true,
                    empty: false,
                    value: None,
                },
                mutability: Some(DeviceMutability201::WriteOnly),
                persistent: Some(true),
                constant: Some(false),
            }],
            characteristics: Some(DeviceCharacteristics201 {
                unit: None,
                data_type: DeviceDataType201::String,
                min_limit: None,
                max_limit: None,
                supports_monitoring: false,
            }),
        })
        .collect();
    DeviceReportState201::Complete {
        progress: DeviceReportProgress201 {
            fragments: 1,
            items: 32,
            bytes: 16_000,
        },
        fragments: vec![DeviceReportFragment201 {
            generated_at: "2026-09-01T03:00:00Z".to_owned(),
            sequence: 0,
            more: false,
            items: 32,
        }],
        items,
    }
}
