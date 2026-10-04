use serde_json::{Value, json};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use std::task::{Context, Poll, Waker};
use uob_application::*;
use uob_contracts::*;

struct ForbiddenAdmission(AtomicUsize);
impl CommandAdmissionPort<Value> for ForbiddenAdmission {
    fn submit(&self, _: ExternalCommand<Value>) -> CommandAdmissionFuture<'_, CommandResult> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Box::pin(async { panic!("unauthorized command reached durable admission") })
    }
}
fn station(name: &str) -> ResourceRef {
    ResourceRef {
        bridge_id: BridgeId::new("bridge").unwrap(),
        station_id: StationId::new(name).unwrap(),
        resource: None,
        native_protocol_reference: None,
    }
}
fn origin() -> AuthenticatedCommandOrigin {
    AuthenticatedCommandOrigin::Management {
        principal_id: PrincipalId::new("operator").unwrap(),
    }
}
fn request(action: &str) -> ExternalCommand<Value> {
    let schema = if action == "SendLocalList" {
        SEND_LOCAL_LIST_REFERENCE_SCHEMA_16.to_owned()
    } else {
        format!("urn:OCPP:1.6:2019:12:{action}Request")
    };
    let payload = if action == "SendLocalList" {
        json!({"listVersion":-2,"updateType":"Full","updateReference":format!("list16:{}", "a".repeat(64))})
    } else {
        json!({})
    };
    ExternalCommand::authenticated(
        CommandRequest {
            request_id: RequestId::new(action).unwrap(),
            correlation_id: None,
            resource: station("station-a"),
            operation: CommandOperation::Ocpp(PrivilegedOcppOperation {
                protocol: ProtocolEdition::Ocpp16j,
                action: ProtocolActionName::new(action).unwrap(),
                payload_schema: PayloadSchemaId::new(schema).unwrap(),
                payload,
            }),
            expires_at: serde_json::from_value(json!("2099-01-01T00:00:00Z")).unwrap(),
        },
        origin(),
    )
}
#[test]
fn all_three_native_actions_need_the_exact_scoped_privileged_grant_before_admission() {
    let mut connector = station("station-a");
    connector.resource = Some(CanonicalResource::Connector {
        connector_id: CanonicalConnectorId::new("one").unwrap(),
    });
    connector.native_protocol_reference = Some(NativeProtocolReference::Ocpp16 { connector_id: 1 });
    for (permission, scope) in [
        (
            AccessPermission::Read,
            AccessResourceScope::Resource(station("station-a")),
        ),
        (
            AccessPermission::Control,
            AccessResourceScope::Resource(station("station-a")),
        ),
        (
            AccessPermission::PrivilegedControl,
            AccessResourceScope::Resource(station("station-b")),
        ),
        (
            AccessPermission::PrivilegedControl,
            AccessResourceScope::Resource(connector),
        ),
        (
            AccessPermission::PrivilegedControl,
            AccessResourceScope::Bridge(BridgeId::new("foreign").unwrap()),
        ),
    ] {
        let inner = Arc::new(ForbiddenAdmission(AtomicUsize::new(0)));
        let policy = AccessPolicy::single(
            AccessGrant::new(origin(), vec![permission], vec![scope]).unwrap(),
        );
        let admission = ScopedCommandAdmissionPort::new(inner.clone(), policy);
        for action in ["GetLocalListVersion", "SendLocalList", "ClearCache"] {
            let mut future = admission.submit(request(action));
            let Poll::Ready(result) = future
                .as_mut()
                .poll(&mut Context::from_waker(Waker::noop()))
            else {
                panic!("denied admission must not wait for persistence or dispatch");
            };
            assert_eq!(
                result.unwrap_err().code(),
                CommandAdmissionErrorCode::Unauthorized
            );
        }
        assert_eq!(inner.0.load(Ordering::SeqCst), 0);
    }
}
