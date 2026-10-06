use super::{CommandSchemaDescriptor, CommandSchemaField};
use serde_json::Value;
use std::sync::LazyLock;
use uob_contracts::{
    CANCEL_RESERVATION_SCHEMA_201, CanonicalResource, CommandErrorCode, NativeProtocolReference,
    Operation, PrivilegedOcppOperation, ProtocolEdition, RESERVE_NOW_REFERENCE_SCHEMA_201,
    ReserveNowReference201, ResourceRef, StationSnapshot, ValueType,
};
pub const ACTIONS: [&str; 2] = ["ReserveNow", "CancelReservation"];
const CONNECTOR_TYPES: [&str; 22] = [
    "cCCS1",
    "cCCS2",
    "cG105",
    "cTesla",
    "cType1",
    "cType2",
    "s309-1P-16A",
    "s309-1P-32A",
    "s309-3P-16A",
    "s309-3P-32A",
    "sBS1361",
    "sCEE-7-7",
    "sType2",
    "sType3",
    "Other1PhMax16A",
    "Other1PhOver16A",
    "Other3Ph",
    "Pan",
    "wInductive",
    "wResonant",
    "Undetermined",
    "Unknown",
];
const SCHEMAS: [&str; 5] = [
    include_str!("../../../../tests/ocpp-fixtures/corpus/schemas/2.0.1/ReserveNowRequest.json"),
    include_str!("../../../../tests/ocpp-fixtures/corpus/schemas/2.0.1/ReserveNowResponse.json"),
    include_str!(
        "../../../../tests/ocpp-fixtures/corpus/schemas/2.0.1/CancelReservationRequest.json"
    ),
    include_str!(
        "../../../../tests/ocpp-fixtures/corpus/schemas/2.0.1/CancelReservationResponse.json"
    ),
    include_str!(
        "../../../../tests/ocpp-fixtures/corpus/schemas/2.0.1/ReservationStatusUpdateRequest.json"
    ),
];
/// Pinned native shapes: 0 `ReserveNow` request, 1 its response, 2 `CancelReservation`
/// request, 3 its response, 4 the station's `ReservationStatusUpdate` request.
pub(crate) fn valid_native(index: usize, value: &Value) -> bool {
    static VALIDATORS: LazyLock<Vec<jsonschema::Validator>> = LazyLock::new(|| {
        SCHEMAS
            .iter()
            .map(|source| {
                let schema: Value = serde_json::from_str(source).expect("pinned native schema");
                jsonschema::options()
                    .should_validate_formats(true)
                    .build(&schema)
                    .expect("pinned native schema")
            })
            .collect()
    });
    VALIDATORS[index].is_valid(value)
}
pub enum Request {
    Reserve(ReserveNowReference201),
    Cancel(i32),
}
pub(crate) fn station_scope(resource: &ResourceRef) -> bool {
    resource.resource.is_none()
        && matches!(
            resource.native_protocol_reference,
            None | Some(NativeProtocolReference::Ocpp201 {
                evse_id: 0,
                connector_id: None
            })
        )
}
pub(crate) fn evse_scope(resource: &ResourceRef) -> Option<u32> {
    match (&resource.resource, resource.native_protocol_reference) {
        (
            Some(CanonicalResource::Evse {
                connector_id: None, ..
            }),
            Some(NativeProtocolReference::Ocpp201 {
                evse_id,
                connector_id: None,
            }),
        ) if evse_id > 0 && i32::try_from(evse_id).is_ok() => Some(evse_id),
        _ => None,
    }
}
pub fn validate(
    resource: &ResourceRef,
    operation: &PrivilegedOcppOperation<Value>,
) -> Result<Request, CommandErrorCode> {
    let invalid = CommandErrorCode::InvalidParameters;
    if operation.protocol != ProtocolEdition::Ocpp201 {
        return Err(CommandErrorCode::UnsupportedOperation);
    }
    match operation.action.as_str() {
        "ReserveNow" if operation.payload_schema.as_str() == RESERVE_NOW_REFERENCE_SCHEMA_201 => {
            let request: ReserveNowReference201 =
                serde_json::from_value(operation.payload.clone()).map_err(|_| invalid)?;
            // The EVSE is the resource; an absent evseId is only a station-scope reservation.
            let scoped = match request.evse_id {
                Some(evse) => evse_scope(resource) == Some(evse),
                None => station_scope(resource),
            };
            if !scoped {
                return Err(invalid);
            }
            Ok(Request::Reserve(request))
        }
        "CancelReservation"
            if operation.payload_schema.as_str() == CANCEL_RESERVATION_SCHEMA_201
                && station_scope(resource) =>
        {
            if !operation
                .payload
                .as_object()
                .is_some_and(|o| o.len() == 1 && o.contains_key("reservationId"))
                || !valid_native(2, &operation.payload)
            {
                return Err(invalid);
            }
            operation.payload["reservationId"]
                .as_i64()
                .and_then(|id| i32::try_from(id).ok())
                .map(Request::Cancel)
                .ok_or(invalid)
        }
        _ => Err(invalid),
    }
}
fn field(name: &'static str, value_type: ValueType, required: bool) -> CommandSchemaField {
    CommandSchemaField {
        name,
        value_type,
        required,
        enum_values: None,
    }
}
pub fn descriptors(snapshot: &StationSnapshot) -> Vec<CommandSchemaDescriptor> {
    let mut result = Vec::new();
    for (resource, capabilities) in std::iter::once((&snapshot.station, &snapshot.capabilities))
        .chain(
            snapshot
                .resources
                .iter()
                .map(|r| (&r.resource, &r.capabilities)),
        )
    {
        if resource.bridge_id != snapshot.station.bridge_id
            || resource.station_id != snapshot.station.station_id
        {
            continue;
        }
        let station = station_scope(resource);
        let evse = evse_scope(resource);
        for action in ACTIONS {
            if !capabilities.supports(&Operation::ProtocolAction {
                protocol: ProtocolEdition::Ocpp201,
                action: action.to_owned(),
            }) || (action == "CancelReservation" && !station)
                || (action == "ReserveNow" && !station && evse.is_none())
            {
                continue;
            }
            let fields = if action == "ReserveNow" {
                let mut fields = vec![
                    field("id", ValueType::SignedInteger, true),
                    field("expiryDateTime", ValueType::Text, true),
                ];
                if evse.is_some() {
                    fields.push(field("evseId", ValueType::UnsignedInteger, true));
                }
                fields.push(CommandSchemaField {
                    enum_values: Some(CONNECTOR_TYPES.to_vec()),
                    ..field("connectorType", ValueType::Text, false)
                });
                fields.push(field("reservationReference", ValueType::Text, true));
                fields
            } else {
                vec![field("reservationId", ValueType::SignedInteger, true)]
            };
            result.push(CommandSchemaDescriptor {
                resource: resource.clone(),
                protocol: ProtocolEdition::Ocpp201,
                action,
                payload_schema: if action == "ReserveNow" {
                    RESERVE_NOW_REFERENCE_SCHEMA_201
                } else {
                    CANCEL_RESERVATION_SCHEMA_201
                },
                fields,
            });
        }
    }
    result
}
