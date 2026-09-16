use super::support::Fixture;
use std::fs;
use uob_release_manager::supervisor::{
    Code, Events, Grant, Permission, Request, Response, Supervisor,
};

fn grant(uid: u32, permissions: &[Permission]) -> Grant {
    Grant {
        uid,
        permissions: permissions.to_vec(),
    }
}

fn events(response: Response) -> Events {
    assert_eq!(response.code, Code::Ok);
    assert!(response.status.is_none());
    response.events.expect("events response")
}

fn stage(manager: &mut Supervisor, digest: &str) {
    assert_eq!(
        manager
            .handle(
                100,
                Request::Stage {
                    digest: digest.into(),
                },
            )
            .code,
        Code::Ok
    );
}

#[test]
fn events_page_the_bounded_history_with_an_exclusive_cursor() {
    let fixture = Fixture::new();
    let digest = fixture.artifacts.digest().to_owned();
    let mut manager = fixture.manager(vec![grant(100, &[Permission::Read, Permission::Stage])]);

    for _ in 0..65 {
        stage(&mut manager, &digest);
    }

    let retained = events(manager.handle(100, Request::Events { after: 0 }));
    assert_eq!(retained.oldest_sequence, 2);
    assert_eq!(retained.latest_sequence, 65);
    assert!(retained.truncated);
    assert_eq!(retained.records.len(), 64);
    assert_eq!(
        retained.records.first().map(|record| record.sequence),
        Some(2)
    );
    assert_eq!(
        retained.records.last().map(|record| record.sequence),
        Some(65)
    );

    let page = events(manager.handle(100, Request::Events { after: 64 }));
    assert!(!page.truncated);
    assert_eq!(page.records.len(), 1);
    assert_eq!(page.records[0].sequence, 65);

    let exhausted = events(manager.handle(100, Request::Events { after: 65 }));
    assert_eq!(exhausted.oldest_sequence, 2);
    assert_eq!(exhausted.latest_sequence, 65);
    assert!(!exhausted.truncated);
    assert!(exhausted.records.is_empty());
}

#[test]
fn events_survive_restart_and_migrate_a_legacy_last_operation() {
    let fixture = Fixture::new();
    let digest = fixture.artifacts.digest().to_owned();
    let grants = vec![grant(100, &[Permission::Read, Permission::Stage])];
    let mut manager = fixture.manager(grants.clone());
    stage(&mut manager, &digest);
    stage(&mut manager, &digest);
    drop(manager);

    let mut manager = fixture.manager(grants.clone());
    let persisted = events(manager.handle(100, Request::Events { after: 0 }));
    assert_eq!(persisted.records.len(), 2);
    assert_eq!(persisted.records[0].sequence, 1);
    assert_eq!(persisted.records[1].sequence, 2);
    drop(manager);

    let state_path = fixture.state.join("state.json");
    let mut legacy: serde_json::Value =
        serde_json::from_slice(&fs::read(&state_path).unwrap()).unwrap();
    legacy.as_object_mut().unwrap().remove("history");
    fs::write(&state_path, serde_json::to_vec(&legacy).unwrap()).unwrap();

    let mut manager = fixture.manager(grants);
    let migrated = events(manager.handle(100, Request::Events { after: 0 }));
    assert_eq!(migrated.records.len(), 1);
    assert_eq!(migrated.records[0].sequence, 2);
    assert!(migrated.truncated);
}

#[test]
fn events_require_read_permission_and_do_not_persist_a_read() {
    let fixture = Fixture::new();
    assert_eq!(
        serde_json::from_str::<Request>(r#"{"operation":"events"}"#).unwrap(),
        Request::Events { after: 0 }
    );
    let mut manager = fixture.manager(vec![grant(100, &[Permission::Stage])]);
    let forbidden = manager.handle(100, Request::Events { after: 0 });
    assert_eq!(forbidden.code, Code::Forbidden);
    assert!(forbidden.events.is_none());
    assert!(!fixture.state.join("state.json").exists());
    drop(manager);

    let digest = fixture.artifacts.digest().to_owned();
    let mut manager = fixture.manager(vec![grant(100, &[Permission::Read, Permission::Stage])]);
    stage(&mut manager, &digest);
    let state_path = fixture.state.join("state.json");
    let before = fs::read(&state_path).unwrap();
    let snapshot = events(manager.handle(100, Request::Events { after: 0 }));
    assert_eq!(snapshot.records.len(), 1);
    assert_eq!(fs::read(&state_path).unwrap(), before);
}

#[test]
fn malformed_persisted_history_is_rejected() {
    let fixture = Fixture::new();
    let digest = fixture.artifacts.digest().to_owned();
    let grants = vec![grant(100, &[Permission::Read, Permission::Stage])];
    let mut manager = fixture.manager(grants.clone());
    stage(&mut manager, &digest);
    drop(manager);

    let state_path = fixture.state.join("state.json");
    let mut state: serde_json::Value =
        serde_json::from_slice(&fs::read(&state_path).unwrap()).unwrap();
    state["history"][0]["request"] = serde_json::json!({"operation": "events", "after": 0});
    fs::write(&state_path, serde_json::to_vec(&state).unwrap()).unwrap();

    assert!(
        Supervisor::open(
            &fixture.state,
            &fixture.store,
            fixture.artifacts.policy.clone(),
            grants,
        )
        .is_err()
    );
}
