use uob_contracts::{
    CommandOperation, NativeProtocolReference, PrivilegedOcppOperation, ProtocolEdition,
    ResourceRef, SEND_LOCAL_LIST_REFERENCE_SCHEMA_201, SendLocalListReference201,
};

pub(super) fn valid<P: 'static>(
    resource: &ResourceRef,
    operation: &PrivilegedOcppOperation<P>,
) -> bool {
    if operation.protocol != ProtocolEdition::Ocpp201
        || resource.resource.is_some()
        || !matches!(
            resource.native_protocol_reference,
            None | Some(NativeProtocolReference::Ocpp201 {
                evse_id: 0,
                connector_id: None
            })
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
            operation.payload_schema.as_str() == SEND_LOCAL_LIST_REFERENCE_SCHEMA_201
                && serde_json::from_value::<SendLocalListReference201>(payload.clone())
                    .is_ok_and(|request| request.valid())
        }
        "GetLocalListVersion" => {
            operation.payload_schema.as_str() == "urn:OCPP:Cp:2:2020:3:GetLocalListVersionRequest"
                && payload.as_object().is_some_and(serde_json::Map::is_empty)
        }
        "ClearCache" => {
            operation.payload_schema.as_str() == "urn:OCPP:Cp:2:2020:3:ClearCacheRequest"
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
    evidence: &uob_contracts::LocalAuthorizationResult201,
) -> bool {
    let CommandOperation::Ocpp(operation) = &command.operation else {
        return false;
    };
    if !valid(&command.resource, operation) {
        return false;
    }
    match evidence {
        uob_contracts::LocalAuthorizationResult201::GetLocalListVersion { version_number } => {
            operation.action.as_str() == "GetLocalListVersion" && *version_number >= 0
        }
        uob_contracts::LocalAuthorizationResult201::ClearCache { .. } => {
            operation.action.as_str() == "ClearCache"
        }
        uob_contracts::LocalAuthorizationResult201::SendLocalList {
            version_number,
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
            serde_json::from_value::<SendLocalListReference201>(payload.clone()).is_ok_and(
                |request| {
                    request.version_number == *version_number && request.update_type == *update_type
                },
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::valid_evidence;
    use uob_contracts::*;

    #[test]
    fn query_evidence_requires_a_native_nonnegative_i32_version() {
        let command = Command {
            schema_version: ContractVersion::V1_INITIAL,
            request_id: RequestId::new("query").unwrap(),
            correlation_id: None,
            resource: ResourceRef {
                bridge_id: BridgeId::new("bridge").unwrap(),
                station_id: StationId::new("station").unwrap(),
                resource: None,
                native_protocol_reference: None,
            },
            operation: CommandOperation::Ocpp(PrivilegedOcppOperation {
                protocol: ProtocolEdition::Ocpp201,
                action: ProtocolActionName::new("GetLocalListVersion").unwrap(),
                payload_schema: PayloadSchemaId::new(
                    "urn:OCPP:Cp:2:2020:3:GetLocalListVersionRequest",
                )
                .unwrap(),
                payload: serde_json::json!({}),
            }),
            expires_at: serde_json::from_value(serde_json::json!("2099-01-01T00:00:00Z")).unwrap(),
            admitted_at: serde_json::from_value(serde_json::json!("2026-01-01T00:00:00Z")).unwrap(),
            origin: AuthenticatedCommandOrigin::Management {
                principal_id: PrincipalId::new("operator").unwrap(),
            },
        };
        for version_number in [0, 1, i32::MAX] {
            assert!(valid_evidence(
                &command,
                &LocalAuthorizationResult201::GetLocalListVersion { version_number },
            ));
        }
        for version_number in [-1, i32::MIN] {
            assert!(!valid_evidence(
                &command,
                &LocalAuthorizationResult201::GetLocalListVersion { version_number },
            ));
        }
    }
}
