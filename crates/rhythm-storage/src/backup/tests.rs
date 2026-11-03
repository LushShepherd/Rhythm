use super::*;
use crate::ReflectionInput;
use rhythm_domain::{SessionOutcome, Settings};

fn note(store: &mut Store, body: &str) -> crate::ReflectionRevision {
    let draft = store
        .create_draft(ReflectionInput::thought(body), 10)
        .unwrap();
    store.finalize_draft(&draft.id, draft.version, 11).unwrap()
}

#[test]
fn backup_captures_committed_wal_and_never_overwrites() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("live.sqlite3");
    let backup = directory.path().join("backup.sqlite3");
    let mut store = Store::open(&path).unwrap();
    store
        .connection
        .pragma_update(None, "wal_autocheckpoint", 0)
        .unwrap();
    let revision = note(&mut store, "  English\n简体与繁體 🌿\r\n");
    store.backup_to(&backup).unwrap();
    assert_eq!(
        Store::open(&backup)
            .unwrap()
            .get_reflection(&revision.record_id)
            .unwrap()
            .unwrap()
            .heads,
        vec![revision]
    );
    let bytes = std::fs::read(&backup).unwrap();
    assert!(store.backup_to(&backup).is_err());
    assert_eq!(std::fs::read(&backup).unwrap(), bytes);
    assert!(store.backup_to(&path).is_err());
}

#[test]
fn restore_preserves_history_drafts_and_source_but_renews_identity() {
    let directory = tempfile::tempdir().unwrap();
    let backup = directory.path().join("backup.sqlite3");
    let mut source = Store::open_in_memory().unwrap();
    source.register_provider_binding("drive").unwrap();
    let revision = note(&mut source, "first version");
    let edit = source.edit_reflection(&revision.record_id, 12).unwrap();
    let edit = source
        .save_draft(&edit.id, edit.version, "修改文字\n還有明天", 13)
        .unwrap();
    let latest = source.finalize_draft(&edit.id, edit.version, 14).unwrap();
    let draft = source
        .create_draft(ReflectionInput::thought("unfinished words"), 15)
        .unwrap();
    source.backup_to(&backup).unwrap();
    let original_bytes = std::fs::read(&backup).unwrap();
    let mut destination = Store::open(directory.path().join("restored.sqlite3")).unwrap();
    note(&mut destination, "old destination");
    let previous_identity = destination.device_id().to_owned();
    destination.restore_from(&backup).unwrap();
    assert_eq!(
        destination.revision_history(&revision.record_id).unwrap(),
        vec![revision, latest]
    );
    assert_eq!(destination.list_drafts().unwrap(), vec![draft]);
    assert_eq!(destination.list_reflections(100, 0).unwrap().len(), 1);
    assert_ne!(destination.device_id(), source.device_id());
    assert_ne!(destination.device_id(), previous_identity);
    assert!(destination.outbox_items("drive").unwrap().is_empty());
    let added = note(&mut destination, "new device");
    assert_eq!(added.author_sequence, 1);
    assert_eq!(added.author_device_id, destination.device_id());
    assert_eq!(std::fs::read(&backup).unwrap(), original_bytes);
}

#[test]
fn restore_running_timer_requires_choice_and_advances_destination_generation() {
    let directory = tempfile::tempdir().unwrap();
    let backup = directory.path().join("timer.sqlite3");
    let mut source = Store::open_in_memory().unwrap();
    let mut engine = TimerEngine::new(Settings::default()).unwrap();
    let sample = |ms| ClockSample {
        epoch: "same-process".into(),
        monotonic_ms: ms,
        utc_ms: ms as i64,
    };
    engine
        .apply(
            Command::StartFocus {
                session_id: "active".into(),
                duration_ms: Some(60_000),
            },
            sample(0),
        )
        .unwrap();
    engine.apply(Command::Poll, sample(10_000)).unwrap();
    source.save_timer(None, &engine.checkpoint(), None).unwrap();
    source.backup_to(&backup).unwrap();
    let mut destination = Store::open_in_memory().unwrap();
    destination
        .save_timer(None, &TimerEngine::default().checkpoint(), None)
        .unwrap();
    let (restored, generation) = destination.restore_from(&backup).unwrap();
    assert_eq!(generation, 2);
    let TimerState::RecoveryRequired(active) = restored.state() else {
        panic!("restored running timer must wait");
    };
    assert_eq!(active.remaining_ms(), 50_000);
    assert_eq!(
        destination.load_timer().unwrap().unwrap().checkpoint,
        restored.checkpoint()
    );
    assert!(destination.list_sessions(100, 0).unwrap().is_empty());
}

#[test]
fn restore_preserves_terminal_sessions() {
    let directory = tempfile::tempdir().unwrap();
    let backup = directory.path().join("sessions.sqlite3");
    let mut source = Store::open_in_memory().unwrap();
    let mut engine = TimerEngine::default();
    let sample = |ms| ClockSample {
        epoch: "clock".into(),
        monotonic_ms: ms,
        utc_ms: ms as i64,
    };
    engine
        .apply(
            Command::StartFocus {
                session_id: "finished".into(),
                duration_ms: Some(10),
            },
            sample(0),
        )
        .unwrap();
    let terminal = engine.apply(Command::Poll, sample(10)).unwrap().unwrap();
    assert_eq!(terminal.outcome, SessionOutcome::Completed);
    source
        .save_timer(None, &engine.checkpoint(), Some(&terminal))
        .unwrap();
    source.backup_to(&backup).unwrap();
    let mut destination = Store::open_in_memory().unwrap();
    destination.restore_from(&backup).unwrap();
    assert_eq!(destination.list_sessions(100, 0).unwrap(), vec![terminal]);
}

#[test]
fn invalid_or_unsupported_restore_never_changes_current_journal() {
    let directory = tempfile::tempdir().unwrap();
    let mut destination = Store::open_in_memory().unwrap();
    let current = note(&mut destination, "keep these words");
    let identity = destination.device_id().to_owned();
    let invalid = directory.path().join("invalid.sqlite3");
    std::fs::write(&invalid, b"not SQLite").unwrap();
    assert!(destination.restore_from(&invalid).is_err());
    let empty = directory.path().join("empty.sqlite3");
    Connection::open(&empty).unwrap();
    assert!(destination.restore_from(&empty).is_err());
    let newer = directory.path().join("newer.sqlite3");
    let source = Store::open_in_memory().unwrap();
    source.backup_to(&newer).unwrap();
    let connection = Connection::open(&newer).unwrap();
    connection.pragma_update(None, "user_version", 999).unwrap();
    drop(connection);
    assert!(matches!(
        destination.restore_from(&newer),
        Err(Error::NewerSchema { .. })
    ));
    assert_eq!(
        destination
            .get_reflection(&current.record_id)
            .unwrap()
            .unwrap()
            .heads,
        vec![current]
    );
    assert_eq!(destination.device_id(), identity);
}

#[test]
fn restore_rejects_tampered_payload_and_schema() {
    let directory = tempfile::tempdir().unwrap();
    let backup = directory.path().join("corrupt.sqlite3");
    let mut source = Store::open_in_memory().unwrap();
    note(&mut source, "original");
    source.backup_to(&backup).unwrap();
    let connection = Connection::open(&backup).unwrap();
    connection.execute_batch("DROP TRIGGER immutable_revisions_update; UPDATE record_revisions SET payload_sha256=zeroblob(32); CREATE TRIGGER immutable_revisions_update BEFORE UPDATE ON record_revisions BEGIN SELECT RAISE(ABORT, 'revisions are immutable'); END;").unwrap();
    drop(connection);
    let mut destination = Store::open_in_memory().unwrap();
    assert!(destination.restore_from(&backup).is_err());
    let extra = directory.path().join("extra.sqlite3");
    source.backup_to(&extra).unwrap();
    let connection = Connection::open(&extra).unwrap();
    connection
        .execute_batch("CREATE TABLE surprise(value TEXT)")
        .unwrap();
    drop(connection);
    assert!(matches!(
        destination.restore_from(&extra),
        Err(Error::UnrecognizedDatabase)
    ));
}

#[test]
fn restore_refuses_the_active_database() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("live.sqlite3");
    let mut store = Store::open(&path).unwrap();
    let result = store.restore_from(&path);
    assert!(
        matches!(result, Err(Error::ActiveDatabase)),
        "path={:?} sqlite={:?} result={result:?}",
        path,
        store.connection.path()
    );
    let alias = directory.path().join("same-journal.sqlite3");
    std::fs::hard_link(&path, &alias).unwrap();
    assert!(matches!(
        store.restore_from(&alias),
        Err(Error::ActiveDatabase)
    ));
}

#[test]
fn failed_destination_write_preserves_live_data_and_identity() {
    let directory = tempfile::tempdir().unwrap();
    let backup = directory.path().join("source.sqlite3");
    let mut source = Store::open_in_memory().unwrap();
    note(&mut source, "restore candidate");
    source.backup_to(&backup).unwrap();
    let destination_path = directory.path().join("read-only-destination.sqlite3");
    let mut destination = Store::open(&destination_path).unwrap();
    let kept = note(&mut destination, "keep on failure");
    let identity = destination.device_id().to_owned();
    drop(destination);
    let mut destination = Store {
        connection: Connection::open_with_flags(
            &destination_path,
            OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap(),
        device_id: identity.clone(),
        migration_backup: None,
        period_note_state: Default::default(),
    };
    assert!(destination.restore_from(&backup).is_err());
    assert_eq!(destination.device_id(), identity);
    assert_eq!(
        destination
            .get_reflection(&kept.record_id)
            .unwrap()
            .unwrap()
            .heads,
        vec![kept]
    );
}

#[test]
fn restore_rejects_orphan_records_and_missing_draft_bases() {
    let directory = tempfile::tempdir().unwrap();
    let mut destination = Store::open_in_memory().unwrap();
    let source = Store::open_in_memory().unwrap();
    source
        .connection
        .execute("INSERT INTO records VALUES('orphan','reflection',1)", [])
        .unwrap();
    let orphan = directory.path().join("orphan.sqlite3");
    source.backup_to(&orphan).unwrap();
    assert!(matches!(
        destination.restore_from(&orphan),
        Err(Error::RevisionIntegrity(_))
    ));
    let mut source = Store::open_in_memory().unwrap();
    let revision = note(&mut source, "saved words");
    let draft = source.edit_reflection(&revision.record_id, 20).unwrap();
    source
        .connection
        .execute(
            "UPDATE reflection_drafts SET bases_json='[\"missing\"]' WHERE draft_id=?1",
            [&draft.id],
        )
        .unwrap();
    let missing = directory.path().join("missing.sqlite3");
    source.backup_to(&missing).unwrap();
    assert!(matches!(
        destination.restore_from(&missing),
        Err(Error::RevisionIntegrity(_))
    ));
}

#[test]
fn restore_migrates_old_snapshot_without_updating_source() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("old.sqlite3");
    let connection = Connection::open(&path).unwrap();
    connection.execute_batch("CREATE TABLE schema_migrations (version INTEGER PRIMARY KEY CHECK(version > 0), checksum TEXT NOT NULL);").unwrap();
    connection
        .execute_batch(migrations::MIGRATIONS[0].1)
        .unwrap();
    connection
        .execute(
            "INSERT INTO schema_migrations VALUES(1,?1)",
            [migrations::checksum(migrations::MIGRATIONS[0].1)],
        )
        .unwrap();
    connection.pragma_update(None, "user_version", 1).unwrap();
    drop(connection);
    let before = std::fs::read(&path).unwrap();
    let mut destination = Store::open_in_memory().unwrap();
    destination.restore_from(&path).unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), before);
    assert!(destination.load_timer().unwrap().is_some());
}
