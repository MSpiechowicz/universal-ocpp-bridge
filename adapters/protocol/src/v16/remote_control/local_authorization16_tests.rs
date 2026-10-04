use super::*;
use crate::v16::remote_control::{ProtectedLocalListUpdate16, ProtectedLocalListValue16};
use serde_json::json;
use std::sync::atomic::{AtomicUsize, Ordering};
use uob_contracts::{BridgeId, StationId};

fn instant(text: &str) -> UtcTimestamp {
    serde_json::from_value(json!(text)).unwrap()
}
struct Clock(parking_lot::Mutex<UtcTimestamp>);
impl CommandClock for Clock {
    fn now(&self) -> UtcTimestamp {
        *self.0.lock()
    }
}
fn deferred() -> (DeferredLocalAuthorizationCall16, Arc<Clock>) {
    let clock = Arc::new(Clock(parking_lot::Mutex::new(instant(
        "2026-09-01T00:00:00Z",
    ))));
    let resource = ResourceRef {
        bridge_id: BridgeId::new("bridge").unwrap(),
        station_id: StationId::new("station").unwrap(),
        resource: None,
        native_protocol_reference: None,
    };
    let reference = format!("list16:{}", "b".repeat(64));
    let provider = Arc::new(LocalAuthorizationUpdates16::new(vec![ProtectedLocalListValue16 {
        resource: resource.clone(), reference: reference.clone(), expires_at: instant("2026-09-02T00:00:00Z"),
        update: ProtectedLocalListUpdate16::from_json_bytes(json!({"listVersion":1,"updateType":"Full","localAuthorizationList":[{"idTag":"\"\\\n","idTagInfo":{"status":"Accepted"}}]}).to_string().into_bytes()).unwrap(),
    }]).unwrap());
    (
        DeferredLocalAuthorizationCall16 {
            provider,
            clock: clock.clone(),
            resource,
            request: SendLocalListReference16 {
                list_version: 1,
                update_type: LocalListUpdateType16::Full,
                update_reference: reference,
            },
            limits: Arc::new(Mutex::new(LocalListLimits16::default())),
            active: Arc::new(Mutex::new(true)),
            expires_at: instant("2026-09-03T00:00:00Z"),
        },
        clock,
    )
}

#[tokio::test]
async fn escaped_frame_size_matches_real_message_with_escaped_message_id() {
    let (deferred, _) = deferred();
    let id = "message\"\\\n";
    let reserved = deferred.wire_size(id).unwrap();
    let actual = Arc::new(AtomicUsize::new(0));
    let measured = actual.clone();
    let sent = deferred
        .send_with(id, move |message| {
            let axum::extract::ws::Message::Text(text) = message else {
                panic!("text wire");
            };
            measured.store(text.len(), Ordering::SeqCst);
            let frame: Value = serde_json::from_str(&text).unwrap();
            assert_eq!(frame[1], id);
            assert_eq!(frame[3]["localAuthorizationList"][0]["idTag"], "\"\\\n");
            std::future::ready(Ok(()))
        })
        .await;
    assert!(sent.unwrap().is_ok());
    assert_eq!(reserved, actual.load(Ordering::SeqCst));
}
#[tokio::test]
async fn expiry_revocation_limits_and_generation_rechecked_at_first_poll() {
    for change in 0..6 {
        let (deferred, clock) = deferred();
        assert!(deferred.wire_size("admitted").is_some());
        let called = Arc::new(AtomicUsize::new(0));
        let observed = called.clone();
        let send = deferred.send_with("admitted", move |_| {
            observed.fetch_add(1, Ordering::SeqCst);
            std::future::ready(Ok(()))
        });
        match change {
            0 => *clock.0.lock() = instant("2026-09-02T00:00:00Z"),
            1 => deferred
                .provider
                .revoke(&deferred.request.update_reference)
                .unwrap(),
            2 => *deferred.active.lock().expect("generation guard") = false,
            3 => deferred.limits.lock().expect("limit guard").send_max = Some(0),
            4 => deferred.limits.lock().expect("limit guard").list_max = Some(0),
            _ => deferred.limits.lock().expect("limit guard").enabled = Some(false),
        }
        assert!(send.await.is_none());
        assert_eq!(called.load(Ordering::SeqCst), 0);
        assert!(deferred.wire_size("admitted").is_none());
    }
}
#[test]
fn differential_capacity_does_not_invent_final_station_cardinality() {
    let limits = LocalListLimits16 {
        enabled: None,
        send_max: Some(2),
        list_max: Some(0),
    };
    assert!(limits.allows(LocalListUpdateType16::Differential, 2));
    assert!(!limits.allows(LocalListUpdateType16::Differential, 3));
    assert!(!limits.allows(LocalListUpdateType16::Full, 1));
    assert!(limits.allows(LocalListUpdateType16::Full, 0));
}
