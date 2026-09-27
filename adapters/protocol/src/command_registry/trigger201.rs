//! Pinned OCPP 2.0.1 `TriggerMessage` request and effective native scope.
use rust_ocpp::v2_0_1::messages::trigger_message::TriggerMessageRequest;
use serde_json::Value;
use uob_contracts::{
    CanonicalResource, CommandErrorCode, NativeProtocolReference, PrivilegedOcppOperation,
    ProtocolEdition, ResourceRef, TriggerEvse201, TriggerMessageClass201,
};

pub(super) const SCHEMA: &str = "urn:OCPP:Cp:2:2020:3:TriggerMessageRequest";
pub(super) const CLASSES: &[&str] = &[
    "BootNotification",
    "LogStatusNotification",
    "FirmwareStatusNotification",
    "Heartbeat",
    "MeterValues",
    "SignChargingStationCertificate",
    "SignV2GCertificate",
    "StatusNotification",
    "TransactionEvent",
    "SignCombinedCertificate",
    "PublishFirmwareStatusNotification",
];

pub(super) fn validate(
    resource: &ResourceRef,
    operation: &PrivilegedOcppOperation<Value>,
) -> Result<(TriggerMessageClass201, Option<TriggerEvse201>), CommandErrorCode> {
    use CommandErrorCode::InvalidParameters;
    if operation.protocol != ProtocolEdition::Ocpp201 || operation.payload_schema.as_str() != SCHEMA
    {
        return Err(InvalidParameters);
    }
    let payload = operation.payload.as_object().ok_or(InvalidParameters)?;
    if payload
        .keys()
        .any(|key| !["requestedMessage", "evse", "customData"].contains(&key.as_str()))
        || payload
            .get("customData")
            .is_some_and(|value| !valid_custom_data(value))
        || payload
            .get("requestedMessage")
            .and_then(Value::as_str)
            .is_none_or(|class| !CLASSES.contains(&class))
    {
        return Err(InvalidParameters);
    }
    let class: TriggerMessageClass201 = serde_json::from_value(payload["requestedMessage"].clone())
        .map_err(|_| InvalidParameters)?;
    let evse = parse_evse(payload.get("evse"))?;
    let _: TriggerMessageRequest =
        serde_json::from_value(operation.payload.clone()).map_err(|_| InvalidParameters)?;

    if !valid_scope(resource, class, evse) {
        return Err(InvalidParameters);
    }
    Ok((class, evse))
}

fn parse_evse(value: Option<&Value>) -> Result<Option<TriggerEvse201>, CommandErrorCode> {
    use CommandErrorCode::InvalidParameters;
    let Some(value) = value else {
        return Ok(None);
    };
    let fields = value.as_object().ok_or(InvalidParameters)?;
    if fields.is_empty()
        || fields
            .keys()
            .any(|key| !["id", "connectorId", "customData"].contains(&key.as_str()))
        || fields
            .get("customData")
            .is_some_and(|value| !valid_custom_data(value))
    {
        return Err(InvalidParameters);
    }
    let evse = TriggerEvse201 {
        id: u32::try_from(
            fields
                .get("id")
                .and_then(Value::as_u64)
                .ok_or(InvalidParameters)?,
        )
        .map_err(|_| InvalidParameters)?,
        connector_id: fields
            .get("connectorId")
            .map(|value| {
                u32::try_from(value.as_u64().ok_or(InvalidParameters)?)
                    .map_err(|_| InvalidParameters)
            })
            .transpose()?,
    };
    if evse.id == 0
        || evse.id > i32::MAX as u32
        || evse
            .connector_id
            .is_some_and(|id| id == 0 || id > i32::MAX as u32)
    {
        return Err(InvalidParameters);
    }
    Ok(Some(evse))
}

fn valid_scope(
    resource: &ResourceRef,
    class: TriggerMessageClass201,
    evse: Option<TriggerEvse201>,
) -> bool {
    let station = resource.resource.is_none() && resource.native_protocol_reference.is_none();
    let evse_resource = matches!(
        (&resource.resource, resource.native_protocol_reference),
        (Some(CanonicalResource::Evse { connector_id: None, .. }),
            Some(NativeProtocolReference::Ocpp201 { evse_id: id, connector_id: None })) if id > 0
    );
    let connector_resource = matches!(
        (&resource.resource, resource.native_protocol_reference),
        (Some(CanonicalResource::Evse { connector_id: Some(_), .. }),
            Some(NativeProtocolReference::Ocpp201 { evse_id: id, connector_id: Some(connector) })) if id > 0 && connector > 0
    );
    if class.is_station_only() {
        station
    } else if class == TriggerMessageClass201::StatusNotification {
        matches!((evse, resource.native_protocol_reference),
            (Some(TriggerEvse201 { id, connector_id: Some(connector_id) }),
                Some(NativeProtocolReference::Ocpp201 { evse_id, connector_id: Some(native_connector) }))
                if connector_resource && id == evse_id && connector_id == native_connector)
    } else {
        match evse {
            None => station,
            Some(TriggerEvse201 { id, connector_id }) => {
                // MeterValues is reported at EVSE granularity; connector-only access cannot
                // authorize an EVSE-wide trigger even when a connectorId was supplied on wire.
                let scope =
                    if class == TriggerMessageClass201::MeterValues || class.is_certificate() {
                        evse_resource
                    } else {
                        evse_resource || connector_resource
                    };
                scope
                    && matches!(resource.native_protocol_reference,
                    Some(NativeProtocolReference::Ocpp201 { evse_id, connector_id: native_connector })
                    if id == evse_id && (native_connector.is_none() || connector_id == native_connector))
            }
        }
    }
}
fn valid_custom_data(value: &Value) -> bool {
    value.as_object().is_some_and(|fields| {
        fields
            .get("vendorId")
            .and_then(Value::as_str)
            .is_some_and(|vendor| vendor.chars().count() <= 255)
    })
}

pub(super) fn discoverable(resource: &ResourceRef) -> bool {
    resource.resource.is_none() && resource.native_protocol_reference.is_none()
        || matches!(
            (&resource.resource, resource.native_protocol_reference),
            (
                Some(CanonicalResource::Evse {
                    connector_id: None,
                    ..
                }),
                Some(NativeProtocolReference::Ocpp201 {
                    evse_id: 1..,
                    connector_id: None
                })
            ) | (
                Some(CanonicalResource::Evse {
                    connector_id: Some(_),
                    ..
                }),
                Some(NativeProtocolReference::Ocpp201 {
                    evse_id: 1..,
                    connector_id: Some(1..)
                })
            )
        )
}
