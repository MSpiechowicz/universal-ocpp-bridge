use uob_contracts::{
    CommandOperation, NativeProtocolReference, PrivilegedOcppOperation, ProtocolEdition,
    ResourceRef, SEND_LOCAL_LIST_REFERENCE_SCHEMA_16, SendLocalListReference16,
};

pub(super) fn valid<P: 'static>(
    resource: &ResourceRef,
    operation: &PrivilegedOcppOperation<P>,
) -> bool {
    if operation.protocol != ProtocolEdition::Ocpp16j
        || resource.resource.is_some()
        || !matches!(
            resource.native_protocol_reference,
            None | Some(NativeProtocolReference::Ocpp16 { connector_id: 0 })
        )
    {
        return false;
    }
    let Some(payload) =
        (&operation.payload as &dyn std::any::Any).downcast_ref::<serde_json::Value>()
    else {
        return false;
    };
    match operation.action.as_str() {
        "SendLocalList" => {
            operation.payload_schema.as_str() == SEND_LOCAL_LIST_REFERENCE_SCHEMA_16
                && serde_json::from_value::<SendLocalListReference16>(payload.clone())
                    .is_ok_and(|request| request.valid())
        }
        "GetLocalListVersion" => {
            operation.payload_schema.as_str() == "urn:OCPP:1.6:2019:12:GetLocalListVersionRequest"
                && payload.as_object().is_some_and(serde_json::Map::is_empty)
        }
        "ClearCache" => {
            operation.payload_schema.as_str() == "urn:OCPP:1.6:2019:12:ClearCacheRequest"
                && payload.as_object().is_some_and(serde_json::Map::is_empty)
        }
        _ => true,
    }
}
pub(super) fn invalid<P: 'static>(resource: &ResourceRef, operation: &CommandOperation<P>) -> bool {
    matches!(operation, CommandOperation::Ocpp(operation)
        if ["SendLocalList", "GetLocalListVersion", "ClearCache"].contains(&operation.action.as_str()) && !valid(resource, operation))
}

pub(super) fn valid_evidence<P: 'static>(
    command: &uob_contracts::Command<P>,
    evidence: &uob_contracts::LocalAuthorizationResult16,
) -> bool {
    let CommandOperation::Ocpp(operation) = &command.operation else {
        return false;
    };
    if !valid(&command.resource, operation) {
        return false;
    }
    match evidence {
        uob_contracts::LocalAuthorizationResult16::GetLocalListVersion { .. } => {
            operation.action.as_str() == "GetLocalListVersion"
        }
        uob_contracts::LocalAuthorizationResult16::ClearCache { .. } => {
            operation.action.as_str() == "ClearCache"
        }
        uob_contracts::LocalAuthorizationResult16::SendLocalList {
            list_version,
            update_type,
            ..
        } => {
            if operation.action.as_str() != "SendLocalList" {
                return false;
            }
            let Some(payload) =
                (&operation.payload as &dyn std::any::Any).downcast_ref::<serde_json::Value>()
            else {
                return false;
            };
            serde_json::from_value::<SendLocalListReference16>(payload.clone()).is_ok_and(
                |request| {
                    request.list_version == *list_version && request.update_type == *update_type
                },
            )
        }
    }
}
