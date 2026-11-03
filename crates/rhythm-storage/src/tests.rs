use super::*;

fn written(store: &mut Store, text: &str, time: i64) -> ReflectionRevision {
    let draft = store
        .create_draft(ReflectionInput::thought(text), time)
        .unwrap();
    store
        .finalize_draft(&draft.id, draft.version, time)
        .unwrap()
}

fn count(store: &Store, table: &str) -> i64 {
    // Test-controlled table names only; production values always use bound parameters.
    store
        .connection
        .query_row(&format!("SELECT count(*) FROM {table}"), [], |row| {
            row.get(0)
        })
        .unwrap()
}

#[test]
fn draft_and_journal_reopen_without_losing_unicode_or_whitespace() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("journal.sqlite3");
    let body = "  Dear me,\n明天慢慢来。\n明天也給自己一點空間。🌱\n";
    let (draft_id, device_id) = {
        let mut store = Store::open(&path).unwrap();
        let draft = store
            .create_draft(
                ReflectionInput {
                    kind: ReflectionKind::FutureSelf,
                    body: body.into(),
                    session_id: Some("future-session".into()),
                    reveal_at_utc_ms: Some(2_000),
                },
                100,
            )
            .unwrap();
        (draft.id, store.device_id().to_owned())
    };
    let revision = {
        let mut store = Store::open(&path).unwrap();
        assert_eq!(store.device_id(), device_id);
        let draft = store.get_draft(&draft_id).unwrap().unwrap();
        assert_eq!(draft.content.body, body);
        assert_eq!(draft.content.reveal_at_utc_ms, Some(2_000));
        assert_eq!(store.list_drafts().unwrap().len(), 1);
        store.finalize_draft(&draft.id, draft.version, 200).unwrap()
    };
    let store = Store::open(&path).unwrap();
    assert!(store.get_draft(&draft_id).unwrap().is_none());
    let reflection = store.get_reflection(&revision.record_id).unwrap().unwrap();
    assert_eq!(reflection.heads, vec![revision]);
    assert_eq!(reflection.heads[0].content.body, body);
}

#[test]
fn newer_draft_text_is_not_overwritten_by_an_older_editor() {
    let mut store = Store::open_in_memory().unwrap();
    let draft = store
        .create_draft(ReflectionInput::thought("first"), 1)
        .unwrap();
    let next = store
        .save_draft(&draft.id, draft.version, "newer", 2)
        .unwrap();
    assert!(matches!(
        store.save_draft(&draft.id, draft.version, "older", 3),
        Err(Error::DraftConflict { .. })
    ));
    assert_eq!(store.get_draft(&draft.id).unwrap(), Some(next));
    assert!(matches!(
        store.discard_draft(&draft.id, 1),
        Err(Error::DraftConflict { .. })
    ));
}

#[test]
fn failed_autosave_keeps_previous_durable_draft() {
    let mut store = Store::open_in_memory().unwrap();
    let draft = store
        .create_draft(ReflectionInput::thought("saved words"), 1)
        .unwrap();
    store.connection.execute_batch("CREATE TRIGGER reject_draft_update BEFORE UPDATE ON reflection_drafts BEGIN SELECT RAISE(ABORT,'simulated write failure'); END;").unwrap();
    assert!(matches!(
        store.save_draft(&draft.id, 1, "unsaved words", 2),
        Err(Error::Sqlite(_))
    ));
    assert_eq!(store.get_draft(&draft.id).unwrap(), Some(draft));
}

#[test]
fn finalization_rolls_back_revision_outbox_and_draft_removal_together() {
    let mut store = Store::open_in_memory().unwrap();
    store
        .register_provider_binding("drive-local-binding")
        .unwrap();
    let draft = store
        .create_draft(ReflectionInput::thought("keep me"), 1)
        .unwrap();
    store.connection.execute_batch("CREATE TRIGGER reject_draft_delete BEFORE DELETE ON reflection_drafts BEGIN SELECT RAISE(ABORT,'simulated failure at final step'); END;").unwrap();
    assert!(matches!(
        store.finalize_draft(&draft.id, 1, 2),
        Err(Error::Sqlite(_))
    ));
    assert_eq!(store.get_draft(&draft.id).unwrap(), Some(draft.clone()));
    assert_eq!(count(&store, "records"), 0);
    assert_eq!(count(&store, "record_revisions"), 0);
    assert_eq!(count(&store, "sync_outbox"), 0);
    store
        .connection
        .execute_batch("DROP TRIGGER reject_draft_delete")
        .unwrap();
    let revision = store.finalize_draft(&draft.id, 1, 3).unwrap();
    assert_eq!(revision.author_sequence, 1);
    assert_eq!(store.outbox_items("drive-local-binding").unwrap().len(), 1);
    assert!(store.get_draft(&draft.id).unwrap().is_none());
}

#[test]
fn edits_keep_history_and_a_stale_draft_requires_an_explicit_choice() {
    let mut store = Store::open_in_memory().unwrap();
    let first = written(&mut store, "first version", 10);
    let a = store.edit_reflection(&first.record_id, 20).unwrap();
    let b = store.edit_reflection(&first.record_id, 20).unwrap();
    let a = store
        .save_draft(&a.id, a.version, "device A words", 21)
        .unwrap();
    let b = store
        .save_draft(&b.id, b.version, "device B words", 21)
        .unwrap();
    let second = store.finalize_draft(&a.id, a.version, 22).unwrap();
    assert!(matches!(
        store.finalize_draft(&b.id, b.version, 23),
        Err(Error::ReflectionConflict { .. })
    ));
    assert_eq!(store.get_draft(&b.id).unwrap(), Some(b.clone()));
    assert_eq!(
        store.revision_history(&first.record_id).unwrap(),
        vec![first.clone(), second.clone()]
    );

    let third = store
        .finalize_draft_as_variant(&b.id, b.version, 24)
        .unwrap();
    let reflection = store.get_reflection(&first.record_id).unwrap().unwrap();
    assert_eq!(reflection.heads.len(), 2);
    assert_eq!(third.parents, vec![first.id.clone()]);
    assert!(matches!(
        store.edit_reflection(&first.record_id, 25),
        Err(Error::ReflectionConflict { .. })
    ));

    let reviewed: Vec<String> = reflection
        .heads
        .iter()
        .map(|head| head.id.clone())
        .collect();
    let combined = store
        .edit_reflection_heads(
            &first.record_id,
            &reviewed,
            ReflectionInput::thought("both thoughts matter"),
            26,
        )
        .unwrap();
    let merged = store
        .finalize_draft(&combined.id, combined.version, 27)
        .unwrap();
    assert_eq!(
        sorted_ids(&merged.parents),
        sorted_ids(&[second.id, third.id])
    );
    assert_eq!(
        store
            .get_reflection(&first.record_id)
            .unwrap()
            .unwrap()
            .heads,
        vec![merged]
    );
    assert_eq!(store.revision_history(&first.record_id).unwrap().len(), 4);
}

#[test]
fn explicit_head_selection_cannot_swallow_unreviewed_changes() {
    let mut store = Store::open_in_memory().unwrap();
    let first = written(&mut store, "first", 1);
    let draft = store.edit_reflection(&first.record_id, 2).unwrap();
    let second = store.finalize_draft(&draft.id, draft.version, 3).unwrap();
    assert!(matches!(
        store.edit_reflection_heads(
            &first.record_id,
            &[first.id],
            ReflectionInput::thought("stale merge"),
            4
        ),
        Err(Error::ReflectionConflict { .. })
    ));
    assert_eq!(
        store
            .get_reflection(&first.record_id)
            .unwrap()
            .unwrap()
            .heads,
        vec![second]
    );
}

#[test]
fn empty_draft_is_allowed_but_optional_writing_creates_no_empty_letter() {
    let mut store = Store::open_in_memory().unwrap();
    let draft = store
        .create_draft(ReflectionInput::thought(" \n "), 1)
        .unwrap();
    assert!(matches!(
        store.finalize_draft(&draft.id, 1, 2),
        Err(Error::EmptyReflection)
    ));
    assert!(store.get_draft(&draft.id).unwrap().is_some());
    assert!(store.list_reflections(20, 0).unwrap().is_empty());
    store.discard_draft(&draft.id, 1).unwrap();
    assert!(store.list_drafts().unwrap().is_empty());
}

#[test]
fn large_draft_is_rejected_without_touching_saved_text() {
    let mut store = Store::open_in_memory().unwrap();
    let draft = store
        .create_draft(ReflectionInput::thought("saved"), 1)
        .unwrap();
    assert!(matches!(
        store.save_draft(&draft.id, 1, &"x".repeat(MAX_REFLECTION_BYTES + 1), 2),
        Err(Error::ReflectionTooLarge)
    ));
    assert_eq!(store.get_draft(&draft.id).unwrap(), Some(draft));
}

#[test]
fn creation_order_is_stable_and_pagination_has_no_duplicate_rows() {
    let mut store = Store::open_in_memory().unwrap();
    let first = written(&mut store, "first", 1);
    let second = written(&mut store, "second", 2);
    assert_eq!(
        store.list_reflections(1, 0).unwrap()[0].id,
        second.record_id
    );
    assert_eq!(store.list_reflections(1, 1).unwrap()[0].id, first.record_id);
    assert!(store.list_reflections(0, 0).unwrap().is_empty());
}

#[test]
fn drafts_stay_local_and_connecting_backfills_all_revision_history() {
    let mut store = Store::open_in_memory().unwrap();
    let first = written(&mut store, "before connection", 1);
    assert_eq!(count(&store, "sync_outbox"), 0);
    store
        .create_draft(ReflectionInput::thought("still private draft"), 2)
        .unwrap();
    let draft = store.edit_reflection(&first.record_id, 3).unwrap();
    store.finalize_draft(&draft.id, draft.version, 4).unwrap();
    store.register_provider_binding("drive").unwrap();
    assert_eq!(store.outbox_items("drive").unwrap().len(), 2);
    store.register_provider_binding("drive").unwrap();
    assert_eq!(store.outbox_items("drive").unwrap().len(), 2);
    written(&mut store, "after connection", 5);
    assert_eq!(store.outbox_items("drive").unwrap().len(), 3);
    store.disconnect_provider().unwrap();
    written(&mut store, "after disconnect", 6);
    assert_eq!(store.outbox_items("drive").unwrap().len(), 3);
    store.register_provider_binding("drive").unwrap();
    assert_eq!(store.outbox_items("drive").unwrap().len(), 4);
    store.register_provider_binding("replacement").unwrap();
    assert_eq!(store.outbox_items("replacement").unwrap().len(), 4);
    assert_eq!(store.outbox_items("drive").unwrap().len(), 4);
}

#[test]
fn foreign_keys_and_immutable_revisions_are_enforced() {
    let mut store = Store::open_in_memory().unwrap();
    let enabled: bool = store
        .connection
        .pragma_query_value(None, "foreign_keys", |row| row.get(0))
        .unwrap();
    assert!(enabled);
    assert!(
        store
            .connection
            .execute(
                "INSERT INTO record_heads(record_id,revision_id) VALUES('missing','missing')",
                []
            )
            .is_err()
    );
    let revision = written(&mut store, "original", 1);
    assert!(
        store
            .connection
            .execute(
                "UPDATE record_revisions SET payload_json='{}' WHERE revision_id=?1",
                [&revision.id]
            )
            .is_err()
    );
    assert!(
        store
            .connection
            .execute(
                "DELETE FROM record_revisions WHERE revision_id=?1",
                [&revision.id]
            )
            .is_err()
    );
    assert_eq!(
        store.revision_history(&revision.record_id).unwrap(),
        vec![revision]
    );
}

#[test]
fn two_connections_cannot_commit_a_stale_timer_checkpoint() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("timer.sqlite3");
    let mut first = Store::open(&path).unwrap();
    let mut second = Store::open(&path).unwrap();
    assert_eq!(first.commit_checkpoint(None, "{}", None).unwrap(), 1);
    assert!(matches!(
        second.commit_checkpoint(None, "{}", None),
        Err(Error::TimerConflict)
    ));
    assert_eq!(
        second
            .commit_checkpoint(Some(1), r#"{"pending":"rest"}"#, None)
            .unwrap(),
        2
    );
    assert!(matches!(
        first.commit_checkpoint(Some(1), "{}", None),
        Err(Error::TimerConflict)
    ));
    assert_eq!(first.load_checkpoint().unwrap().unwrap().generation, 2);
}

#[test]
fn session_and_checkpoint_commit_atomically_and_duplicate_terminal_data_is_idempotent() {
    let mut store = Store::open_in_memory().unwrap();
    store.register_provider_binding("drive").unwrap();
    store.commit_checkpoint(None, "{}", None).unwrap();
    let terminal = StoredSession {
        id: "session-1".into(),
        json: r#"{"started_utc_ms":10,"detected_utc_ms":30}"#.into(),
    };
    store.connection.execute_batch("CREATE TRIGGER reject_checkpoint BEFORE UPDATE ON local_timer_checkpoint BEGIN SELECT RAISE(ABORT,'simulated checkpoint failure'); END;").unwrap();
    assert!(
        store
            .commit_checkpoint(Some(1), r#"{"pending":"rest"}"#, Some(&terminal))
            .is_err()
    );
    assert_eq!(count(&store, "terminal_sessions"), 0);
    assert_eq!(count(&store, "records"), 0);
    assert_eq!(count(&store, "sync_outbox"), 0);
    assert_eq!(store.load_checkpoint().unwrap().unwrap().generation, 1);
    store
        .connection
        .execute_batch("DROP TRIGGER reject_checkpoint")
        .unwrap();
    assert_eq!(
        store
            .commit_checkpoint(Some(1), "{}", Some(&terminal))
            .unwrap(),
        2
    );
    assert_eq!(
        store
            .commit_checkpoint(Some(2), "{}", Some(&terminal))
            .unwrap(),
        3
    );
    assert_eq!(count(&store, "terminal_sessions"), 1);
    assert_eq!(count(&store, "record_revisions"), 1);
    assert_eq!(store.outbox_items("drive").unwrap().len(), 1);
    let different = StoredSession {
        id: terminal.id.clone(),
        json: r#"{"started_utc_ms":10,"detected_utc_ms":900}"#.into(),
    };
    assert!(matches!(
        store.commit_checkpoint(Some(3), "{}", Some(&different)),
        Err(Error::SessionConflict(_))
    ));
    assert_eq!(store.load_checkpoint().unwrap().unwrap().generation, 3);
}

#[test]
fn migrations_are_repeatable_and_configure_wal_for_file_databases() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("journal.sqlite3");
    let revision = {
        let mut store = Store::open(&path).unwrap();
        written(&mut store, "remember", 1)
    };
    let store = Store::open(&path).unwrap();
    assert_eq!(count(&store, "schema_migrations"), 7);
    let mode: String = store
        .connection
        .pragma_query_value(None, "journal_mode", |row| row.get(0))
        .unwrap();
    assert_eq!(mode, "wal");
    assert_eq!(
        store.revision_history(&revision.record_id).unwrap(),
        vec![revision]
    );
}

#[test]
fn migration_checksum_drift_and_newer_databases_are_rejected() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("journal.sqlite3");
    {
        Store::open(&path).unwrap();
    }
    {
        let connection = Connection::open(&path).unwrap();
        connection
            .execute(
                "UPDATE schema_migrations SET checksum='changed' WHERE version=1",
                [],
            )
            .unwrap();
    }
    assert!(matches!(
        Store::open(&path),
        Err(Error::MigrationChecksum { version: 1 })
    ));
    {
        let connection = Connection::open(&path).unwrap();
        connection.pragma_update(None, "user_version", 99).unwrap();
    }
    assert!(matches!(
        Store::open(&path),
        Err(Error::NewerSchema { found: 99, .. })
    ));
}

#[test]
fn upgrading_v1_preserves_drafts_and_creates_consistent_backup() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("journal.sqlite3");
    let draft = Draft {
        id: "draft-v1".into(),
        record_id: None,
        base_revision_ids: vec![],
        content: ReflectionInput::thought("舊稿還在"),
        version: 1,
        created_at_utc_ms: 1,
        updated_at_utc_ms: 2,
        source_timezone: None,
        source_utc_offset_minutes: None,
    };
    {
        let connection = Connection::open(&path).unwrap();
        connection.execute_batch("CREATE TABLE schema_migrations(version INTEGER PRIMARY KEY,checksum TEXT NOT NULL)").unwrap();
        connection
            .execute_batch(migrations::MIGRATIONS[0].1)
            .unwrap();
        connection
            .execute(
                "INSERT INTO schema_migrations(version,checksum) VALUES(1,?1)",
                [migrations::checksum(migrations::MIGRATIONS[0].1)],
            )
            .unwrap();
        connection.pragma_update(None, "user_version", 1).unwrap();
        connection.execute("INSERT INTO reflection_drafts(draft_id,record_id,bases_json,payload_json,version,created_at_utc_ms,updated_at_utc_ms) VALUES(?1,NULL,'[]',?2,1,1,2)", params![draft.id, serde_json::to_string(&draft.content).unwrap()]).unwrap();
    }
    let store = Store::open(&path).unwrap();
    assert_eq!(store.get_draft("draft-v1").unwrap(), Some(draft.clone()));
    assert_eq!(count(&store, "schema_migrations"), 7);
    let backup = Connection::open(store.migration_backup().unwrap()).unwrap();
    let backup_version: i64 = backup
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .unwrap();
    assert_eq!(backup_version, 1);
    let old_body: String = backup.query_row("SELECT json_extract(payload_json,'$.body') FROM reflection_drafts WHERE draft_id='draft-v1'", [], |row| row.get(0)).unwrap();
    assert_eq!(old_body, draft.content.body);
}

#[test]
fn unrelated_sqlite_file_is_not_claimed_as_a_rhythm_database() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("unrelated.sqlite3");
    {
        let connection = Connection::open(&path).unwrap();
        connection.execute_batch("CREATE TABLE personal_data(message TEXT);INSERT INTO personal_data VALUES('keep');").unwrap();
    }
    assert!(matches!(
        Store::open(&path),
        Err(Error::UnrecognizedDatabase)
    ));
    let connection = Connection::open(&path).unwrap();
    assert_eq!(
        connection
            .query_row("SELECT message FROM personal_data", [], |row| row
                .get::<_, String>(0))
            .unwrap(),
        "keep"
    );
    let mode: String = connection
        .pragma_query_value(None, "journal_mode", |row| row.get(0))
        .unwrap();
    assert_eq!(mode, "delete");
}

#[test]
fn invalid_timer_checkpoint_is_rejected_before_a_write() {
    let mut store = Store::open_in_memory().unwrap();
    let mut checkpoint = TimerEngine::default().checkpoint();
    checkpoint.settings.focus_ms = 0;
    assert!(matches!(
        store.save_timer(None, &checkpoint, None),
        Err(Error::TimerValidation(_))
    ));
    assert!(store.load_checkpoint().unwrap().is_none());
}

#[test]
fn a_terminal_session_id_cannot_be_reused_for_a_new_period() {
    use rhythm_domain::{ClockSample, Command};
    let mut store = Store::open_in_memory().unwrap();
    let mut engine = TimerEngine::default();
    let sample = |millis| ClockSample {
        epoch: "test".into(),
        monotonic_ms: millis,
        utc_ms: millis as i64,
    };
    let first = store.save_timer(None, &engine.checkpoint(), None).unwrap();
    engine
        .apply(
            Command::StartFocus {
                session_id: "same-session".into(),
                duration_ms: Some(10),
            },
            sample(0),
        )
        .unwrap();
    let second = store
        .save_timer(Some(first), &engine.checkpoint(), None)
        .unwrap();
    let terminal = engine.apply(Command::Poll, sample(10)).unwrap().unwrap();
    let third = store
        .save_timer(Some(second), &engine.checkpoint(), Some(&terminal))
        .unwrap();
    engine.apply(Command::NewRhythm, sample(11)).unwrap();
    engine
        .apply(
            Command::StartFocus {
                session_id: "same-session".into(),
                duration_ms: Some(10),
            },
            sample(12),
        )
        .unwrap();
    assert!(matches!(
        store.save_timer(Some(third), &engine.checkpoint(), None),
        Err(Error::SessionConflict(_))
    ));
    assert_eq!(store.load_timer().unwrap().unwrap().revision, third);
    assert_eq!(store.get_session("same-session").unwrap(), Some(terminal));
}

fn completed_session(store: &mut Store) -> SessionRecord {
    use rhythm_domain::{ClockSample, Command};
    let mut engine = TimerEngine::default();
    engine
        .apply(
            Command::StartFocus {
                session_id: "completed".into(),
                duration_ms: Some(10),
            },
            ClockSample {
                epoch: "test".into(),
                monotonic_ms: 0,
                utc_ms: 100,
            },
        )
        .unwrap();
    let session = engine
        .apply(
            Command::Poll,
            ClockSample {
                epoch: "test".into(),
                monotonic_ms: 10,
                utc_ms: 110,
            },
        )
        .unwrap()
        .unwrap();
    store
        .save_timer(None, &engine.checkpoint(), Some(&session))
        .unwrap();
    session
}

#[test]
fn terminal_session_heads_cannot_be_edited_as_a_reflection() {
    let mut store = Store::open_in_memory().unwrap();
    let session = completed_session(&mut store);
    let heads = head_ids(&store.connection, &session.id).unwrap();
    assert!(matches!(
        store.edit_reflection_heads(
            &session.id,
            &heads,
            ReflectionInput::thought("wrong kind"),
            120
        ),
        Err(Error::ReflectionNotFound(_))
    ));
    assert!(store.list_drafts().unwrap().is_empty());
    assert_eq!(store.get_session(&session.id).unwrap(), Some(session));
}

#[test]
fn session_reads_detect_projection_corruption() {
    let mut store = Store::open_in_memory().unwrap();
    let session = completed_session(&mut store);
    store
        .connection
        .execute(
            "UPDATE terminal_sessions SET payload_json='{}' WHERE session_id=?1",
            [&session.id],
        )
        .unwrap();
    assert!(matches!(
        store.get_session(&session.id),
        Err(Error::SessionIntegrity(_))
    ));
    assert!(matches!(
        store.list_sessions(10, 0),
        Err(Error::SessionIntegrity(_))
    ));
}

#[test]
fn terminal_session_validation_precedes_storage() {
    use rhythm_domain::{ClockSample, Command};
    let mut store = Store::open_in_memory().unwrap();
    let mut engine = TimerEngine::default();
    engine
        .apply(
            Command::StartFocus {
                session_id: "bad-duration".into(),
                duration_ms: Some(10),
            },
            ClockSample {
                epoch: "test".into(),
                monotonic_ms: 0,
                utc_ms: 0,
            },
        )
        .unwrap();
    let mut session = engine
        .apply(
            Command::Poll,
            ClockSample {
                epoch: "test".into(),
                monotonic_ms: 10,
                utc_ms: 10,
            },
        )
        .unwrap()
        .unwrap();
    session.elapsed_ms = 11;
    assert!(matches!(
        store.save_timer(None, &engine.checkpoint(), Some(&session)),
        Err(Error::TimerValidation(_))
    ));
    assert!(store.load_timer().unwrap().is_none());
    assert_eq!(count(&store, "terminal_sessions"), 0);
}
