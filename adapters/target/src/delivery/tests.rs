use super::*;

fn policy(completion: DeliverySemantic) -> DeliveryRetryPolicy {
    DeliveryRetryPolicy {
        completion,
        initial_backoff: Duration::from_secs(1),
        maximum_backoff: Duration::from_secs(8),
    }
}

#[test]
fn local_exposure_never_satisfies_named_peer_acknowledgement() {
    let exposed = DeliveryOutcome::LocallyExposed {
        surface: "ems.http.sse".to_owned(),
    };
    assert!(!completes(
        DeliverySemantic::NamedPeerAcknowledgement,
        &exposed
    ));
    assert!(completes(DeliverySemantic::LocalExposure, &exposed));
    assert!(!completes(
        DeliverySemantic::NamedPeerAcknowledgement,
        &DeliveryOutcome::Acknowledged {
            peer: String::new(),
            scope: uob_application::AcknowledgementScope("processing".to_owned()),
        }
    ));
}

#[test]
fn retry_backoff_is_exponential_and_bounded() {
    let start = UtcTimestamp::new(time::OffsetDateTime::UNIX_EPOCH);
    assert_eq!(
        retry_at(start, policy(DeliverySemantic::NamedPeerAcknowledgement), 0),
        UtcTimestamp::new(time::OffsetDateTime::UNIX_EPOCH + Duration::from_secs(1))
    );
    assert_eq!(
        retry_at(
            start,
            policy(DeliverySemantic::NamedPeerAcknowledgement),
            20
        ),
        UtcTimestamp::new(time::OffsetDateTime::UNIX_EPOCH + Duration::from_secs(8))
    );
}

#[tokio::test]
async fn stalled_delivery_is_joined_at_deadline_and_outer_cancellation_cannot_detach_it() {
    for cancel_outer in [false, true] {
        let (alive, mut dropped) = tokio::sync::oneshot::channel::<()>();
        let join = tokio::spawn(async move {
            let _alive = alive;
            std::future::pending().await
        });
        let task = TargetDeliveryWorkerTask {
            shutdown: None,
            join: Some(join),
        };
        if cancel_outer {
            assert!(
                tokio::time::timeout(Duration::from_millis(5), task.shutdown())
                    .await
                    .is_err()
            );
            assert!(
                tokio::time::timeout(Duration::from_secs(1), dropped)
                    .await
                    .unwrap()
                    .is_err()
            );
        } else {
            assert!(matches!(
                task.shutdown_with_deadline(Duration::from_millis(5)).await,
                Err(TargetDeliveryWorkerError::ShutdownDeadlineExceeded)
            ));
            assert!(matches!(
                dropped.try_recv(),
                Err(tokio::sync::oneshot::error::TryRecvError::Closed)
            ));
        }
    }
}
