use super::*;
use uob_application::capture::{
    CaptureFilter, CaptureGrant, CaptureLevel, CaptureManager, CapturePermission,
};
use uob_contracts::BridgeId;

#[tokio::test]
async fn stalled_notification_receiver_cannot_pin_capture_or_subscriber_slot() {
    let manager = CaptureManager::new(true);
    let filter = CaptureFilter {
        bridge: BridgeId::new("bridge").unwrap(),
        station: None,
        target: None,
    };
    let grant = CaptureGrant::new(
        filter.bridge.clone(),
        vec![CapturePermission::Read, CapturePermission::Capture],
        None,
        None,
    )
    .unwrap();
    let status = manager
        .start(&grant, filter, CaptureLevel::Metadata, None)
        .unwrap();
    let shared = Arc::new(Mutex::new(Shared {
        lease: Some(manager.lease(&grant, status.id, false).unwrap()),
        terminal: "expiry",
    }));
    let (sender, mut receiver) = mpsc::channel(1);
    tokio::time::timeout(
        Duration::from_secs(1),
        notify(shared.clone(), sender, Duration::from_millis(30)),
    )
    .await
    .unwrap();
    assert!(shared.lock().unwrap().lease.is_none());
    assert_eq!(shared.lock().unwrap().terminal, "slow_reader");
    assert_eq!(receiver.recv().await, Some(()));
    assert_eq!(receiver.recv().await, None);
    let first = manager.lease(&grant, status.id, false).unwrap();
    let second = manager.lease(&grant, status.id, false).unwrap();
    drop((first, second));
    manager.stop(&grant, status.id).unwrap();
    assert!(manager.wake_after(status.id).is_none());
}
