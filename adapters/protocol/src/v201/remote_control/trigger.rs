//! OCPP 2.0.1 `TriggerMessage` target snapshot and native response.
use rust_ocpp::v2_0_1::messages::trigger_message::TriggerMessageResponse;
use serde_json::Value;
use uob_application::{CommandDispatchOutcome, TriggerExpectation201};
use uob_contracts::{
    CommandErrorCode, NativeProtocolReference, PrivilegedOcppOperation, ResourceRef,
    StationSnapshot, TriggerEvse201, TriggerMessageClass201, TriggerNativeResponse201,
    TriggerTarget201,
};

pub(super) fn prepare(
    operation: &PrivilegedOcppOperation<Value>,
    resource: &ResourceRef,
    snapshot: &StationSnapshot,
) -> Result<(Value, TriggerExpectation201), CommandErrorCode> {
    use CommandErrorCode::InvalidParameters;
    crate::command_registry::validate_privileged_operation(resource, operation)?;
    let class: TriggerMessageClass201 =
        serde_json::from_value(operation.payload["requestedMessage"].clone())
            .map_err(|_| InvalidParameters)?;
    let native_scope = operation
        .payload
        .get("evse")
        .map(|evse| {
            Ok(TriggerEvse201 {
                id: u32::try_from(evse["id"].as_u64().ok_or(InvalidParameters)?)
                    .map_err(|_| InvalidParameters)?,
                connector_id: evse
                    .get("connectorId")
                    .map(|connector| {
                        u32::try_from(connector.as_u64().ok_or(InvalidParameters)?)
                            .map_err(|_| InvalidParameters)
                    })
                    .transpose()?,
            })
        })
        .transpose()?;
    let expected_targets = if class.is_station_only() {
        vec![TriggerTarget201::Station]
    } else if class.is_certificate() {
        // SignCertificateRequest carries no EVSE identity; station receipt cannot
        // prove a scoped EVSE even though the requested target remains that EVSE.
        vec![
            native_scope.map_or(TriggerTarget201::Station, |evse| TriggerTarget201::Evse {
                id: evse.id,
            }),
        ]
    } else if class == TriggerMessageClass201::StatusNotification {
        let evse = native_scope.ok_or(InvalidParameters)?;
        vec![TriggerTarget201::Connector {
            id: evse.id,
            connector_id: evse.connector_id.ok_or(InvalidParameters)?,
        }]
    } else if let Some(evse) = native_scope {
        if class == TriggerMessageClass201::TransactionEvent
            && let Some(connector_id) = evse.connector_id
        {
            vec![TriggerTarget201::Connector {
                id: evse.id,
                connector_id,
            }]
        } else {
            vec![TriggerTarget201::Evse { id: evse.id }]
        }
    } else {
        if resource != &snapshot.station {
            return Err(InvalidParameters);
        }
        let mut ids = snapshot
            .resources
            .iter()
            .filter_map(|entry| {
                if entry.resource.bridge_id != snapshot.station.bridge_id
                    || entry.resource.station_id != snapshot.station.station_id
                {
                    return None;
                }
                match entry.resource.native_protocol_reference {
                    Some(NativeProtocolReference::Ocpp201 {
                        evse_id: evse_id @ 1..,
                        ..
                    }) => Some(evse_id),
                    _ => None,
                }
            })
            .collect::<Vec<_>>();
        ids.sort_unstable();
        ids.dedup();
        if ids.is_empty() || ids.len() > 64 {
            return Err(InvalidParameters);
        }
        ids.into_iter()
            .map(|id| TriggerTarget201::Evse { id })
            .collect()
    };
    Ok((
        operation.payload.clone(),
        TriggerExpectation201 {
            requested_class: class,
            native_scope,
            expected_targets,
        },
    ))
}

pub(super) fn response(payload: &Value) -> CommandDispatchOutcome {
    let Some(fields) = payload.as_object() else {
        return super::mapping::uncertain();
    };
    if fields
        .keys()
        .any(|key| !["status", "statusInfo", "customData"].contains(&key.as_str()))
        || !fields.contains_key("status")
        || fields
            .get("customData")
            .is_some_and(|value| !valid_custom_data(value))
        || !status_info_valid(payload.get("statusInfo"))
    {
        return super::mapping::uncertain();
    }
    if serde_json::from_value::<TriggerMessageResponse>(payload.clone()).is_err() {
        return super::mapping::uncertain();
    }
    let Ok(status) = serde_json::from_value(payload["status"].clone()) else {
        return super::mapping::uncertain();
    };
    let status_info = match payload.get("statusInfo") {
        Some(value) => {
            let Some(reason_code) = value["reasonCode"].as_str() else {
                return super::mapping::uncertain();
            };
            Some(uob_contracts::TriggerStatusInfo201 {
                reason_code: reason_code.to_owned(),
                additional_info: value
                    .get("additionalInfo")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
            })
        }
        None => None,
    };
    CommandDispatchOutcome::TriggerResponse201(TriggerNativeResponse201 {
        status,
        status_info,
    })
}

fn valid_custom_data(value: &Value) -> bool {
    value.as_object().is_some_and(|fields| {
        fields
            .get("vendorId")
            .and_then(Value::as_str)
            .is_some_and(|vendor| vendor.chars().count() <= 255)
    })
}

fn status_info_valid(value: Option<&Value>) -> bool {
    value.is_none_or(|value| {
        value.as_object().is_some_and(|fields| {
            fields
                .keys()
                .all(|key| ["reasonCode", "additionalInfo", "customData"].contains(&key.as_str()))
                && fields
                    .get("reasonCode")
                    .and_then(Value::as_str)
                    .is_some_and(|reason| reason.chars().count() <= 20)
                && fields.get("additionalInfo").is_none_or(|info| {
                    info.as_str()
                        .is_some_and(|info| info.chars().count() <= 512)
                })
                && fields.get("customData").is_none_or(valid_custom_data)
        })
    })
}
