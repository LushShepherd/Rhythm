use super::*;
use crate::PeriodNote;

fn profile() -> DesktopProfile {
    serde_json::from_value(serde_json::json!({
        "schema":1,
        "preferences":{
            "rhythm.language":"zh-Hant","rhythm.theme":"light","rhythm.entered":"true",
            "rhythm.adaptive.v1":"{\"schema\":1,\"enabled\":true,\"context\":\"study\",\"bounds\":{\"focus\":{\"minMs\":600000,\"maxMs\":3600000},\"break\":{\"minMs\":180000,\"maxMs\":900000}},\"rhythm\":{\"study:morning\":{\"focus\":{\"samples\":1,\"adjustmentMs\":0,\"pending\":[60000]}}},\"receptivity\":{}}",
            "rhythm.reminder-delay.v1":"{\"schema\":1,\"enabled\":true,\"baseMs\":540000,\"kinds\":{\"focus\":{\"samples\":1,\"adjustmentMs\":0,\"pending\":[1]}}}"
        },
        "backgrounds":{"schema":1,
            "focus":{"choice":"custom","custom":{"name":"山 🌿.png","mime":"image/png","base64":"iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+aZ1cAAAAASUVORK5CYII="}},
            "break":{"choice":"coast","custom":{"name":"saved-for-later.png","mime":"image/png","base64":"iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+aZ1cAAAAASUVORK5CYII="}}
        }
    })).unwrap()
}

fn archived_notes() -> Vec<PeriodNote> {
    (0..137)
        .map(|index| PeriodNote {
            id: format!("old-{index}"),
            kind: if index % 2 == 0 { "focus" } else { "break" }.into(),
            body: format!("  Words {index}\n简体 繁體 🌿\r\n"),
            version: 3,
            created_at_utc_ms: index,
            updated_at_utc_ms: index + 1,
            archived_at_utc_ms: Some(index + 2),
            source_timezone: None,
            source_utc_offset_minutes: None,
        })
        .collect()
}

#[test]
fn complete_snapshot_keeps_every_note_raw_preference_and_dormant_custom_image() {
    let directory = tempfile::tempdir().unwrap();
    let source_path = directory.path().join("live.sqlite3");
    let backup_path = directory.path().join("complete.sqlite3");
    let restored_path = directory.path().join("restored.sqlite3");
    let mut source = Store::open(&source_path).unwrap();
    source
        .connection
        .pragma_update(None, "wal_autocheckpoint", 0)
        .unwrap();
    source.import_period_notes(&archived_notes()).unwrap();
    let expected_notes = source.all_period_notes().unwrap();
    assert_eq!(expected_notes.len(), 137);
    let profile = profile();
    source.backup_complete_to(&backup_path, &profile).unwrap();
    assert!(source.pending_desktop_profile().unwrap().is_none());
    assert_eq!(
        Store::inspect_backup_profile(&backup_path).unwrap(),
        Some(profile.clone())
    );
    let original_bytes = std::fs::read(&backup_path).unwrap();
    let mut restored = Store::open(&restored_path).unwrap();
    let (_, _, pending) = restored
        .restore_complete_from(
            &backup_path,
            &Some(profile.clone()),
            rhythm_domain::ClockSample {
                epoch: "new".into(),
                monotonic_ms: 0,
                utc_ms: 1000,
            },
        )
        .unwrap();
    let pending = pending.unwrap();
    assert_eq!(pending.profile, profile);
    assert_eq!(restored.all_period_notes().unwrap(), expected_notes);
    drop(restored);
    let mut restored = Store::open(&restored_path).unwrap();
    assert_eq!(
        restored.pending_desktop_profile().unwrap(),
        Some(pending.clone())
    );
    assert!(
        !restored
            .acknowledge_desktop_profile_restore("stale")
            .unwrap()
    );
    assert_eq!(
        restored.pending_desktop_profile().unwrap(),
        Some(pending.clone())
    );
    assert!(
        restored
            .acknowledge_desktop_profile_restore(&pending.token)
            .unwrap()
    );
    assert!(restored.pending_desktop_profile().unwrap().is_none());
    assert!(
        !restored
            .acknowledge_desktop_profile_restore(&pending.token)
            .unwrap()
    );
    assert_eq!(std::fs::read(&backup_path).unwrap(), original_bytes);
}

#[test]
fn corrupt_or_changed_profile_never_replaces_destination_and_cannot_overwrite_backup() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("complete.sqlite3");
    let source = Store::open_in_memory().unwrap();
    let profile = profile();
    source.backup_complete_to(&path, &profile).unwrap();
    let bytes = std::fs::read(&path).unwrap();
    assert!(source.backup_complete_to(&path, &profile).is_err());
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
    let mut destination = Store::open_in_memory().unwrap();
    let draft = destination
        .create_draft(crate::ReflectionInput::thought("Keep my unsent words"), 1)
        .unwrap();
    let mut other = profile.clone();
    other
        .preferences
        .insert("rhythm.theme".into(), "dark".into());
    let sample = || rhythm_domain::ClockSample {
        epoch: "new".into(),
        monotonic_ms: 0,
        utc_ms: 1000,
    };
    assert!(matches!(
        destination.restore_complete_from(&path, &Some(other), sample()),
        Err(Error::BackupProfileChanged)
    ));
    assert_eq!(destination.list_drafts().unwrap(), vec![draft.clone()]);
    let tampered = Connection::open(&path).unwrap();
    tampered
        .execute("UPDATE local_desktop_profile SET payload_json='{}'", [])
        .unwrap();
    drop(tampered);
    assert!(Store::inspect_backup_profile(&path).is_err());
    assert!(
        destination
            .restore_complete_from(&path, &Some(profile), sample())
            .is_err()
    );
    assert_eq!(destination.list_drafts().unwrap(), vec![draft]);
    assert!(destination.pending_desktop_profile().unwrap().is_none());
}

#[test]
fn failed_destination_commit_keeps_journal_and_has_no_partial_pending_restore() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("complete.sqlite3");
    let source = Store::open_in_memory().unwrap();
    let profile = profile();
    source.backup_complete_to(&path, &profile).unwrap();
    let destination_path = directory.path().join("read-only.sqlite3");
    let mut destination = Store::open(&destination_path).unwrap();
    let draft = destination
        .create_draft(crate::ReflectionInput::thought("unchanged"), 1)
        .unwrap();
    let identity = destination.device_id().to_owned();
    drop(destination);
    let mut destination = Store {
        connection: Connection::open_with_flags(
            &destination_path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap(),
        device_id: identity.clone(),
        migration_backup: None,
        period_note_state: Default::default(),
    };
    assert!(
        destination
            .restore_complete_from(
                &path,
                &Some(profile),
                rhythm_domain::ClockSample {
                    epoch: "new".into(),
                    monotonic_ms: 0,
                    utc_ms: 1000
                }
            )
            .is_err()
    );
    assert_eq!(destination.device_id(), identity);
    assert_eq!(destination.list_drafts().unwrap(), vec![draft]);
    assert!(destination.pending_desktop_profile().unwrap().is_none());
}

#[test]
fn old_database_snapshot_retains_no_profile_and_upgrades_without_touching_source() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("old.sqlite3");
    let source = Store::open_in_memory().unwrap();
    source.connection.execute_batch("ALTER TABLE reflection_drafts DROP COLUMN source_timezone; ALTER TABLE reflection_drafts DROP COLUMN source_utc_offset_minutes; ALTER TABLE record_revisions DROP COLUMN source_timezone; ALTER TABLE record_revisions DROP COLUMN source_utc_offset_minutes; ALTER TABLE local_period_notes DROP COLUMN source_timezone; ALTER TABLE local_period_notes DROP COLUMN source_utc_offset_minutes; DROP TABLE local_desktop_profile; DELETE FROM schema_migrations WHERE version>=6; PRAGMA user_version=5;").unwrap();
    source.backup_to(&path).unwrap();
    let bytes = std::fs::read(&path).unwrap();
    assert_eq!(Store::inspect_backup_profile(&path).unwrap(), None);
    let mut destination = Store::open_in_memory().unwrap();
    let (_, _, pending) = destination
        .restore_complete_from(
            &path,
            &None,
            rhythm_domain::ClockSample {
                epoch: "new".into(),
                monotonic_ms: 0,
                utc_ms: 1000,
            },
        )
        .unwrap();
    assert!(pending.is_none());
    assert!(destination.pending_desktop_profile().unwrap().is_none());
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
}

#[test]
fn invalid_profile_is_rejected_before_creating_a_snapshot() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("invalid.sqlite3");
    let source = Store::open_in_memory().unwrap();
    let mut invalid = profile();
    invalid
        .preferences
        .insert("unrelated.secret".into(), "excluded".into());
    assert!(source.backup_complete_to(&path, &invalid).is_err());
    assert!(!path.exists());
    for base64 in ["", "YQ=", "YR==", "YWJ=", "AA===", "AAAA "] {
        let mut invalid = profile();
        invalid.backgrounds.focus.custom.as_mut().unwrap().base64 = base64.into();
        assert!(invalid.validate().is_err(), "accepted {base64}");
    }
    let mut invalid = profile();
    invalid.schema = 2;
    assert!(invalid.validate().is_err());
}
