use super::{CommandSchemaDescriptor, CommandSchemaField, connector_scope, station_scope};
use serde_json::Value;
use uob_contracts::{
    CommandErrorCode, Operation, PrivilegedOcppOperation, ProtocolEdition,
    RESERVE_NOW_REFERENCE_SCHEMA_16, ReserveNowReference16, ResourceRef, StationSnapshot,
    ValueType,
};
pub const ACTIONS: [&str; 2] = ["ReserveNow", "CancelReservation"];
pub const CANCEL_SCHEMA: &str = "urn:OCPP:1.6:2019:12:CancelReservationRequest";
pub enum Request {
    Reserve(ReserveNowReference16),
    Cancel(i32),
}
pub fn validate(
    resource: &ResourceRef,
    operation: &PrivilegedOcppOperation<Value>,
) -> Result<Request, CommandErrorCode> {
    let invalid = CommandErrorCode::InvalidParameters;
    if operation.protocol != ProtocolEdition::Ocpp16j {
        return Err(CommandErrorCode::UnsupportedOperation);
    }
    match operation.action.as_str() {
        "ReserveNow" if operation.payload_schema.as_str() == RESERVE_NOW_REFERENCE_SCHEMA_16 => {
            let request: ReserveNowReference16 =
                serde_json::from_value(operation.payload.clone()).map_err(|_| invalid)?;
            if (request.connector_id == 0 && !station_scope(resource))
                || (request.connector_id > 0
                    && connector_scope(resource) != Some(request.connector_id))
            {
                return Err(invalid);
            }
            Ok(Request::Reserve(request))
        }
        "CancelReservation"
            if operation.payload_schema.as_str() == CANCEL_SCHEMA && station_scope(resource) =>
        {
            if !operation
                .payload
                .as_object()
                .is_some_and(|o| o.len() == 1 && o.contains_key("reservationId"))
            {
                return Err(invalid);
            }
            let request: rust_ocpp::v1_6::messages::cancel_reservation::CancelReservationRequest =
                serde_json::from_value(operation.payload.clone()).map_err(|_| invalid)?;
            Ok(Request::Cancel(request.reservation_id))
        }
        _ => Err(invalid),
    }
}
fn field(name: &'static str, value_type: ValueType) -> CommandSchemaField {
    CommandSchemaField {
        name,
        value_type,
        required: true,
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
        for action in ACTIONS {
            if !capabilities.supports(&Operation::ProtocolAction {
                protocol: ProtocolEdition::Ocpp16j,
                action: action.to_owned(),
            }) {
                continue;
            }
            if action == "CancelReservation" && !station_scope(resource) {
                continue;
            }
            if action == "ReserveNow"
                && !station_scope(resource)
                && connector_scope(resource).is_none()
            {
                continue;
            }
            let fields = if action == "ReserveNow" {
                vec![
                    field("connectorId", ValueType::UnsignedInteger),
                    field("expiryDate", ValueType::Text),
                    field("reservationId", ValueType::SignedInteger),
                    field("reservationReference", ValueType::Text),
                ]
            } else {
                vec![field("reservationId", ValueType::SignedInteger)]
            };
            result.push(CommandSchemaDescriptor {
                resource: resource.clone(),
                protocol: ProtocolEdition::Ocpp16j,
                action,
                payload_schema: if action == "ReserveNow" {
                    RESERVE_NOW_REFERENCE_SCHEMA_16
                } else {
                    CANCEL_SCHEMA
                },
                fields,
            });
        }
    }
    result
}
