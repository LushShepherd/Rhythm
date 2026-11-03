use super::*;
use crate::ReflectionRevision;
use rhythm_domain::{ClockSample, Command, TimerEngine};

fn written(store: &mut Store, text: &str) -> ReflectionRevision {
    let draft = store
        .create_draft(ReflectionInput::thought(text), 1)
        .unwrap();
    store.finalize_draft(&draft.id, draft.version, 2).unwrap()
}

fn edit(store: &mut Store, record: &str, text: &str) -> ReflectionRevision {
    let draft = store.edit_reflection(record, 3).unwrap();
    let draft = store.save_draft(&draft.id, draft.version, text, 4).unwrap();
    store.finalize_draft(&draft.id, draft.version, 5).unwrap()
}

fn archived(store: &Store, id: &str, text: &str) {
    store.connection.execute("INSERT INTO local_period_notes(period_id,kind,body,created_at_utc_ms,updated_at_utc_ms,archived_at_utc_ms) VALUES(?1,'focus',?2,1,2,3)", (id, text)).unwrap();
}

fn zoned_note(id: &str, instant: &str, zone: &str) -> SyncPeriodNote {
    let timestamp = chrono::DateTime::parse_from_rfc3339(instant)
        .unwrap()
        .timestamp_millis();
    let (source_timezone, source_utc_offset_minutes) = crate::source_time::at_zone(timestamp, zone);
    SyncPeriodNote {
        origin_id: hash(id.as_bytes()),
        kind: "focus".into(),
        body: id.into(),
        created_at_utc_ms: timestamp,
        updated_at_utc_ms: timestamp + 1,
        archived_at_utc_ms: timestamp + 2,
        source_timezone,
        source_utc_offset_minutes,
    }
}

#[test]
fn source_times_survive_sync_reexport_and_restore_with_stable_identity() {
    let first = zoned_note(
        "before DST fallback",
        "2026-11-01T08:30:00Z",
        "America/Los_Angeles",
    );
    let second = zoned_note(
        "after DST fallback",
        "2026-11-01T09:30:00Z",
        "America/Los_Angeles",
    );
    let west = zoned_note(
        "near midnight west",
        "2026-11-02T00:30:00Z",
        "America/Los_Angeles",
    );
    let east = zoned_note(
        "near midnight east",
        "2026-11-02T00:30:00Z",
        "Asia/Shanghai",
    );
    let payload =
        serde_json::to_string(&ReflectionInput::thought("letter written abroad")).unwrap();
    let revision = SyncRevision {
        revision_id: "written-abroad".into(),
        record_id: "letter-abroad".into(),
        kind: "reflection".into(),
        record_created_at_utc_ms: first.created_at_utc_ms,
        parents: vec![],
        author_device_id: "author-abroad".into(),
        author_sequence: 1,
        schema_version: 1,
        created_at_utc_ms: first.created_at_utc_ms,
        payload_sha256: hash(payload.as_bytes()),
        payload_json: payload,
        source_timezone: first.source_timezone.clone(),
        source_utc_offset_minutes: first.source_utc_offset_minutes,
    };
    let incoming = SyncArchive {
        protocol: 2,
        source_device_id: "author-abroad".into(),
        revisions: vec![revision.clone()],
        period_notes: vec![west, second.clone(), east, first.clone()],
    };
    let mut a = Store::open_in_memory().unwrap();
    assert_eq!(a.import_sync_archive(&incoming).unwrap().inserted_notes, 4);
    let objects = a.export_sync_objects().unwrap();
    assert!(objects.iter().all(|object| object.archive.protocol == 2));
    let mut b = Store::open_in_memory().unwrap();
    for object in objects.iter().rev() {
        b.import_sync_archive(&object.archive).unwrap();
    }
    assert_eq!(b.export_sync_objects().unwrap(), objects);
    let a_revision = a
        .get_reflection(&revision.record_id)
        .unwrap()
        .unwrap()
        .heads
        .remove(0);
    assert_eq!(a_revision.source_timezone, revision.source_timezone);
    assert_eq!(a_revision.source_utc_offset_minutes, Some(-420));
    let notes = b.note_history_page(None, 100, true).unwrap().entries;
    assert!(
        notes
            .windows(2)
            .all(|pair| pair[0].created_at_utc_ms >= pair[1].created_at_utc_ms)
    );
    assert_eq!(
        notes
            .iter()
            .find(|note| note.body == second.body)
            .unwrap()
            .source_utc_offset_minutes,
        Some(-480)
    );
    let ids: Vec<_> = b
        .all_period_notes()
        .unwrap()
        .into_iter()
        .map(|note| note.id)
        .collect();
    let directory = tempfile::tempdir().unwrap();
    let backup = directory.path().join("timezone.sqlite3");
    b.backup_to(&backup).unwrap();
    let mut restored = Store::open_in_memory().unwrap();
    restored.restore_from(&backup).unwrap();
    assert_eq!(restored.export_sync_objects().unwrap(), objects);
    assert_eq!(
        restored
            .all_period_notes()
            .unwrap()
            .into_iter()
            .map(|note| note.id)
            .collect::<Vec<_>>(),
        ids
    );
    assert_eq!(
        restored
            .get_reflection(&revision.record_id)
            .unwrap()
            .unwrap()
            .heads,
        vec![a_revision]
    );
    assert_eq!(
        restored
            .import_sync_archive(&incoming)
            .unwrap()
            .inserted_notes,
        0
    );
}

#[test]
fn legacy_sync_keeps_original_bytes_and_never_guesses_source_timezone() {
    let note = SyncPeriodNote {
        origin_id: hash(b"legacy"),
        kind: "break".into(),
        body: "legacy words".into(),
        created_at_utc_ms: 1,
        updated_at_utc_ms: 2,
        archived_at_utc_ms: 3,
        source_timezone: None,
        source_utc_offset_minutes: None,
    };
    let archive = SyncArchive {
        protocol: 1,
        source_device_id: "logical-record".into(),
        revisions: vec![],
        period_notes: vec![note.clone()],
    };
    let expected = format!(
        "{{\"protocol\":1,\"source_device_id\":\"logical-record\",\"revisions\":[],\"period_notes\":[{{\"origin_id\":\"{}\",\"kind\":\"break\",\"body\":\"legacy words\",\"created_at_utc_ms\":1,\"updated_at_utc_ms\":2,\"archived_at_utc_ms\":3}}]}}",
        note.origin_id
    );
    assert_eq!(archive.to_json().unwrap(), expected.as_bytes());
    let mut destination = Store::open_in_memory().unwrap();
    destination
        .import_sync_archive(&SyncArchive::from_json(expected.as_bytes()).unwrap())
        .unwrap();
    assert_eq!(
        destination.export_sync_objects().unwrap(),
        vec![SyncObject {
            id: hash(expected.as_bytes()),
            archive
        }]
    );
    let imported = destination.all_period_notes().unwrap().remove(0);
    assert_eq!(imported.source_timezone, None);
    assert_eq!(imported.source_utc_offset_minutes, None);
}

#[test]
fn unsupported_or_partial_timezone_metadata_rejects_the_whole_sync_batch() {
    let note = zoned_note("new words", "2026-11-02T00:30:00Z", "Asia/Shanghai");
    let valid = SyncArchive {
        protocol: 2,
        source_device_id: "source".into(),
        revisions: vec![],
        period_notes: vec![note],
    };
    let mut cases = Vec::new();
    let mut protocol = valid.clone();
    protocol.protocol = 1;
    cases.push(protocol);
    let mut partial = valid.clone();
    partial.period_notes[0].source_utc_offset_minutes = None;
    cases.push(partial);
    let mut unknown = valid.clone();
    unknown.period_notes[0].source_timezone = Some("Missing/Zone".into());
    cases.push(unknown);
    let mut offset = valid.clone();
    offset.period_notes[0].source_utc_offset_minutes = Some(1440);
    cases.push(offset);
    for rejected in cases {
        let mut destination = Store::open_in_memory().unwrap();
        assert!(destination.import_sync_archive(&rejected).is_err());
        assert!(destination.all_period_notes().unwrap().is_empty());
        assert!(SyncArchive::from_json(&serde_json::to_vec(&rejected).unwrap()).is_err());
    }
    // A recorded offset remains authoritative even if a later timezone database
    // would calculate a different historical offset.
    let mut historical = valid;
    historical.period_notes[0].source_utc_offset_minutes = Some(475);
    let mut destination = Store::open_in_memory().unwrap();
    destination.import_sync_archive(&historical).unwrap();
    assert_eq!(
        destination.export_sync_archive().unwrap().period_notes,
        historical.period_notes
    );
}

#[test]
fn metadata_changes_preserve_note_variants_and_reject_revision_identity_changes() {
    let mut source = Store::open_in_memory().unwrap();
    written(&mut source, "immutable reflection");
    let mut archive = source.export_sync_archive().unwrap();
    archive.protocol = 2;
    archive.revisions[0].source_timezone = Some("Asia/Shanghai".into());
    archive.revisions[0].source_utc_offset_minutes = Some(480);
    let mut destination = Store::open_in_memory().unwrap();
    destination.import_sync_archive(&archive).unwrap();
    let original = destination.export_sync_objects().unwrap();
    archive.revisions[0].source_timezone = Some("America/Los_Angeles".into());
    archive.revisions[0].source_utc_offset_minutes = Some(-480);
    archive.period_notes.push(zoned_note(
        "must roll back",
        "2026-11-02T00:30:00Z",
        "Asia/Shanghai",
    ));
    assert!(destination.import_sync_archive(&archive).is_err());
    assert_eq!(destination.export_sync_objects().unwrap(), original);

    let east = zoned_note(
        "same words and dates",
        "2026-11-02T00:30:00Z",
        "Asia/Shanghai",
    );
    let mut west = east.clone();
    west.source_timezone = Some("America/Los_Angeles".into());
    west.source_utc_offset_minutes = Some(-480);
    let mut legacy = east.clone();
    legacy.source_timezone = None;
    legacy.source_utc_offset_minutes = None;
    let variants = SyncArchive {
        protocol: 2,
        source_device_id: "source".into(),
        revisions: vec![],
        period_notes: vec![east, west, legacy],
    };
    assert_eq!(
        destination
            .import_sync_archive(&variants)
            .unwrap()
            .inserted_notes,
        3
    );
    assert_eq!(
        destination
            .import_sync_archive(&variants)
            .unwrap()
            .inserted_notes,
        0
    );
    assert_eq!(destination.all_period_notes().unwrap().len(), 3);
}

fn sample(time: u64) -> ClockSample {
    ClockSample {
        epoch: "test".into(),
        monotonic_ms: time,
        utc_ms: 100 + time as i64,
    }
}

fn completed(store: &mut Store) {
    let mut timer = TimerEngine::default();
    timer
        .apply(
            Command::StartFocus {
                session_id: "session".into(),
                duration_ms: Some(10),
            },
            sample(0),
        )
        .unwrap();
    let terminal = timer.apply(Command::Poll, sample(10)).unwrap();
    store
        .save_timer(None, &timer.checkpoint(), terminal.as_ref())
        .unwrap();
}

#[test]
fn offline_reflection_branches_converge_and_reordered_replay_is_idempotent() {
    let directory = tempfile::tempdir().unwrap();
    let mut a = Store::open(directory.path().join("a.sqlite3")).unwrap();
    let mut b = Store::open(directory.path().join("b.sqlite3")).unwrap();
    let initial = written(&mut a, "original\n原文  🌱");
    b.import_sync_archive(&a.export_sync_archive().unwrap())
        .unwrap();
    let left = edit(&mut a, &initial.record_id, "offline A");
    let right = edit(&mut b, &initial.record_id, "offline B");
    let a_archive = a.export_sync_archive().unwrap();
    let mut b_archive = b.export_sync_archive().unwrap();
    b_archive.revisions.reverse();
    assert_eq!(
        a.import_sync_archive(&b_archive)
            .unwrap()
            .conflicting_reflections,
        1
    );
    assert_eq!(
        b.import_sync_archive(&a_archive)
            .unwrap()
            .conflicting_reflections,
        1
    );
    for store in [&mut a, &mut b] {
        let reflection = store.get_reflection(&initial.record_id).unwrap().unwrap();
        let heads: HashSet<_> = reflection.heads.iter().map(|head| &head.id).collect();
        assert_eq!(heads, HashSet::from([&left.id, &right.id]));
        let report = store.import_sync_archive(&b_archive).unwrap();
        assert_eq!(report.inserted_revisions, 0);
        assert_eq!(report.inserted_notes, 0);
    }
    assert_eq!(
        a.export_sync_objects().unwrap(),
        b.export_sync_objects().unwrap()
    );
    drop(a);
    let reopened = Store::open(directory.path().join("a.sqlite3")).unwrap();
    assert_eq!(
        reopened
            .get_reflection(&initial.record_id)
            .unwrap()
            .unwrap()
            .heads
            .len(),
        2
    );
}

#[test]
fn deliberate_resolution_and_late_branch_preserve_all_versions() {
    let mut a = Store::open_in_memory().unwrap();
    let mut b = Store::open_in_memory().unwrap();
    let initial = written(&mut a, "start");
    b.import_sync_archive(&a.export_sync_archive().unwrap())
        .unwrap();
    let left = edit(&mut a, &initial.record_id, "A");
    let right = edit(&mut b, &initial.record_id, "B");
    a.import_sync_archive(&b.export_sync_archive().unwrap())
        .unwrap();
    let draft = a
        .edit_reflection_heads(
            &initial.record_id,
            &[left.id, right.id],
            ReflectionInput::thought("combined"),
            6,
        )
        .unwrap();
    a.finalize_draft(&draft.id, draft.version, 7).unwrap();
    // B wrote one more offline branch before learning about the resolution.
    let late = edit(&mut b, &initial.record_id, "late B");
    a.import_sync_archive(&b.export_sync_archive().unwrap())
        .unwrap();
    let reflection = a.get_reflection(&initial.record_id).unwrap().unwrap();
    assert_eq!(reflection.heads.len(), 2);
    assert!(reflection.heads.iter().any(|head| head.id == late.id));
    assert_eq!(a.revision_history(&initial.record_id).unwrap().len(), 5);
}

#[test]
fn period_variants_keep_origin_and_reexport_without_multiplying() {
    let mut a = Store::open_in_memory().unwrap();
    let mut b = Store::open_in_memory().unwrap();
    archived(&a, "period-A", "same words");
    archived(&a, "period-B", "same words");
    let first = a.export_sync_objects().unwrap();
    assert_eq!(
        first.len(),
        2,
        "independently authored identical notes retain identity"
    );
    for object in &first {
        b.import_sync_archive(&object.archive).unwrap();
    }
    assert_eq!(first, b.export_sync_objects().unwrap());
    a.save_period_note("period-A", 1, "late words\n简体 繁體", 4)
        .unwrap();
    for _ in 0..3 {
        a.import_sync_archive(&b.export_sync_archive().unwrap())
            .unwrap();
        b.import_sync_archive(&a.export_sync_archive().unwrap())
            .unwrap();
    }
    assert_eq!(
        a.export_sync_objects().unwrap(),
        b.export_sync_objects().unwrap()
    );
    assert_eq!(a.export_sync_archive().unwrap().period_notes.len(), 3);
    assert_eq!(a.all_period_notes().unwrap().len(), 3);
    assert_eq!(b.all_period_notes().unwrap().len(), 3);
}

#[test]
fn sync_leaves_active_notes_timer_and_drafts_untouched() {
    let mut source = Store::open_in_memory().unwrap();
    written(&mut source, "remote reflection");
    archived(&source, "remote-note", "remote words");
    completed(&mut source);
    let mut local = Store::open_in_memory().unwrap();
    let draft = local
        .create_draft(ReflectionInput::thought("private unfinished words"), 1)
        .unwrap();
    let mut timer = TimerEngine::default();
    timer
        .apply(
            Command::StartFocus {
                session_id: "active-session".into(),
                duration_ms: Some(100),
            },
            sample(0),
        )
        .unwrap();
    local.save_timer(None, &timer.checkpoint(), None).unwrap();
    let active = local.period_notes().unwrap().period_note.unwrap();
    let active = local
        .save_period_note(&active.id, active.version, "current editor", 101)
        .unwrap();
    let before = local.load_checkpoint().unwrap();
    local
        .import_sync_archive(&source.export_sync_archive().unwrap())
        .unwrap();
    assert_eq!(local.load_checkpoint().unwrap(), before);
    assert_eq!(local.period_notes().unwrap().period_note, Some(active));
    assert_eq!(local.get_draft(&draft.id).unwrap(), Some(draft));
    let outgoing = local.export_sync_archive().unwrap();
    assert!(
        outgoing
            .period_notes
            .iter()
            .all(|note| note.body != "current editor")
    );
    assert!(
        outgoing
            .revisions
            .iter()
            .all(|revision| !revision.payload_json.contains("private unfinished words"))
    );
    assert_eq!(local.list_sessions(100, 0).unwrap().len(), 1);
}

#[test]
fn missing_parent_corruption_unknown_versions_and_cycles_apply_nothing() {
    let mut source = Store::open_in_memory().unwrap();
    let initial = written(&mut source, "one");
    edit(&mut source, &initial.record_id, "two");
    let archive = source.export_sync_archive().unwrap();
    let mut invalid_archives = Vec::new();
    let mut missing = archive.clone();
    missing
        .revisions
        .retain(|revision| revision.revision_id != initial.id);
    invalid_archives.push(missing);
    let mut corrupt = archive.clone();
    corrupt.revisions[0].payload_json.push(' ');
    invalid_archives.push(corrupt);
    let mut future = archive.clone();
    future.protocol = 99;
    invalid_archives.push(future);
    let mut future = archive.clone();
    future.revisions[0].schema_version = 2;
    invalid_archives.push(future);
    let mut cycle = archive.clone();
    let later = cycle
        .revisions
        .iter()
        .find(|revision| revision.revision_id != initial.id)
        .unwrap()
        .revision_id
        .clone();
    cycle
        .revisions
        .iter_mut()
        .find(|revision| revision.revision_id == initial.id)
        .unwrap()
        .parents
        .push(later);
    invalid_archives.push(cycle);
    let mut wrong_record = archive.clone();
    wrong_record.revisions[0].record_id = "another-record".into();
    invalid_archives.push(wrong_record);
    for invalid in invalid_archives {
        let mut target = Store::open_in_memory().unwrap();
        assert!(target.import_sync_archive(&invalid).is_err());
        assert!(target.export_sync_archive().unwrap().revisions.is_empty());
    }
    let mut extra = serde_json::to_value(&archive).unwrap();
    extra["deleted_records"] = serde_json::json!(["unsupported deletion"]);
    assert!(SyncArchive::from_json(&serde_json::to_vec(&extra).unwrap()).is_err());
}

#[test]
fn identity_collisions_and_failed_note_writes_roll_back_the_entire_batch() {
    let mut source = Store::open_in_memory().unwrap();
    let first = written(&mut source, "original");
    let mut target = Store::open_in_memory().unwrap();
    target
        .import_sync_archive(&source.export_sync_archive().unwrap())
        .unwrap();
    written(&mut source, "new words");
    let mut changed = source.export_sync_archive().unwrap();
    let revision = changed
        .revisions
        .iter_mut()
        .find(|revision| revision.revision_id == first.id)
        .unwrap();
    revision.payload_json =
        serde_json::to_string(&ReflectionInput::thought("changed identity")).unwrap();
    revision.payload_sha256 = hash(revision.payload_json.as_bytes());
    assert!(target.import_sync_archive(&changed).is_err());
    assert_eq!(target.export_sync_archive().unwrap().revisions.len(), 1);
    archived(&source, "note", "archive");
    target.connection.execute_batch("CREATE TRIGGER reject_import BEFORE INSERT ON local_period_notes BEGIN SELECT RAISE(ABORT,'simulated write failure'); END;").unwrap();
    assert!(
        target
            .import_sync_archive(&source.export_sync_archive().unwrap())
            .is_err()
    );
    assert_eq!(target.export_sync_archive().unwrap().revisions.len(), 1);
}

#[test]
fn large_history_has_no_manual_exchange_thousand_note_limit() {
    let source = Store::open_in_memory().unwrap();
    for index in 0..1_105 {
        archived(&source, &format!("note-{index}"), "kept");
    }
    let archive = source.export_sync_archive().unwrap();
    assert_eq!(archive.period_notes.len(), 1_105);
    let mut target = Store::open_in_memory().unwrap();
    assert_eq!(
        target.import_sync_archive(&archive).unwrap().inserted_notes,
        1_105
    );
    assert_eq!(
        target.export_sync_objects().unwrap(),
        source.export_sync_objects().unwrap()
    );
    let mut too_large = archive;
    too_large.period_notes[0].body = "a".repeat(MAX_REFLECTION_BYTES + 1);
    assert!(target.import_sync_archive(&too_large).is_err());
}

#[test]
fn contradictory_session_history_is_preserved_excluded_and_backup_recoverable() {
    let mut source = Store::open_in_memory().unwrap();
    completed(&mut source);
    let first = source.export_sync_archive().unwrap();
    let mut second = first.clone();
    let revision = &mut second.revisions[0];
    revision.revision_id = "another-terminal-revision".into();
    revision.author_device_id = "another-device".into();
    let mut session: SessionRecord = serde_json::from_str(&revision.payload_json).unwrap();
    session.recovered = true;
    revision.payload_json = serde_json::to_string(&session).unwrap();
    revision.payload_sha256 = hash(revision.payload_json.as_bytes());
    let report = source.import_sync_archive(&second).unwrap();
    assert_eq!(report.conflicting_sessions, 1);
    assert!(source.get_session("session").unwrap().is_none());
    assert_eq!(source.export_sync_archive().unwrap().revisions.len(), 2);
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("conflicts.sqlite3");
    source.backup_to(&path).unwrap();
    Store::inspect_backup_profile(&path).unwrap();
    let mut restored = Store::open_in_memory().unwrap();
    restored.restore_from(&path).unwrap();
    assert_ne!(restored.device_id(), source.device_id());
    assert_eq!(
        restored.export_sync_objects().unwrap(),
        source.export_sync_objects().unwrap()
    );
    let mut target = Store::open_in_memory().unwrap();
    target.import_sync_archive(&second).unwrap();
    target.import_sync_archive(&first).unwrap();
    assert_eq!(
        target.export_sync_objects().unwrap(),
        source.export_sync_objects().unwrap()
    );
    assert!(target.list_sessions(100, 0).unwrap().is_empty());
}

#[test]
fn imported_history_cannot_close_an_active_session_with_a_shared_identity() {
    let mut source = Store::open_in_memory().unwrap();
    completed(&mut source);
    let mut target = Store::open_in_memory().unwrap();
    let mut timer = TimerEngine::default();
    timer
        .apply(
            Command::StartFocus {
                session_id: "session".into(),
                duration_ms: Some(100),
            },
            sample(0),
        )
        .unwrap();
    target.save_timer(None, &timer.checkpoint(), None).unwrap();
    let before = target.load_checkpoint().unwrap();
    assert!(
        target
            .import_sync_archive(&source.export_sync_archive().unwrap())
            .is_err()
    );
    assert_eq!(target.load_checkpoint().unwrap(), before);
    assert!(target.export_sync_archive().unwrap().revisions.is_empty());
}

#[test]
fn restored_older_profile_rejoins_without_losing_newer_remote_words() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("earlier.sqlite3");
    let mut a = Store::open_in_memory().unwrap();
    let mut b = Store::open_in_memory().unwrap();
    let initial = written(&mut a, "initial");
    archived(&a, "period-one", "original period");
    a.backup_to(&path).unwrap();
    b.import_sync_archive(&a.export_sync_archive().unwrap())
        .unwrap();
    edit(&mut b, &initial.record_id, "later remote words");
    archived(&b, "period-two", "new remote period");
    a.restore_from(&path).unwrap();
    let offline = edit(&mut a, &initial.record_id, "restored device offline words");
    a.import_sync_archive(&b.export_sync_archive().unwrap())
        .unwrap();
    b.import_sync_archive(&a.export_sync_archive().unwrap())
        .unwrap();
    assert_eq!(
        a.export_sync_objects().unwrap(),
        b.export_sync_objects().unwrap()
    );
    let reflection = b.get_reflection(&initial.record_id).unwrap().unwrap();
    assert_eq!(reflection.heads.len(), 2);
    assert!(reflection.heads.iter().any(|head| head.id == offline.id));
    assert_eq!(a.export_sync_archive().unwrap().period_notes.len(), 2);
}

#[test]
fn serialized_size_limit_counts_json_escaping_before_allocating_a_larger_archive() {
    let archive = SyncArchive {
        protocol: 1,
        source_device_id: "test".into(),
        revisions: Vec::new(),
        period_notes: (0..11)
            .map(|index| SyncPeriodNote {
                origin_id: hash(format!("note-{index}").as_bytes()),
                kind: "focus".into(),
                body: "\0".repeat(MAX_REFLECTION_BYTES),
                created_at_utc_ms: 1,
                updated_at_utc_ms: 2,
                archived_at_utc_ms: 3,
                source_timezone: None,
                source_utc_offset_minutes: None,
            })
            .collect(),
    };
    assert!(matches!(
        archive.to_json(),
        Err(Error::InvalidSyncArchive(_))
    ));
}
