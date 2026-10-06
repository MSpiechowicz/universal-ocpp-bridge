use std::{
    pin::pin,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    task::{Context, Poll, Waker},
};

use uob_application::{
    AccessGrant, AccessPermission, AccessPolicy, AccessResourceScope, CommandAdmissionError,
    CommandAdmissionErrorCode, CommandAdmissionFuture, CommandAdmissionPort,
    ScopedCommandAdmissionPort,
};
use uob_contracts::{
    AuthenticatedCommandOrigin, BridgeId, CommandOperation, CommandRequest, CommandResult,
    ExternalCommand, PrincipalId, RequestId, ResourceRef, StationId, UtcTimestamp,
};

struct CountingCommands(AtomicUsize);

impl CommandAdmissionPort<()> for CountingCommands {
    fn submit(&self, _command: ExternalCommand<()>) -> CommandAdmissionFuture<'_, CommandResult> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Box::pin(async {
            Err(CommandAdmissionError::new(
                CommandAdmissionErrorCode::Unavailable,
                "fixture",
            ))
        })
    }
}

fn bridge(value: &str) -> BridgeId {
    BridgeId::new(value).unwrap()
}

fn origin() -> AuthenticatedCommandOrigin {
    AuthenticatedCommandOrigin::Management {
        principal_id: PrincipalId::new("operator-a").unwrap(),
    }
}

fn grant(permissions: Vec<AccessPermission>, scope: AccessResourceScope) -> AccessGrant {
    AccessGrant::new(origin(), permissions, vec![scope]).unwrap()
}

#[test]
fn configuration_permissions_require_an_explicit_bridge_wide_grant() {
    let station_scoped = grant(
        vec![AccessPermission::ConfigurationRead],
        AccessResourceScope::Station {
            bridge_id: bridge("bridge-a"),
            station_id: StationId::new("a").unwrap(),
        },
    );
    assert!(
        !station_scoped.permits_bridge(AccessPermission::ConfigurationRead, &bridge("bridge-a"))
    );

    let reader = grant(
        vec![AccessPermission::Read, AccessPermission::ConfigurationRead],
        AccessResourceScope::Bridge(bridge("bridge-a")),
    );
    assert!(reader.permits_bridge(AccessPermission::ConfigurationRead, &bridge("bridge-a")));
    assert!(!reader.permits_bridge(AccessPermission::ConfigurationRead, &bridge("bridge-b")));
    for permission in [
        AccessPermission::ConfigurationWrite,
        AccessPermission::DestructiveDisposition,
    ] {
        assert!(!reader.permits_bridge(permission, &bridge("bridge-a")));
    }
}

#[test]
fn configuration_permissions_never_authorize_commands() {
    let inner = Arc::new(CountingCommands(AtomicUsize::new(0)));
    let administrator = grant(
        vec![
            AccessPermission::ConfigurationRead,
            AccessPermission::ConfigurationWrite,
            AccessPermission::DestructiveDisposition,
        ],
        AccessResourceScope::Bridge(bridge("bridge-a")),
    );
    let port = ScopedCommandAdmissionPort::new(inner.clone(), AccessPolicy::single(administrator));
    let command = ExternalCommand::authenticated(
        CommandRequest {
            request_id: RequestId::new("request-a").unwrap(),
            correlation_id: None,
            resource: ResourceRef {
                bridge_id: bridge("bridge-a"),
                station_id: StationId::new("a").unwrap(),
                resource: None,
                native_protocol_reference: None,
            },
            operation: CommandOperation::Start {
                authorization_reference: None,
            },
            expires_at: UtcTimestamp::new(time::OffsetDateTime::UNIX_EPOCH),
        },
        origin(),
    );
    let mut future = pin!(port.submit(command));
    let Poll::Ready(result) = future
        .as_mut()
        .poll(&mut Context::from_waker(Waker::noop()))
    else {
        panic!("access rejection is immediate");
    };
    assert_eq!(
        result.unwrap_err().code(),
        CommandAdmissionErrorCode::Unauthorized
    );
    assert_eq!(inner.0.load(Ordering::SeqCst), 0);
}
