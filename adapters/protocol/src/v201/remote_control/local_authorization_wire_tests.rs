use super::*;
use crate::v201::remote_control::{ProtectedLocalListUpdate201, ProtectedLocalListValue201};
use serde_json::json;
use std::sync::atomic::{AtomicUsize, Ordering};
use uob_contracts::{BridgeId, LocalListUpdateType201, StationId};

fn instant(text: &str) -> UtcTimestamp {
    serde_json::from_value(json!(text)).unwrap()
}
struct Clock(parking_lot::Mutex<UtcTimestamp>);
impl CommandClock for Clock {
    fn now(&self) -> UtcTimestamp {
        *self.0.lock()
    }
}
fn deferred() -> (DeferredLocalAuthorizationCall201, Arc<Clock>) {
    let clock = Arc::new(Clock(parking_lot::Mutex::new(instant(
        "2026-09-01T00:00:00Z",
    ))));
    let resource = ResourceRef {
        bridge_id: BridgeId::new("bridge").unwrap(),
        station_id: StationId::new("station").unwrap(),
        resource: None,
        native_protocol_reference: None,
    };
    let reference = format!("list201:{}", "b".repeat(64));
    let provider = Arc::new(LocalAuthorizationUpdates201::new(vec![ProtectedLocalListValue201 {
        resource: resource.clone(), reference: reference.clone(), expires_at: instant("2026-09-02T00:00:00Z"),
        update: ProtectedLocalListUpdate201::from_json_bytes(json!({"versionNumber":1,"updateType":"Full","localAuthorizationList":[{"idToken":{"idToken":"native@token","type":"Local"},"idTokenInfo":{"status":"Accepted","personalMessage":{"format":"UTF8","content":"\"\\\n"}}}]}).to_string().into_bytes()).unwrap(),
    }]).unwrap());
    (
        DeferredLocalAuthorizationCall201 {
            provider: Some(provider),
            clock: clock.clone(),
            resource,
            request: Some(SendLocalListReference201 {
                version_number: 1,
                update_type: LocalListUpdateType201::Full,
                update_reference: reference,
            }),
            action: "SendLocalList",
            authority: Arc::new(Mutex::new(true)),
            limits: Arc::new(Mutex::new(LocalListLimits201::default())),
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
            assert_eq!(
                frame[3]["localAuthorizationList"][0]["idTokenInfo"]["personalMessage"]["content"],
                "\"\\\n"
            );
            std::future::ready(Ok(()))
        })
        .await;
    assert!(sent.unwrap().is_ok());
    assert_eq!(reserved, actual.load(Ordering::SeqCst));
}
#[tokio::test]
async fn expiry_revocation_limits_and_generation_rechecked_at_first_poll() {
    for change in 0..10 {
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
                .as_ref()
                .unwrap()
                .revoke(&deferred.request.as_ref().unwrap().update_reference)
                .unwrap(),
            2 => *deferred.active.lock().expect("generation guard") = false,
            3 => deferred.limits.lock().expect("limit guard").items = Some(0),
            4 => deferred.limits.lock().expect("limit guard").capacity = Some(0),
            5 => deferred.limits.lock().expect("limit guard").enabled = Some(false),
            6 => *deferred.authority.lock().unwrap() = false,
            7 => deferred.limits.lock().unwrap().bytes = Some(1),
            8 => deferred.limits.lock().unwrap().available = Some(false),
            _ => *clock.0.lock() = instant("2026-09-03T00:00:00Z"),
        }
        assert!(send.await.is_none());
        assert_eq!(called.load(Ordering::SeqCst), 0);
        assert!(deferred.wire_size("admitted").is_none());
    }
}
#[test]
fn differential_capacity_does_not_invent_final_station_cardinality() {
    let limits = LocalListLimits201 {
        enabled: None,
        items: Some(2),
        capacity: Some(0),
        ..Default::default()
    };
    assert!(limits.allows(2, 0));
    assert!(!limits.allows(3, 0));
    assert!(!limits.allows(1, 1));
    assert!(limits.allows(0, 0));
}
#[tokio::test]
async fn query_and_cache_revocation_recheck_at_first_socket_poll() {
    for action in ["GetLocalListVersion", "ClearCache"] {
        for change in 0..4 {
            let (mut deferred, clock) = deferred();
            deferred.request = None;
            deferred.provider = None;
            deferred.action = action;
            let invoked = Arc::new(AtomicUsize::new(0));
            let observed = invoked.clone();
            let send = deferred.send_with("queued-read-or-cache", move |_| {
                observed.fetch_add(1, Ordering::SeqCst);
                std::future::ready(Ok(()))
            });
            match change {
                0 => *deferred.authority.lock().unwrap() = false,
                1 => *deferred.active.lock().unwrap() = false,
                2 => *clock.0.lock() = deferred.expires_at,
                _ if action == "ClearCache" => {
                    deferred.limits.lock().unwrap().cache_enabled = Some(false);
                }
                _ => *deferred.active.lock().unwrap() = false,
            }
            assert!(send.await.is_none());
            assert_eq!(invoked.load(Ordering::SeqCst), 0);
        }
    }
}
#[tokio::test]
async fn complete_call_limit_includes_native_metadata_and_real_escaped_envelope() {
    let limit = uob_contracts::LOCAL_AUTHORIZATION_BYTES_LIMIT_201;
    let mut native = json!({"versionNumber":1,"updateType":"Full","customData":{"vendorId":"vendor","padding":""}});
    let native_base = serde_json::to_vec(&native).unwrap().len();
    let overhead = serde_json::to_vec(&(2, "id", "SendLocalList", &native))
        .unwrap()
        .len()
        - native_base;
    for extra in [0, overhead] {
        native["customData"]["padding"] = json!("x".repeat(limit - native_base - extra));
        let (mut deferred, _) = deferred();
        let entry = ProtectedLocalListValue201 {
            resource: deferred.resource.clone(),
            reference: deferred
                .request
                .as_ref()
                .expect("Send reference")
                .update_reference
                .clone(),
            expires_at: instant("2026-09-02T00:00:00Z"),
            update: ProtectedLocalListUpdate201::from_json_bytes(
                serde_json::to_vec(&native).unwrap(),
            )
            .unwrap(),
        };
        deferred.provider = Some(Arc::new(
            LocalAuthorizationUpdates201::new(vec![entry]).unwrap(),
        ));
        if extra == 0 {
            assert_eq!(serde_json::to_vec(&native).unwrap().len(), limit);
            assert!(deferred.wire_size("id").is_none());
        } else {
            assert_eq!(deferred.wire_size("id"), Some(limit));
            let sent = deferred
                .send_with("id", move |message| {
                    let axum::extract::ws::Message::Text(text) = message else {
                        panic!("text wire");
                    };
                    assert_eq!(text.len(), limit);
                    std::future::ready(Ok(()))
                })
                .await;
            assert!(sent.expect("bounded native send").is_ok());
            assert!(deferred.wire_size("id\"\\\n").is_none());
        }
    }
}

fn differential(upserts: usize, deletions: usize) -> DeferredLocalAuthorizationCall201 {
    let (mut deferred, _) = deferred();
    let mut entries = Vec::new();
    for index in 0..upserts + deletions {
        let mut entry = json!({"idToken":{"idToken":format!("identity-{index}"),"type":"Local"}});
        if index < upserts {
            entry["idTokenInfo"] = json!({"status":"Accepted"});
        }
        entries.push(entry);
    }
    deferred.request.as_mut().expect("reference").update_type =
        LocalListUpdateType201::Differential;
    let value = ProtectedLocalListValue201 {
        resource: deferred.resource.clone(),
        reference: deferred
            .request
            .as_ref()
            .expect("reference")
            .update_reference
            .clone(),
        expires_at: instant("2026-09-02T00:00:00Z"),
        update: ProtectedLocalListUpdate201::from_json_bytes(
            serde_json::to_vec(&json!({"versionNumber":1,"updateType":"Differential",
                "localAuthorizationList":entries}))
            .unwrap(),
        )
        .expect("validated unique native identities"),
    };
    deferred.provider = Some(Arc::new(
        LocalAuthorizationUpdates201::new(vec![value]).unwrap(),
    ));
    deferred
}

#[tokio::test]
async fn differential_unique_upsert_lower_bound_rechecks_capacity_before_socket_poll() {
    for (upserts, deletions, allowed) in [(3, 0, false), (0, 3, true), (2, 3, true)] {
        let deferred = differential(upserts, deletions);
        assert!(deferred.wire_size("queued").is_some());
        let invoked = Arc::new(AtomicUsize::new(0));
        let observed = invoked.clone();
        let send = deferred.send_with("queued", move |message| {
            observed.fetch_add(1, Ordering::SeqCst);
            let axum::extract::ws::Message::Text(text) = message else {
                panic!("text wire");
            };
            let frame: Value = serde_json::from_str(&text).unwrap();
            assert_eq!(
                frame[3]["localAuthorizationList"].as_array().unwrap().len(),
                upserts + deletions
            );
            std::future::ready(Ok(()))
        });
        deferred.limits.lock().expect("limit guard").capacity = Some(2);
        let outcome = send.await;
        assert_eq!(outcome.is_some(), allowed);
        assert_eq!(invoked.load(Ordering::SeqCst), usize::from(allowed));
        if let Some(result) = outcome {
            assert!(result.is_ok());
        }
    }
}

#[tokio::test]
async fn differential_deletions_still_respect_smaller_learned_items_and_bytes() {
    for items_limit in [true, false] {
        let deferred = differential(0, 3);
        deferred.limits.lock().expect("limit guard").capacity = Some(2);
        let bytes = deferred
            .wire_size("queued")
            .expect("deletions do not consume capacity");
        let invoked = Arc::new(AtomicUsize::new(0));
        let observed = invoked.clone();
        let send = deferred.send_with("queued", move |_| {
            observed.fetch_add(1, Ordering::SeqCst);
            std::future::ready(Ok(()))
        });
        {
            let mut limits = deferred.limits.lock().expect("limit guard");
            if items_limit {
                limits.items = Some(2);
            } else {
                limits.bytes = Some(bytes - 1);
            }
        }
        assert!(send.await.is_none());
        assert_eq!(invoked.load(Ordering::SeqCst), 0);
        assert!(deferred.wire_size("queued").is_none());
    }
}
