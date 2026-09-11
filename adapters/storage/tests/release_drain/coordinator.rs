use super::*;
use std::{
    sync::atomic::{AtomicUsize, Ordering},
    time::Duration as Window,
};
use uob_application::{
    CommandAdmissionPort, CommandClock, CommandCoordinator, CommandDispatchOutcome,
    StationCommandContext, StationCommandFuture, StationCommandPort,
    release_drain::ReleaseDrainPort,
};
use uob_contracts::{
    Connectivity, Operation, ProtocolEdition, ResourceCapabilities, SupportedOperation,
};
struct Peer(AtomicUsize);
impl StationCommandPort<String> for Peer {
    fn context(&self, _: ResourceRef) -> StationCommandFuture<'_, Option<StationCommandContext>> {
        Box::pin(async {
            Ok(Some(StationCommandContext {
                connectivity: Connectivity::Connected {
                    protocol: ProtocolEdition::Ocpp16j,
                    connected_at: timestamp(0),
                    last_message_at: None,
                },
                capabilities: ResourceCapabilities {
                    operations: vec![SupportedOperation {
                        operation: Operation::Start,
                        parameters: vec![],
                    }],
                    ..ResourceCapabilities::default()
                },
            }))
        })
    }
    fn dispatch(&self, _: Command<String>) -> StationCommandFuture<'_, CommandDispatchOutcome> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Box::pin(async {
            Ok(CommandDispatchOutcome::ProtocolResponse {
                accepted: true,
                error: None,
            })
        })
    }
}
struct Clock;
impl CommandClock for Clock {
    fn now(&self) -> UtcTimestamp {
        timestamp(0)
    }
}

#[tokio::test]
async fn two_application_ingresses_reject_during_drain_before_socket_dispatch() {
    let database = TestDatabase::new();
    let store = Arc::new(Store::open(database.path(), 8).unwrap());
    let peer = Arc::new(Peer(AtomicUsize::new(0)));
    let management = CommandCoordinator::new(store.clone(), peer.clone(), Arc::new(Clock));
    let target = CommandCoordinator::new(store.clone(), peer.clone(), Arc::new(Clock));
    let id = store.begin_drain(Window::from_secs(2)).await.unwrap();
    let origins = [
        AuthenticatedCommandOrigin::Management {
            principal_id: PrincipalId::new("management").unwrap(),
        },
        AuthenticatedCommandOrigin::Target {
            principal_id: PrincipalId::new("target").unwrap(),
            target_instance_id: uob_contracts::TargetInstanceId::new("ems").unwrap(),
        },
    ];
    for (ingress, origin) in [management, target].iter().zip(origins) {
        let request = ExternalCommand::authenticated(
            CommandRequest {
                request_id: RequestId::new("start").unwrap(),
                correlation_id: None,
                resource: resource("station-a"),
                operation: start(None),
                expires_at: timestamp(2),
            },
            origin,
        );
        let error = ingress.submit(request).await.unwrap_err();
        assert_eq!(
            error.code(),
            uob_application::CommandAdmissionErrorCode::Busy
        );
    }
    assert_eq!(peer.0.load(Ordering::SeqCst), 0);
    assert!(
        store
            .command_by_request_id(RequestId::new("start").unwrap())
            .await
            .unwrap()
            .is_none()
    );
    store.cancel_drain(id).await.unwrap();
}
