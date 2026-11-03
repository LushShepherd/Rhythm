use rhythm_domain::{Checkpoint, PeriodKind, SessionRecord, TimerState};
use rusqlite::{Connection, OptionalExtension, Row, TransactionBehavior, params};
use sha2::{Digest, Sha256};
use std::collections::HashSet;

use crate::{
    Error, HistoryNote, NoteHistoryCursor, NoteHistoryPage, PeriodNote, PeriodNoteState,
    ReflectionKind, Result, Store, as_i64, as_u64, next_version,
};

const COLUMNS: &str = "period_id,kind,body,version,created_at_utc_ms,updated_at_utc_ms,archived_at_utc_ms,source_timezone,source_utc_offset_minutes";

// Page over metadata first. Large note bodies are read only for this page, and
// the ordinary timer snapshot continues to carry at most 100 archived notes.
pub(crate) const HISTORY_METADATA: &str = "
    SELECT 'period:' || period_id AS history_key, period_id AS id,
        'period' AS source, archived_at_utc_ms AS at_utc_ms, version,
        source_timezone, source_utc_offset_minutes
    FROM local_period_notes WHERE archived_at_utc_ms IS NOT NULL
    UNION ALL
    SELECT 'reflection:' || r.revision_id, r.revision_id,
        CASE WHEN EXISTS(SELECT 1 FROM record_heads h WHERE h.revision_id=r.revision_id)
            THEN 'reflection' ELSE 'reflection_version' END,
        r.created_at_utc_ms, r.author_sequence, r.source_timezone, r.source_utc_offset_minutes
    FROM record_revisions r JOIN records rec ON rec.record_id=r.record_id
    WHERE ?1=0 AND rec.kind='reflection'
    UNION ALL
    SELECT 'draft:' || d.draft_id, d.draft_id, 'draft', d.updated_at_utc_ms, d.version,
        d.source_timezone, d.source_utc_offset_minutes
    FROM reflection_drafts d
    WHERE ?1=0 AND NOT EXISTS(
        SELECT 1 FROM local_period_notes p WHERE p.origin_draft_id=d.draft_id
        AND p.archived_at_utc_ms IS NOT NULL
        AND p.body=json_extract(d.payload_json,'$.body'))";

fn reflection_kind(kind: &ReflectionKind) -> &'static str {
    match kind {
        ReflectionKind::Thought => "thought",
        ReflectionKind::Intention => "intention",
        ReflectionKind::FutureSelf => "future_self",
    }
}

pub(crate) fn history_entry(
    connection: &Connection,
    id: &str,
    source: &str,
) -> Result<HistoryNote> {
    if source == "period" {
        let value = note(connection, id)?.ok_or(Error::PeriodNoteNotFound)?;
        return Ok(HistoryNote {
            id: value.id,
            source: source.into(),
            kind: value.kind,
            body: value.body,
            version: value.version,
            created_at_utc_ms: value.created_at_utc_ms,
            updated_at_utc_ms: value.updated_at_utc_ms,
            archived_at_utc_ms: value.archived_at_utc_ms,
            record_id: None,
            source_timezone: value.source_timezone,
            source_utc_offset_minutes: value.source_utc_offset_minutes,
        });
    }
    if source == "draft" {
        let value =
            crate::read_draft(connection, id)?.ok_or_else(|| Error::DraftNotFound(id.into()))?;
        return Ok(HistoryNote {
            id: value.id,
            source: source.into(),
            kind: reflection_kind(&value.content.kind).into(),
            body: value.content.body,
            version: value.version,
            created_at_utc_ms: value.created_at_utc_ms,
            updated_at_utc_ms: value.updated_at_utc_ms,
            archived_at_utc_ms: None,
            record_id: value.record_id,
            source_timezone: value.source_timezone,
            source_utc_offset_minutes: value.source_utc_offset_minutes,
        });
    }
    // Use the integrity-checked revision reader, including for older versions.
    let value = crate::read_revision(connection, id)?;
    Ok(HistoryNote {
        id: value.id,
        source: source.into(),
        kind: reflection_kind(&value.content.kind).into(),
        body: value.content.body,
        version: value.author_sequence,
        created_at_utc_ms: value.created_at_utc_ms,
        updated_at_utc_ms: value.created_at_utc_ms,
        archived_at_utc_ms: Some(value.created_at_utc_ms),
        record_id: Some(value.record_id),
        source_timezone: value.source_timezone,
        source_utc_offset_minutes: value.source_utc_offset_minutes,
    })
}

pub(crate) fn history_revision(connection: &Connection, periods_only: bool) -> Result<String> {
    let mut digest = Sha256::new();
    let mut query = connection.prepare(&format!(
        "SELECT history_key,source,at_utc_ms,version,source_timezone,source_utc_offset_minutes FROM ({HISTORY_METADATA}) ORDER BY history_key"
    ))?;
    let mut rows = query.query([periods_only])?;
    while let Some(row) = rows.next()? {
        for index in [0, 1] {
            let value: String = row.get(index)?;
            digest.update((value.len() as u64).to_le_bytes());
            digest.update(value.as_bytes());
        }
        digest.update(row.get::<_, i64>(2)?.to_le_bytes());
        digest.update(row.get::<_, i64>(3)?.to_le_bytes());
        let source_timezone: Option<String> = row.get(4)?;
        let source_offset: Option<i32> = row.get(5)?;
        digest.update(serde_json::to_vec(&(source_timezone, source_offset))?);
    }
    Ok(format!("{:x}", digest.finalize()))
}

fn row_note(row: &Row<'_>) -> rusqlite::Result<PeriodNote> {
    let version: i64 = row.get(3)?;
    let version =
        u64::try_from(version).map_err(|_| rusqlite::Error::IntegralValueOutOfRange(3, version))?;
    let source_timezone: Option<String> = row.get(7)?;
    let source_utc_offset_minutes: Option<i32> = row.get(8)?;
    if !crate::source_time::valid(source_timezone.as_deref(), source_utc_offset_minutes) {
        return Err(rusqlite::Error::FromSqlConversionFailure(
            7,
            rusqlite::types::Type::Text,
            Box::new(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "invalid original timezone metadata",
            )),
        ));
    }
    Ok(PeriodNote {
        id: row.get(0)?,
        kind: row.get(1)?,
        body: row.get(2)?,
        version,
        created_at_utc_ms: row.get(4)?,
        updated_at_utc_ms: row.get(5)?,
        archived_at_utc_ms: row.get(6)?,
        source_timezone,
        source_utc_offset_minutes,
    })
}

fn note(connection: &Connection, id: &str) -> Result<Option<PeriodNote>> {
    Ok(connection
        .query_row(
            &format!("SELECT {COLUMNS} FROM local_period_notes WHERE period_id=?1"),
            [id],
            row_note,
        )
        .optional()?)
}

fn generation(connection: &Connection) -> Result<u64> {
    as_u64(connection.query_row(
        "SELECT generation FROM local_period_note_state WHERE singleton=1",
        [],
        |row| row.get(0),
    )?)
}

pub(crate) fn bump(connection: &Connection) -> Result<()> {
    let next = as_i64(next_version(generation(connection)?)?)?;
    connection.execute(
        "UPDATE local_period_note_state SET generation=?1 WHERE singleton=1",
        [next],
    )?;
    Ok(())
}

pub(crate) fn read_state(connection: &Connection) -> Result<PeriodNoteState> {
    let id: Option<String> = connection.query_row(
        "SELECT current_period_id FROM local_period_note_state WHERE singleton=1",
        [],
        |row| row.get(0),
    )?;
    let current = id
        .map(|id| note(connection, &id)?.ok_or(Error::PeriodNoteNotFound))
        .transpose()?;
    let mut query = connection.prepare(&format!("SELECT {COLUMNS} FROM local_period_notes WHERE archived_at_utc_ms IS NOT NULL ORDER BY archived_at_utc_ms DESC,period_id"))?;
    // Rust's Unicode whitespace rules match the editor. Empty records stay stored
    // so a pending save made at the boundary can populate the correct archive.
    let archived = query
        .query_map([], row_note)?
        .filter_map(|row| match row {
            Ok(note) if note.body.trim().is_empty() => None,
            other => Some(other),
        })
        .take(100)
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(PeriodNoteState {
        notes_revision: generation(connection)?,
        period_note: current,
        archived_notes: archived,
    })
}

fn period_id(prefix: &str, session: &str) -> String {
    format!("{prefix}:{:x}", Sha256::digest(session.as_bytes()))
}

/// Runs inside the exact transaction that stores the timer and terminal record.
pub(crate) fn reconcile(
    connection: &Connection,
    checkpoint: &Checkpoint,
    terminal: Option<&SessionRecord>,
) -> Result<()> {
    reconcile_with_source(connection, checkpoint, terminal, true)
}

fn reconcile_with_source(
    connection: &Connection,
    checkpoint: &Checkpoint,
    terminal: Option<&SessionRecord>,
    capture_source: bool,
) -> Result<()> {
    let desired = match &checkpoint.state {
        TimerState::Running(active)
        | TimerState::Paused(active)
        | TimerState::RecoveryRequired(active) => Some((
            period_id("period", &active.id),
            if active.kind == PeriodKind::Focus {
                "focus"
            } else {
                "break"
            },
            active.started_utc_ms,
            active.last_sample.utc_ms,
        )),
        TimerState::Ready { .. } => terminal.map(|session| {
            (
                period_id("break", &session.id),
                "break",
                session.detected_utc_ms,
                session.detected_utc_ms,
            )
        }),
    };
    let Some((id, kind, created, now)) = desired else {
        return Ok(());
    };
    let current: Option<String> = connection.query_row(
        "SELECT current_period_id FROM local_period_note_state WHERE singleton=1",
        [],
        |row| row.get(0),
    )?;
    if current.as_deref() == Some(&id) {
        return Ok(());
    }
    if let Some(current) = current {
        connection.execute("UPDATE local_period_notes SET archived_at_utc_ms=COALESCE(archived_at_utc_ms,?1) WHERE period_id=?2", (now, current))?;
    }
    let (source_timezone, source_utc_offset_minutes) = if capture_source {
        crate::source_time::system(created)
    } else {
        (None, None)
    };
    connection.execute("INSERT INTO local_period_notes(period_id,kind,created_at_utc_ms,updated_at_utc_ms,source_timezone,source_utc_offset_minutes) VALUES(?1,?2,?3,?3,?4,?5)", (&id, kind, created, source_timezone, source_utc_offset_minutes))?;
    connection.execute(
        "UPDATE local_period_note_state SET current_period_id=?1 WHERE singleton=1",
        [&id],
    )?;
    bump(connection)
}

impl Store {
    /// All older notes are available through bounded cursor pages. The cursor's
    /// metadata revision catches imports, late archived-note saves, legacy draft
    /// changes, and restored journals. A changed history restarts at its first
    /// page instead of silently skipping or mixing entries from two snapshots.
    pub fn note_history_page(
        &self,
        cursor: Option<&NoteHistoryCursor>,
        limit: u32,
        periods_only: bool,
    ) -> Result<NoteHistoryPage> {
        if !(1..=100).contains(&limit) {
            return Err(Error::InvalidNoteArchive(
                "history pages contain 1 to 100 notes",
            ));
        }
        if let Some(cursor) = cursor
            && (cursor.key.is_empty()
                || cursor.key.len() > 512
                || cursor.revision.len() != 64
                || !cursor.revision.bytes().all(|byte| byte.is_ascii_hexdigit())
                || cursor.periods_only != periods_only)
        {
            return Err(Error::InvalidNoteArchive(
                "the note history cursor is invalid",
            ));
        }
        let transaction = self.connection.unchecked_transaction()?;
        let revision = history_revision(&transaction, periods_only)?;
        let restarted = cursor.is_some_and(|cursor| cursor.revision != revision);
        let cursor = cursor.filter(|_| !restarted);
        let mut query = transaction.prepare(&format!(
            "SELECT history_key,id,source,at_utc_ms FROM ({HISTORY_METADATA})
            WHERE (?2 IS NULL OR at_utc_ms < ?2 OR (at_utc_ms = ?2 AND history_key > ?3))
            ORDER BY at_utc_ms DESC,history_key"
        ))?;
        let mut rows = query.query(params![
            periods_only,
            cursor.map(|value| value.at_utc_ms),
            cursor.map(|value| value.key.as_str())
        ])?;
        let mut entries = Vec::new();
        let mut last = None;
        let mut more = false;
        while let Some(row) = rows.next()? {
            let key: String = row.get(0)?;
            let id: String = row.get(1)?;
            let source: String = row.get(2)?;
            let at_utc_ms: i64 = row.get(3)?;
            let entry = history_entry(&transaction, &id, &source)?;
            // Keep the same Unicode whitespace policy as the ordinary note view.
            // Empty rows remain in storage for late saves and complete backups.
            if entry.body.trim().is_empty() {
                continue;
            }
            if entries.len() == limit as usize {
                more = true;
                break;
            }
            entries.push(entry);
            last = Some(NoteHistoryCursor {
                revision: revision.clone(),
                at_utc_ms,
                key,
                periods_only,
            });
        }
        drop(rows);
        drop(query);
        transaction.commit()?;
        Ok(NoteHistoryPage {
            revision,
            entries,
            next_cursor: if more { last } else { None },
            restarted,
        })
    }

    /// Merge portable archived notes without overwriting a local note or current
    /// editor. A content fingerprint keeps repeated imports idempotent across
    /// devices while retaining divergent revisions as separate archive entries.
    pub fn import_period_notes(&mut self, notes: &[PeriodNote]) -> Result<usize> {
        if notes.len() > 1_000 {
            return Err(Error::InvalidNoteArchive(
                "at most 1000 notes can be imported at once",
            ));
        }
        let mut total_bytes = 0_usize;
        for imported in notes {
            crate::validate_id(&imported.id)?;
            crate::source_time::validate(
                imported.source_timezone.as_deref(),
                imported.source_utc_offset_minutes,
            )?;
            if !matches!(imported.kind.as_str(), "focus" | "break")
                || imported.archived_at_utc_ms.is_none()
                || imported.version == 0
                || imported.version > 9_007_199_254_740_991
            {
                return Err(Error::InvalidNoteArchive(
                    "only valid archived focus or break notes are accepted",
                ));
            }
            for timestamp in [
                Some(imported.created_at_utc_ms),
                Some(imported.updated_at_utc_ms),
                imported.archived_at_utc_ms,
            ]
            .into_iter()
            .flatten()
            {
                if !(-8_640_000_000_000_000..=8_640_000_000_000_000).contains(&timestamp) {
                    return Err(Error::InvalidNoteArchive(
                        "a note date is outside the supported range",
                    ));
                }
            }
            total_bytes = total_bytes.saturating_add(imported.body.len());
            if imported.body.len() > 100_000 || total_bytes > 8 * 1024 * 1024 {
                return Err(Error::InvalidNoteArchive(
                    "note text exceeds the import size limit",
                ));
            }
        }
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let existing = {
            let mut query =
                transaction.prepare(&format!("SELECT {COLUMNS} FROM local_period_notes"))?;
            query
                .query_map([], row_note)?
                .collect::<rusqlite::Result<Vec<_>>>()?
        };
        let mut fingerprints = existing
            .iter()
            .map(import_fingerprint)
            .collect::<Result<HashSet<_>>>()?;
        let mut inserted = 0;
        for imported in notes {
            let fingerprint = import_fingerprint(imported)?;
            if !fingerprints.insert(fingerprint.clone()) {
                continue;
            }
            let id = format!("import:{fingerprint}");
            // A deliberately conflicting identifier is an error, never permission
            // to replace words. The entire batch is rolled back on any failure.
            if note(&transaction, &id)?.is_some() {
                return Err(Error::InvalidNoteArchive(
                    "an imported identifier conflicts with local content",
                ));
            }
            transaction.execute(
                "INSERT INTO local_period_notes(period_id,kind,body,version,created_at_utc_ms,updated_at_utc_ms,archived_at_utc_ms,source_timezone,source_utc_offset_minutes) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)",
                params![id, imported.kind, imported.body, as_i64(imported.version)?, imported.created_at_utc_ms, imported.updated_at_utc_ms, imported.archived_at_utc_ms, imported.source_timezone, imported.source_utc_offset_minutes],
            )?;
            inserted += 1;
        }
        if inserted > 0 {
            bump(&transaction)?;
        }
        let state = read_state(&transaction)?;
        transaction.commit()?;
        self.period_note_state = state;
        Ok(inserted)
    }

    pub(crate) fn initialize_period_notes(&mut self) -> Result<()> {
        // Migration must not make timer recovery depend on unrelated legacy
        // journal rows. The ordinary journal read still reports malformed data.
        let drafts = {
            let mut query = self.connection.prepare("SELECT draft_id,json_extract(payload_json,'$.body'),created_at_utc_ms,updated_at_utc_ms,source_timezone,source_utc_offset_minutes FROM reflection_drafts WHERE record_id IS NULL AND json_extract(payload_json,'$.kind')='thought' AND json_extract(payload_json,'$.session_id') IS NULL AND json_type(payload_json,'$.body')='text'")?;
            query
                .query_map([], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, i64>(3)?,
                        row.get::<_, Option<String>>(4)?,
                        row.get::<_, Option<i32>>(5)?,
                    ))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?
        };
        let timer = self.load_timer()?;
        let last = if timer
            .as_ref()
            .is_some_and(|timer| matches!(timer.checkpoint.state, TimerState::Ready { .. }))
        {
            self.list_sessions(1, 0)?.into_iter().next()
        } else {
            None
        };
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        for (draft_id, body, created, updated, source_timezone, source_offset) in drafts
            .iter()
            .filter(|(_, body, _, _, _, _)| !body.trim().is_empty())
        {
            let id = period_id("legacy", draft_id);
            let inserted = transaction.execute("INSERT OR IGNORE INTO local_period_notes(period_id,kind,body,created_at_utc_ms,updated_at_utc_ms,archived_at_utc_ms,origin_draft_id,source_timezone,source_utc_offset_minutes) VALUES(?1,'break',?2,?3,?4,?4,?5,?6,?7)", params![id, body, created, updated, draft_id, source_timezone, source_offset])?;
            if inserted != 0 {
                bump(&transaction)?;
            }
        }
        if let Some(timer) = timer {
            // A note reconstructed from an older timer has no reliable writing
            // zone; opening the database on this device cannot establish it.
            reconcile_with_source(&transaction, &timer.checkpoint, last.as_ref(), false)?;
        }
        let state = read_state(&transaction)?;
        transaction.commit()?;
        self.period_note_state = state;
        Ok(())
    }

    pub fn period_notes(&self) -> Result<PeriodNoteState> {
        read_state(&self.connection)
    }

    /// The state read within the last successful timer/note transaction. Returning
    /// it cannot turn a committed write into a misleading post-commit read error.
    pub fn committed_period_notes(&self) -> &PeriodNoteState {
        &self.period_note_state
    }

    pub fn all_period_notes(&self) -> Result<Vec<PeriodNote>> {
        let mut query = self.connection.prepare(&format!(
            "SELECT {COLUMNS} FROM local_period_notes ORDER BY created_at_utc_ms,period_id"
        ))?;
        Ok(query
            .query_map([], row_note)?
            .collect::<rusqlite::Result<_>>()?)
    }

    pub fn save_period_note(
        &mut self,
        id: &str,
        version: u64,
        body: &str,
        now: i64,
    ) -> Result<PeriodNote> {
        crate::validate_id(id)?;
        if body.len() > crate::MAX_REFLECTION_BYTES {
            return Err(Error::ReflectionTooLarge);
        }
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut saved = note(&transaction, id)?.ok_or(Error::PeriodNoteNotFound)?;
        if saved.version != version {
            return Err(Error::PeriodNoteConflict);
        }
        saved.version = next_version(saved.version)?;
        saved.body = body.into();
        saved.updated_at_utc_ms = now;
        transaction.execute("UPDATE local_period_notes SET body=?1,version=?2,updated_at_utc_ms=?3 WHERE period_id=?4", params![body, as_i64(saved.version)?, now, id])?;
        bump(&transaction)?;
        let state = read_state(&transaction)?;
        transaction.commit()?;
        self.period_note_state = state;
        Ok(saved)
    }
}

fn import_fingerprint(note: &PeriodNote) -> Result<String> {
    // Keep legacy five-field IDs byte-identical. Known provenance extends the
    // identity so differing original writing contexts remain recoverable variants.
    let mut fields = vec![
        serde_json::json!(note.kind),
        serde_json::json!(note.body),
        serde_json::json!(note.created_at_utc_ms),
        serde_json::json!(note.updated_at_utc_ms),
        serde_json::json!(note.archived_at_utc_ms),
    ];
    if note.source_timezone.is_some() {
        fields.push(serde_json::json!(note.source_timezone));
        fields.push(serde_json::json!(note.source_utc_offset_minutes));
    }
    let bytes = serde_json::to_vec(&fields)?;
    Ok(format!("{:x}", Sha256::digest(bytes)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seed_archive(store: &Store, count: usize) {
        for index in 0..count {
            store.connection.execute(
                "INSERT INTO local_period_notes(period_id,kind,body,created_at_utc_ms,updated_at_utc_ms,archived_at_utc_ms) VALUES(?1,'focus',?2,1,2,?3)",
                params![format!("note-{index:04}"), format!("Note {index}\n简体 · 繁體  "), 1_000 - (index / 3) as i64],
            ).unwrap();
        }
    }

    #[test]
    fn history_cursor_reaches_every_note_beyond_one_hundred_with_timestamp_ties() {
        let store = Store::open_in_memory().unwrap();
        seed_archive(&store, 235);
        for (id, body, archived) in [
            ("blank", "\u{2003}\n\t", Some(2_000)),
            ("active", "Current writing stays in its editor", None),
        ] {
            store.connection.execute(
                "INSERT INTO local_period_notes(period_id,kind,body,created_at_utc_ms,updated_at_utc_ms,archived_at_utc_ms) VALUES(?1,'break',?2,1,2,?3)",
                params![id, body, archived],
            ).unwrap();
        }
        assert_eq!(store.period_notes().unwrap().archived_notes.len(), 100);
        let mut cursor = None;
        let mut entries = Vec::new();
        loop {
            let page = store.note_history_page(cursor.as_ref(), 17, false).unwrap();
            assert!(!page.restarted);
            assert!(page.entries.len() <= 17);
            entries.extend(page.entries);
            cursor = page.next_cursor;
            if cursor.is_none() {
                break;
            }
        }
        assert_eq!(entries.len(), 235);
        for (index, entry) in entries.iter().enumerate() {
            assert_eq!(entry.id, format!("note-{index:04}"));
            assert_eq!(entry.body, format!("Note {index}\n简体 · 繁體  "));
        }
    }

    #[test]
    fn history_change_restarts_a_page_and_invalid_scopes_are_rejected() {
        let mut store = Store::open_in_memory().unwrap();
        seed_archive(&store, 105);
        let first = store.note_history_page(None, 20, false).unwrap();
        let cursor = first.next_cursor.unwrap();
        store
            .save_period_note("note-0000", 1, "Saved after archiving", 3_000)
            .unwrap();
        let restarted = store.note_history_page(Some(&cursor), 20, false).unwrap();
        assert!(restarted.restarted);
        assert_ne!(restarted.revision, first.revision);
        assert_eq!(restarted.entries[0].body, "Saved after archiving");
        assert!(store.note_history_page(Some(&cursor), 20, true).is_err());
        assert!(store.note_history_page(None, 0, false).is_err());
        assert!(store.note_history_page(None, 101, false).is_err());
    }

    #[test]
    fn history_preserves_legacy_variants_prior_versions_and_unfinished_words() {
        let mut store = Store::open_in_memory().unwrap();
        let initial = store
            .create_draft(crate::ReflectionInput::thought("First saved words"), 10)
            .unwrap();
        let original = store
            .finalize_draft(&initial.id, initial.version, 11)
            .unwrap();
        let left = store.edit_reflection(&original.record_id, 12).unwrap();
        let right = store.edit_reflection(&original.record_id, 13).unwrap();
        let left = store
            .save_draft(&left.id, left.version, "Left preserved version", 14)
            .unwrap();
        let right = store
            .save_draft(&right.id, right.version, "Right preserved version", 15)
            .unwrap();
        store.finalize_draft(&left.id, left.version, 16).unwrap();
        store
            .finalize_draft_as_variant(&right.id, right.version, 17)
            .unwrap();
        let draft = store
            .create_draft(crate::ReflectionInput::thought("Unfinished thought"), 18)
            .unwrap();
        // An earlier three-state migration copied this exact legacy draft.
        store.connection.execute(
            "INSERT INTO local_period_notes(period_id,kind,body,created_at_utc_ms,updated_at_utc_ms,archived_at_utc_ms,origin_draft_id) VALUES('legacy-copy','break',?1,18,18,18,?2)",
            params![draft.content.body, draft.id],
        ).unwrap();
        let page = store.note_history_page(None, 100, false).unwrap();
        assert_eq!(page.entries.len(), 4);
        assert_eq!(
            page.entries
                .iter()
                .filter(|item| item.source == "reflection")
                .count(),
            2
        );
        assert_eq!(
            page.entries
                .iter()
                .filter(|item| item.source == "reflection_version")
                .count(),
            1
        );
        assert_eq!(
            page.entries
                .iter()
                .filter(|item| item.body == "Unfinished thought")
                .count(),
            1
        );
        store
            .save_draft(&draft.id, draft.version, "Newer unfinished thought", 20)
            .unwrap();
        let changed = store.note_history_page(None, 100, false).unwrap();
        assert_eq!(changed.entries.len(), 5);
        assert!(
            changed
                .entries
                .iter()
                .any(|item| item.source == "draft" && item.body == "Newer unfinished thought")
        );
        assert_ne!(page.revision, changed.revision);
        let portable = store.note_history_page(None, 100, true).unwrap();
        assert_eq!(portable.entries.len(), 1);
        assert_eq!(portable.entries[0].source, "period");
    }

    #[test]
    fn portable_fingerprint_matches_browser_unicode_fixture() {
        let mut note = PeriodNote {
            id: "any-source-id".into(),
            kind: "focus".into(),
            body: "A little space.\n简体 · 繁體  ".into(),
            version: 1,
            created_at_utc_ms: 10,
            updated_at_utc_ms: 20,
            archived_at_utc_ms: Some(30),
            source_timezone: None,
            source_utc_offset_minutes: None,
        };
        assert_eq!(
            import_fingerprint(&note).unwrap(),
            "85ccf4b931ab6a24f443c765efc34421d87a40291782cff3db6cc0b389ec740e"
        );
        note.source_timezone = Some("Asia/Shanghai".into());
        note.source_utc_offset_minutes = Some(480);
        assert_eq!(
            import_fingerprint(&note).unwrap(),
            "1e3a2483fa5dc8d13f793264ada3c31920ed0e10dc3dc2cece6c84a648b625ca"
        );
    }

    #[test]
    fn portable_import_preserves_known_and_unknown_timezone_variants() {
        let legacy = PeriodNote {
            id: "portable-source".into(),
            kind: "focus".into(),
            body: "same body".into(),
            version: 1,
            created_at_utc_ms: 10,
            updated_at_utc_ms: 20,
            archived_at_utc_ms: Some(30),
            source_timezone: None,
            source_utc_offset_minutes: None,
        };
        let mut east = legacy.clone();
        east.source_timezone = Some("Asia/Shanghai".into());
        east.source_utc_offset_minutes = Some(480);
        let mut west = legacy.clone();
        west.source_timezone = Some("America/Los_Angeles".into());
        west.source_utc_offset_minutes = Some(-480);
        let variants = vec![legacy, east, west];
        let mut store = Store::open_in_memory().unwrap();
        assert_eq!(store.import_period_notes(&variants).unwrap(), 3);
        assert_eq!(store.import_period_notes(&variants).unwrap(), 0);
        let saved = store.all_period_notes().unwrap();
        for input in &variants {
            assert!(
                saved
                    .iter()
                    .any(|output| output.source_timezone == input.source_timezone
                        && output.source_utc_offset_minutes == input.source_utc_offset_minutes
                        && output.created_at_utc_ms == input.created_at_utc_ms)
            );
        }
        let before = history_revision(&store.connection, false).unwrap();
        store.connection.execute("UPDATE local_period_notes SET source_timezone='UTC',source_utc_offset_minutes=0 WHERE source_timezone IS NULL", []).unwrap();
        assert_ne!(history_revision(&store.connection, false).unwrap(), before);
        let mut invalid = variants[0].clone();
        invalid.source_timezone = Some("Asia/Shanghai".into());
        assert!(store.import_period_notes(&[invalid]).is_err());
        assert_eq!(store.all_period_notes().unwrap().len(), 3);
    }
}
