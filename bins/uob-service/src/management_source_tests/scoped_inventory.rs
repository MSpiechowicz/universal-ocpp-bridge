use super::*;
use uob_application::AccessResourceScope;

#[tokio::test]
async fn caller_scoped_inventory_pages_never_use_another_stations_cursor() {
    let database = TestDatabase::new();
    let store = ManagementStore::open(database.path(), 8).unwrap();
    for name in ["a", "b", "c", "d"] {
        save_snapshot(&store, name).await;
    }
    let port = ScopedTargetQueryPort::new(source(&store), authorization(&["a", "b", "c", "d"]));
    let scopes = vec![
        AccessResourceScope::Station {
            bridge_id: station("b").bridge_id,
            station_id: station("b").station_id,
        },
        AccessResourceScope::Station {
            bridge_id: station("d").bridge_id,
            station_id: station("d").station_id,
        },
    ];
    let TargetQueryResult::StationSnapshots(first) = port
        .query(TargetQuery::StationSnapshotsScoped {
            query: SnapshotQuery {
                after: None,
                limit: one(),
            },
            scopes: scopes.clone(),
        })
        .await
        .unwrap()
    else {
        panic!("snapshot page expected");
    };
    assert_eq!(first.items, vec![snapshot("b")]);
    let cursor = first.next_cursor.expect("second authorized station");
    assert!(cursor.as_str().contains("\"b\""), "{cursor:?}");
    assert!(!cursor.as_str().contains("\"c\""), "{cursor:?}");
    let TargetQueryResult::StationSnapshots(second) = port
        .query(TargetQuery::StationSnapshotsScoped {
            query: SnapshotQuery {
                after: Some(cursor),
                limit: one(),
            },
            scopes,
        })
        .await
        .unwrap()
    else {
        panic!("snapshot page expected");
    };
    assert_eq!(second.items, vec![snapshot("d")]);
    assert!(second.next_cursor.is_none());
}
