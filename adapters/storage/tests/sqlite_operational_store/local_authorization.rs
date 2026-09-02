use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use uob_application::{
    AtomicStoreWrite, AuthorizationChange, AuthorizationDecision, AuthorizationDenialReason,
    AuthorizationGuardedCommandPort, AuthorizationReference, AuthorizationState,
    CommandAdmissionError, CommandAdmissionErrorCode, CommandAdmissionFuture, CommandAdmissionPort,
    LocalAuthorizationPolicy, LocalAuthorizationService, OperationalStore, PageLimit,
    RecoveryQuery,
};
use uob_contracts::{
    AuthenticatedCommandOrigin, CommandOperation, CommandRequest, ExternalCommand, PrincipalId,
    RequestId, TargetInstanceId,
};

use super::{Store, TestDatabase, block_on, resource, text, timestamp};

#[test]
fn expiry_and_revocation_survive_restart() {
    let database = TestDatabase::new();
    let store = Store::open(database.path(), 8).expect("open SQLite store");
    let mut write = AtomicStoreWrite::empty();
    write.authorization_changes = vec![AuthorizationChange {
        reference: text(AuthorizationReference::new, "sha256:allowed"),
        resource: resource(),
        state: AuthorizationState::Active,
        revision: 1,
        changed_at: timestamp(0),
        expires_at: Some(timestamp(5)),
    }];
    block_on(store.write_atomic(write)).expect("persist allowlist entry");
    drop(store);

    let reopened = Store::open(database.path(), 8).expect("reopen SQLite store");
    let recovered = block_on(reopened.recover(RecoveryQuery {
        limit: PageLimit::new(10).expect("recovery limit"),
    }))
    .expect("recover authorization policy");
    let policy =
        LocalAuthorizationPolicy::restore(recovered.authorization).expect("valid revisions");
    let reference = text(AuthorizationReference::new, "sha256:allowed");
    assert!(matches!(
        policy.decide(&reference, &resource(), timestamp(4)),
        AuthorizationDecision::Allowed { .. }
    ));
    assert_eq!(
        policy.decide(&reference, &resource(), timestamp(5)),
        AuthorizationDecision::Denied {
            reason: AuthorizationDenialReason::Expired
        }
    );

    let mut revocation = AtomicStoreWrite::empty();
    revocation.authorization_changes.push(AuthorizationChange {
        reference: reference.clone(),
        resource: resource(),
        state: AuthorizationState::Revoked,
        revision: 2,
        changed_at: timestamp(3),
        expires_at: None,
    });
    block_on(reopened.write_atomic(revocation)).expect("persist revocation");
    drop(reopened);
    let reopened = Store::open(database.path(), 8).expect("reopen after revocation");
    let recovered = block_on(reopened.recover(RecoveryQuery {
        limit: PageLimit::new(10).expect("recovery limit"),
    }))
    .expect("recover revoked policy");
    let policy = LocalAuthorizationPolicy::restore(recovered.authorization).expect("valid policy");
    assert_eq!(
        policy.decide(&reference, &resource(), timestamp(4)),
        AuthorizationDecision::Denied {
            reason: AuthorizationDenialReason::Revoked
        }
    );
}

#[test]
fn management_and_target_payloads_cannot_bypass_local_start_policy() {
    let database = TestDatabase::new();
    let store = Arc::new(Store::open(database.path(), 8).expect("open SQLite store"));
    let service = Arc::new(
        block_on(LocalAuthorizationService::recover(
            store,
            PageLimit::new(10).expect("limit"),
        ))
        .expect("recover empty policy"),
    );
    let calls = Arc::new(AtomicUsize::new(0));
    let guard = AuthorizationGuardedCommandPort::new(
        Arc::new(CountingCommandPort(calls.clone())),
        service,
        Arc::new(|| timestamp(1)),
    );

    for (index, origin) in [
        AuthenticatedCommandOrigin::Management {
            principal_id: text(PrincipalId::new, "browser-operator"),
        },
        AuthenticatedCommandOrigin::Target {
            target_instance_id: text(TargetInstanceId::new, "target-main"),
            principal_id: text(PrincipalId::new, "target-operator"),
        },
    ]
    .into_iter()
    .enumerate()
    {
        let command = ExternalCommand::authenticated(
            CommandRequest {
                request_id: RequestId::new(format!("request-{index}")).expect("request ID"),
                correlation_id: None,
                resource: resource(),
                operation: CommandOperation::Start {
                    authorization_reference: Some("payload-forged-reference".to_owned()),
                },
                expires_at: timestamp(8),
            },
            origin,
        );
        let error = block_on(guard.submit(command)).expect_err("local policy must reject");
        assert_eq!(error.code(), CommandAdmissionErrorCode::PolicyRejected);
    }
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

struct CountingCommandPort(Arc<AtomicUsize>);

impl CommandAdmissionPort<String> for CountingCommandPort {
    fn submit(
        &self,
        _command: ExternalCommand<String>,
    ) -> CommandAdmissionFuture<'_, uob_contracts::CommandResult> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Box::pin(async {
            Err(CommandAdmissionError::new(
                CommandAdmissionErrorCode::Unavailable,
                "test.inner_called",
            ))
        })
    }
}
