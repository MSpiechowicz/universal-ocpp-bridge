use serde::{Deserialize, Serialize};
use time::{Date, Month, PrimitiveDateTime, Time, UtcOffset};
use uob_contracts::{
    AuthenticatedCommandOrigin, BridgeId, CapabilityError, Command, CommandErrorCode,
    CommandLifecycle, CommandOperation, CommandRequest, CommandResult, CommandValidationError,
    ContractVersion, ExternalCommand, Operation, PayloadSchemaId, PrincipalId,
    PrivilegedOcppOperation, ProtocolActionName, ProtocolEdition, RequestId, ResourceCapabilities,
    ResourceRef, StationId, SupportedOperation, TargetInstanceId, TransactionId, UtcTimestamp,
};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct BaseReportPayload {
    request_id: u64,
    report_base: String,
}

fn text<T, E: std::fmt::Debug>(constructor: impl FnOnce(String) -> Result<T, E>, value: &str) -> T {
    constructor(value.to_owned()).expect("valid fixture identity")
}

fn timestamp(minute: u8) -> UtcTimestamp {
    UtcTimestamp::new(
        PrimitiveDateTime::new(
            Date::from_calendar_date(2026, Month::September, 1).expect("fixture date"),
            Time::from_hms(14, minute, 0).expect("fixture time"),
        )
        .assume_offset(UtcOffset::UTC),
    )
}

fn station() -> ResourceRef {
    ResourceRef {
        bridge_id: text(BridgeId::new, "bridge-berlin-1"),
        station_id: text(StationId::new, "station-7"),
        resource: None,
        native_protocol_reference: None,
    }
}

#[test]
fn admitted_command_fixture_round_trips_with_distinct_resource_and_target_identities() {
    let fixture = include_str!("fixtures/command-start-v1.json");
    let command: Command<BaseReportPayload> =
        serde_json::from_str(fixture).expect("decode command fixture");

    assert_eq!(command.resource.station_id.as_str(), "station-7");
    assert!(matches!(
        &command.origin,
        AuthenticatedCommandOrigin::Target {
            target_instance_id,
            principal_id: _
        } if target_instance_id.as_str() == "main-ems"
    ));
    assert_eq!(
        serde_json::to_value(command).expect("encode command fixture"),
        serde_json::from_str::<serde_json::Value>(fixture).expect("JSON fixture")
    );
}

#[test]
fn request_payload_cannot_supply_authenticated_origin_or_unknown_operation() {
    let injected_origin = r#"{
        "request_id":"cmd-1",
        "resource":{"bridge_id":"bridge-berlin-1","station_id":"station-7"},
        "operation":{"kind":"start","parameters":{"authorization_reference":null}},
        "expires_at":"2026-09-01T14:01:00Z",
        "origin":{"kind":"management","principal_id":"attacker"}
    }"#;
    let unknown_operation = r#"{
        "request_id":"cmd-2",
        "resource":{"bridge_id":"bridge-berlin-1","station_id":"station-7"},
        "operation":{"kind":"mirror_observed_state","parameters":{}},
        "expires_at":"2026-09-01T14:01:00Z"
    }"#;

    assert!(serde_json::from_str::<CommandRequest<BaseReportPayload>>(injected_origin).is_err());
    assert!(serde_json::from_str::<CommandRequest<BaseReportPayload>>(unknown_operation).is_err());
}

#[test]
fn adapter_attaches_authenticated_origin_and_result_route() {
    let request = CommandRequest {
        request_id: text(RequestId::new, "cmd-target-7"),
        correlation_id: None,
        resource: station(),
        operation: CommandOperation::<BaseReportPayload>::Stop {
            transaction_id: text(TransactionId::new, "tx-7"),
        },
        expires_at: timestamp(2),
    };
    let origin = AuthenticatedCommandOrigin::Target {
        target_instance_id: text(TargetInstanceId::new, "main-ems"),
        principal_id: text(PrincipalId::new, "ems-operator-4"),
    };
    let command = ExternalCommand::authenticated(request, origin.clone()).admit(timestamp(0));

    assert_eq!(command.return_route().origin, origin);
    assert_eq!(command.return_route().request_id.as_str(), "cmd-target-7");
    assert_eq!(command.resource.station_id.as_str(), "station-7");
}

#[test]
fn expiry_and_unsupported_capabilities_fail_explicitly() {
    let request = CommandRequest {
        request_id: text(RequestId::new, "cmd-stop-7"),
        correlation_id: None,
        resource: station(),
        operation: CommandOperation::<BaseReportPayload>::Stop {
            transaction_id: text(TransactionId::new, "tx-7"),
        },
        expires_at: timestamp(1),
    };
    let command = ExternalCommand::authenticated(
        request,
        AuthenticatedCommandOrigin::Management {
            principal_id: text(PrincipalId::new, "console-operator"),
        },
    )
    .admit(timestamp(0));
    let start_only = ResourceCapabilities {
        operations: vec![SupportedOperation {
            operation: Operation::Start,
            parameters: Vec::new(),
        }],
        ..ResourceCapabilities::default()
    };

    assert_eq!(
        command.validate_for_dispatch(&start_only, timestamp(1)),
        Err(CommandValidationError::Expired)
    );
    assert_eq!(
        command.validate_for_dispatch(&start_only, timestamp(0)),
        Err(CommandValidationError::UnsupportedOperation(
            CapabilityError::UnsupportedOperation(Operation::Stop)
        ))
    );
}

#[test]
fn privileged_ocpp_payload_is_typed_and_requires_an_exact_advertised_action() {
    let operation = CommandOperation::Ocpp(PrivilegedOcppOperation {
        protocol: ProtocolEdition::Ocpp201,
        action: text(ProtocolActionName::new, "GetBaseReport"),
        payload_schema: text(PayloadSchemaId::new, "ocpp201.GetBaseReportRequest.ed4.v1"),
        payload: BaseReportPayload {
            request_id: 17,
            report_base: "full_inventory".to_owned(),
        },
    });
    let request = CommandRequest {
        request_id: text(RequestId::new, "cmd-report-17"),
        correlation_id: None,
        resource: station(),
        operation,
        expires_at: timestamp(2),
    };
    let command = ExternalCommand::authenticated(
        request,
        AuthenticatedCommandOrigin::Management {
            principal_id: text(PrincipalId::new, "admin-1"),
        },
    )
    .admit(timestamp(0));
    let wrong_action = ResourceCapabilities {
        operations: vec![SupportedOperation {
            operation: Operation::ProtocolAction {
                protocol: ProtocolEdition::Ocpp201,
                action: "SetVariables".to_owned(),
            },
            parameters: Vec::new(),
        }],
        ..ResourceCapabilities::default()
    };

    assert!(matches!(
        command.validate_for_dispatch(&wrong_action, timestamp(0)),
        Err(CommandValidationError::UnsupportedOperation(_))
    ));

    let encoded = serde_json::to_string(&command).expect("typed protocol operation");
    let injected_payload_field = encoded.replace(
        "\"report_base\":\"full_inventory\"",
        "\"report_base\":\"full_inventory\",\"raw_requester\":\"attacker\"",
    );
    assert!(
        serde_json::from_str::<Command<BaseReportPayload>>(&injected_payload_field).is_err(),
        "typed payload rejects fields outside its pinned shape"
    );
}

#[test]
fn result_fixtures_keep_protocol_response_and_observed_effect_separate() {
    let fixture = include_str!("fixtures/command-results-v1.json");
    let results: Vec<CommandResult> = serde_json::from_str(fixture).expect("result fixtures");

    assert!(matches!(results[0].lifecycle, CommandLifecycle::Admitted));
    assert!(matches!(
        results[1].lifecycle,
        CommandLifecycle::ProtocolResponse {
            accepted: true,
            error: None
        }
    ));
    assert!(results[1].observed_effects.is_empty());
    assert!(matches!(
        &results[2].lifecycle,
        CommandLifecycle::Rejected { error }
            if error.code == CommandErrorCode::Expired
    ));
    assert!(matches!(
        &results[3].lifecycle,
        CommandLifecycle::Rejected { error }
            if error.code == CommandErrorCode::UnsupportedOperation
    ));
    assert!(matches!(
        results[4].lifecycle,
        CommandLifecycle::TransmissionUncertain { .. }
    ));
    assert!(matches!(
        results[5].lifecycle,
        CommandLifecycle::ProtocolResponse {
            accepted: true,
            error: None
        }
    ));
    assert_eq!(results[5].observed_effects.len(), 1);
    assert!(matches!(
        &results[6].lifecycle,
        CommandLifecycle::ProtocolResponse {
            accepted: false,
            error: Some(error)
        } if error.code == CommandErrorCode::ProtocolRejected
    ));
    assert_eq!(
        serde_json::to_value(results).expect("re-encode result fixtures"),
        serde_json::from_str::<serde_json::Value>(fixture).expect("JSON fixture")
    );
}

#[test]
fn common_command_wire_shape_has_stable_contract_version() {
    let command: Command<BaseReportPayload> =
        serde_json::from_str(include_str!("fixtures/command-start-v1.json"))
            .expect("command fixture");
    assert_eq!(command.schema_version, ContractVersion::V1_INITIAL);
}
