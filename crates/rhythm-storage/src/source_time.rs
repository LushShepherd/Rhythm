//! Preserve the local writing context alongside its authoritative UTC instant.
//! Never infer provenance for old records from the computer that imports them.
use chrono::{DateTime, Offset, TimeZone, Utc};

use crate::{Error, Result};

pub(crate) fn at_zone(utc_ms: i64, zone_name: &str) -> (Option<String>, Option<i32>) {
    let Some(utc) = DateTime::<Utc>::from_timestamp_millis(utc_ms) else {
        return (None, None);
    };
    let Ok(zone) = zone_name.parse::<chrono_tz::Tz>() else {
        return (None, None);
    };
    let seconds = zone
        .offset_from_utc_datetime(&utc.naive_utc())
        .fix()
        .local_minus_utc();
    // Historical sub-minute offsets cannot be represented faithfully by this
    // protocol. Keep them unknown rather than round into a fabricated wall time.
    if seconds % 60 != 0 {
        return (None, None);
    }
    (Some(zone_name.to_owned()), Some(seconds / 60))
}

pub(crate) fn system(utc_ms: i64) -> (Option<String>, Option<i32>) {
    iana_time_zone::get_timezone().map_or((None, None), |zone| at_zone(utc_ms, &zone))
}

pub(crate) fn valid(zone: Option<&str>, offset: Option<i32>) -> bool {
    match (zone, offset) {
        (None, None) => true,
        (Some(zone), Some(offset)) => {
            !zone.is_empty()
                && zone.len() <= 128
                && zone.parse::<chrono_tz::Tz>().is_ok()
                && zone
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b"/_+-".contains(&byte))
                && (-1439..=1439).contains(&offset)
        }
        _ => false,
    }
}

pub(crate) fn validate(zone: Option<&str>, offset: Option<i32>) -> Result<()> {
    if valid(zone, offset) {
        Ok(())
    } else {
        Err(Error::InvalidNoteArchive(
            "invalid original timezone metadata",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn instant(value: &str) -> i64 {
        DateTime::parse_from_rfc3339(value)
            .unwrap()
            .timestamp_millis()
    }

    #[test]
    fn repeated_dst_hour_keeps_two_distinct_utc_instants_and_offsets() {
        let earlier = instant("2026-11-01T08:30:00Z");
        let later = instant("2026-11-01T09:30:00Z");
        assert_eq!(at_zone(earlier, "America/Los_Angeles").1, Some(-420));
        assert_eq!(at_zone(later, "America/Los_Angeles").1, Some(-480));
        assert_eq!(earlier - 420 * 60_000, later - 480 * 60_000);
        assert!(earlier < later);
    }

    #[test]
    fn one_utc_instant_can_have_different_original_calendar_dates() {
        let utc = instant("2026-11-02T00:30:00Z");
        assert_eq!(at_zone(utc, "America/Los_Angeles").1, Some(-480));
        assert_eq!(at_zone(utc, "Asia/Shanghai").1, Some(480));
        let west = DateTime::from_timestamp_millis(utc - 480 * 60_000).unwrap();
        let east = DateTime::from_timestamp_millis(utc + 480 * 60_000).unwrap();
        assert_eq!(west.date_naive().to_string(), "2026-11-01");
        assert_eq!(east.date_naive().to_string(), "2026-11-02");
    }

    #[test]
    fn authoring_captures_source_at_creation_and_saves_keep_it() {
        let created = instant("2026-11-01T08:30:00Z");
        let saved = instant("2026-11-01T09:30:00Z");
        let expected = system(created);
        let mut store = crate::Store::open_in_memory().unwrap();
        let draft = store
            .create_draft(crate::ReflectionInput::thought("first words"), created)
            .unwrap();
        assert_eq!(
            (
                draft.source_timezone.clone(),
                draft.source_utc_offset_minutes
            ),
            expected
        );
        let updated = store
            .save_draft(&draft.id, draft.version, "later words", saved)
            .unwrap();
        assert_eq!(updated.source_timezone, draft.source_timezone);
        assert_eq!(
            updated.source_utc_offset_minutes,
            draft.source_utc_offset_minutes
        );
        let revision = store
            .finalize_draft(&updated.id, updated.version, saved)
            .unwrap();
        assert_eq!(
            (revision.source_timezone, revision.source_utc_offset_minutes),
            system(saved)
        );
        let mut timer = rhythm_domain::TimerEngine::default();
        timer
            .apply(
                rhythm_domain::Command::StartFocus {
                    session_id: "new-period".into(),
                    duration_ms: Some(10),
                },
                rhythm_domain::ClockSample {
                    epoch: "test".into(),
                    monotonic_ms: 0,
                    utc_ms: created,
                },
            )
            .unwrap();
        store.save_timer(None, &timer.checkpoint(), None).unwrap();
        let note = store.period_notes().unwrap().period_note.unwrap();
        assert_eq!(
            (note.source_timezone, note.source_utc_offset_minutes),
            expected
        );
    }

    #[test]
    fn schema_six_upgrade_leaves_legacy_provenance_unknown_and_backups_unchanged() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("schema-six.sqlite3");
        let mut source = crate::Store::open_in_memory().unwrap();
        let draft = source
            .create_draft(crate::ReflectionInput::thought("legacy letter"), 1)
            .unwrap();
        let revision = source.finalize_draft(&draft.id, draft.version, 2).unwrap();
        let draft = source
            .create_draft(crate::ReflectionInput::thought("legacy draft"), 3)
            .unwrap();
        source.connection.execute("INSERT INTO local_period_notes(period_id,kind,body,created_at_utc_ms,updated_at_utc_ms,archived_at_utc_ms) VALUES('legacy-period','focus','legacy period',1,2,3)", []).unwrap();
        source.connection.execute_batch("ALTER TABLE local_period_notes DROP COLUMN source_timezone; ALTER TABLE local_period_notes DROP COLUMN source_utc_offset_minutes; ALTER TABLE reflection_drafts DROP COLUMN source_timezone; ALTER TABLE reflection_drafts DROP COLUMN source_utc_offset_minutes; ALTER TABLE record_revisions DROP COLUMN source_timezone; ALTER TABLE record_revisions DROP COLUMN source_utc_offset_minutes; DELETE FROM schema_migrations WHERE version=7; PRAGMA user_version=6;").unwrap();
        source.backup_to(&path).unwrap();
        let original = std::fs::read(&path).unwrap();
        let mut restored = crate::Store::open_in_memory().unwrap();
        restored.restore_from(&path).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), original);
        let upgraded = crate::Store::open(&path).unwrap();
        for store in [&restored, &upgraded] {
            for note in store.all_period_notes().unwrap() {
                assert_eq!(note.source_timezone, None);
                assert_eq!(note.source_utc_offset_minutes, None);
            }
            let saved = store.get_draft(&draft.id).unwrap().unwrap();
            assert_eq!(saved.source_timezone, None);
            assert_eq!(saved.source_utc_offset_minutes, None);
            let saved = store
                .get_reflection(&revision.record_id)
                .unwrap()
                .unwrap()
                .heads
                .remove(0);
            assert_eq!(saved.source_timezone, None);
            assert_eq!(saved.source_utc_offset_minutes, None);
            assert_eq!(store.export_sync_archive().unwrap().protocol, 1);
        }
        let migration_backup =
            rusqlite::Connection::open(upgraded.migration_backup().unwrap()).unwrap();
        let version: i64 = migration_backup
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap();
        assert_eq!(version, 6);
    }
}
