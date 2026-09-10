use super::*;

#[test]
fn backup_includes_committed_wal_and_survives_concurrent_writes() {
    let root = std::env::temp_dir().join(format!("uob-backup-{}", uuid::Uuid::new_v4()));
    fs::create_dir(&root).unwrap();
    let source = root.join("live.db");
    let destination = root.join("backup.db");
    let live = Connection::open(&source).unwrap();
    live.execute_batch("PRAGMA journal_mode=WAL; PRAGMA wal_autocheckpoint=0; CREATE TABLE records(id INTEGER PRIMARY KEY, value BLOB); INSERT INTO records VALUES(1, zeroblob(1048576));").unwrap();
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let writer_stop = stop.clone();
    let writer_source = source.clone();
    let writer = std::thread::spawn(move || {
        let db = Connection::open(writer_source).unwrap();
        while !writer_stop.load(std::sync::atomic::Ordering::Relaxed) {
            db.execute("INSERT INTO records(value) VALUES(zeroblob(128))", [])
                .unwrap();
        }
    });
    let limits = Limits {
        expected_schema_version: 0,
        maximum_bytes: 16 * 1024 * 1024,
        timeout: Duration::from_secs(5),
    };
    let bytes = create(&source, &destination, limits).unwrap();
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    writer.join().unwrap();
    assert!(bytes > 1_048_576);
    let saved = Connection::open(&destination).unwrap();
    assert_eq!(
        saved
            .query_row("SELECT length(value) FROM records WHERE id=1", [], |r| r
                .get::<_, u32>(0))
            .unwrap(),
        1_048_576
    );
    live.execute("INSERT INTO records VALUES(-1, 'after backup')", [])
        .unwrap();
    assert_eq!(
        saved
            .query_row("SELECT count(*) FROM records WHERE id=-1", [], |r| r
                .get::<_, u32>(0))
            .unwrap(),
        0
    );
    assert!(create(&source, &destination, limits).is_err());
    let limited = root.join("limited.db");
    assert!(
        create(
            &source,
            &limited,
            Limits {
                maximum_bytes: 1,
                ..limits
            }
        )
        .is_err()
    );
    assert!(!limited.exists());
    assert!(
        create(
            &source,
            &limited,
            Limits {
                timeout: Duration::from_nanos(1),
                ..limits
            }
        )
        .is_err()
    );
    assert!(!limited.exists());
    assert!(
        create(
            &source,
            &limited,
            Limits {
                expected_schema_version: 99,
                ..limits
            }
        )
        .is_err()
    );
    assert!(!limited.exists());
    drop(saved);
    drop(live);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn corrupt_missing_or_symlink_sources_do_not_create_backups() {
    let root = std::env::temp_dir().join(format!("uob-backup-{}", uuid::Uuid::new_v4()));
    fs::create_dir(&root).unwrap();
    let source = root.join("bad.db");
    let output = root.join("backup.db");
    let limits = Limits {
        expected_schema_version: 0,
        maximum_bytes: 4096,
        timeout: Duration::from_secs(1),
    };
    assert!(create(&source, &output, limits).is_err());
    fs::write(&source, b"not a SQLite database").unwrap();
    assert!(create(&source, &output, limits).is_err());
    let link = root.join("link.db");
    std::os::unix::fs::symlink(&source, &link).unwrap();
    assert!(create(&link, &output, limits).is_err());
    assert!(!output.exists());
    fs::remove_dir_all(root).unwrap();
}
