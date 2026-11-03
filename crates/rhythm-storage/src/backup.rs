//! Explicit, consistent snapshots. Restore never edits its source and replaces the
//! destination in SQLite's own atomic backup transaction, including committed WAL data.
use std::{
    fs::OpenOptions,
    path::Path,
    time::{Duration, Instant},
};

use rhythm_domain::{ClockSample, Command, TimerEngine, TimerState};
use rusqlite::{
    Connection, OpenFlags,
    backup::{Backup, StepResult},
};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::{
    DesktopProfile, Error, PendingDesktopProfile, Result, Store, desktop_profile, migrations,
    validate_content,
};

impl Store {
    /// Write a standalone SQLite snapshot to a NEW file. Existing files are never
    /// overwritten. The caller must flush editor drafts before requesting a snapshot.
    pub fn backup_to(&self, path: impl AsRef<Path>) -> Result<()> {
        self.backup_impl(path.as_ref(), None)
    }

    pub fn backup_complete_to(
        &self,
        path: impl AsRef<Path>,
        profile: &DesktopProfile,
    ) -> Result<()> {
        profile.validate()?;
        self.backup_impl(path.as_ref(), Some(profile))
    }

    fn backup_impl(&self, path: &Path, profile: Option<&DesktopProfile>) -> Result<()> {
        let file = OpenOptions::new().write(true).create_new(true).open(path)?;
        let result = (|| {
            let mut destination = Connection::open(path)?;
            copy_database(&self.connection, &mut destination)?;
            // A pending restoration is not a snapshot of the current frontend.
            // Legacy database-only backups deliberately carry no UI state.
            let has_profile: bool = destination.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='local_desktop_profile')", [], |row| row.get(0))?;
            if has_profile {
                destination.execute("DELETE FROM local_desktop_profile", [])?;
            }
            if let Some(profile) = profile {
                desktop_profile::write(
                    &destination,
                    &PendingDesktopProfile {
                        token: Uuid::new_v4().to_string(),
                        profile: profile.clone(),
                    },
                )?;
            }
            check_integrity(&destination)?;
            // A portable snapshot must have no required sidecar, even when its source
            // is in WAL mode. Closing this sole connection checkpoints its WAL.
            destination.pragma_update(None, "journal_mode", "DELETE")?;
            destination.close().map_err(|(_, error)| error)?;
            file.sync_all()?;
            Ok(())
        })();
        drop(file);
        if result.is_err() {
            let _ = std::fs::remove_file(path);
        }
        result
    }

    /// Restore a verified snapshot, returning the exact prevalidated timer and its
    /// new generation for infallible adoption by the application after commit.
    /// Caller must preserve its current journal first and stop concurrent service work.
    /// No source schema or data is written until validation and migration have passed
    /// in an isolated in-memory copy. Failure leaves the destination transaction intact.
    pub fn restore_from(&mut self, path: impl AsRef<Path>) -> Result<(TimerEngine, u64)> {
        self.restore_impl(path.as_ref(), None, None)
            .map(|(engine, revision, _)| (engine, revision))
    }

    /// Preview and commit each validate an isolated copy. The expected profile
    /// prevents replacing data with preferences that were never frontend-validated.
    pub fn inspect_backup_profile(path: impl AsRef<Path>) -> Result<Option<DesktopProfile>> {
        Ok(stage_backup(path.as_ref())?
            .pending_desktop_profile()?
            .map(|pending| pending.profile))
    }

    pub fn restore_complete_from(
        &mut self,
        path: impl AsRef<Path>,
        expected_profile: &Option<DesktopProfile>,
        sample: ClockSample,
    ) -> Result<(TimerEngine, u64, Option<PendingDesktopProfile>)> {
        self.restore_impl(path.as_ref(), Some(expected_profile), Some(sample))
    }

    fn restore_impl(
        &mut self,
        path: &Path,
        expected_profile: Option<&Option<DesktopProfile>>,
        desktop_sample: Option<ClockSample>,
    ) -> Result<(TimerEngine, u64, Option<PendingDesktopProfile>)> {
        if let Some(current) = self.connection.path()
            && !current.is_empty()
            && same_file::is_same_file(current, path)?
        {
            return Err(Error::ActiveDatabase);
        }
        let mut staged = stage_backup(path)?;
        let profile = staged
            .pending_desktop_profile()?
            .map(|pending| pending.profile);
        if expected_profile.is_some_and(|expected| expected != &profile) {
            return Err(Error::BackupProfileChanged);
        }
        let pending_profile = profile.map(|profile| PendingDesktopProfile {
            token: Uuid::new_v4().to_string(),
            profile,
        });
        let mut engine = match staged.load_timer()? {
            Some(timer) => TimerEngine::from_checkpoint(timer.checkpoint)?,
            None => TimerEngine::default(),
        };
        // Even a same-process backup cannot resume an old running clock anchor.
        // The domain closes its verified interval without crediting the missing time.
        let terminal = if let Some(sample) = desktop_sample {
            engine.apply(Command::Interrupt, sample)?
        } else if matches!(engine.state(), TimerState::Running(_)) {
            engine.apply(
                Command::Poll,
                ClockSample {
                    epoch: Uuid::new_v4().to_string(),
                    monotonic_ms: 0,
                    utc_ms: 0,
                },
            )?
        } else {
            None
        };
        let generation = self
            .load_timer()?
            .map_or(Ok(1), |saved| crate::next_version(saved.revision))?;
        let generation_sql = crate::as_i64(generation)?;
        let identity = Uuid::new_v4().to_string();
        let transaction = staged.connection.transaction()?;
        transaction.execute("DELETE FROM sync_outbox", [])?;
        transaction.execute("DELETE FROM provider_bindings", [])?;
        transaction.execute(
            "UPDATE local_identity SET device_id=?1,next_sequence=1 WHERE singleton=1",
            [&identity],
        )?;
        if let Some(pending) = &pending_profile {
            desktop_profile::write(&transaction, pending)?;
        }
        transaction.commit()?;
        staged.device_id = identity.clone();
        let saved_revision = staged.load_timer()?.map(|timer| timer.revision);
        staged.save_timer(saved_revision, &engine.checkpoint(), terminal.as_ref())?;
        staged.connection.execute(
            "UPDATE local_timer_checkpoint SET generation=?1 WHERE singleton=1",
            [generation_sql],
        )?;
        // sqlite3_backup holds one destination transaction until DONE; dropping an
        // unfinished backup rolls it back. No fallible reads follow this commit.
        copy_database(&staged.connection, &mut self.connection)?;
        self.device_id = identity;
        self.migration_backup = None;
        self.period_note_state = staged.period_note_state;
        Ok((engine, generation, pending_profile))
    }
}

fn stage_backup(path: &Path) -> Result<Store> {
    let source = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let mut staged = Connection::open_in_memory()?;
    copy_database(&source, &mut staged)?;
    drop(source);
    check_integrity(&staged)?;
    let recognized: bool = staged.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='schema_migrations')", [], |row| row.get(0))?;
    let version: i64 = staged.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if !recognized || version == 0 {
        return Err(Error::UnrecognizedDatabase);
    }
    migrations::migrate(&mut staged, None)?;
    let canonical = Store::open_in_memory()?;
    if schema(&staged)? != schema(&canonical.connection)? {
        return Err(Error::UnrecognizedDatabase);
    }
    let staged = Store::initialize(staged, None)?;
    validate_snapshot(&staged)?;
    staged.pending_desktop_profile()?;
    Ok(staged)
}

fn copy_database(source: &Connection, destination: &mut Connection) -> Result<()> {
    let backup = Backup::new(source, destination)?;
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        match backup.step(256)? {
            StepResult::Done => return Ok(()),
            StepResult::More => {}
            StepResult::Busy | StepResult::Locked => std::thread::sleep(Duration::from_millis(10)),
            _ => {}
        }
        if Instant::now() >= deadline {
            return Err(Error::BackupBusy);
        }
    }
}

fn check_integrity(connection: &Connection) -> Result<()> {
    let check: String = connection.query_row("PRAGMA integrity_check", [], |row| row.get(0))?;
    if check != "ok" {
        return Err(Error::IntegrityCheck(check));
    }
    let mut check = connection.prepare("PRAGMA foreign_key_check")?;
    if check.query([])?.next()?.is_some() {
        return Err(Error::IntegrityCheck("Broken record references".into()));
    }
    Ok(())
}

fn schema(connection: &Connection) -> Result<Vec<(String, String, String)>> {
    let mut statement = connection.prepare(
        "SELECT type,name,sql FROM sqlite_master WHERE name NOT LIKE 'sqlite_%' ORDER BY type,name",
    )?;
    Ok(statement
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?
        .collect::<rusqlite::Result<_>>()?)
}

fn validate_snapshot(store: &Store) -> Result<()> {
    // Validate every revision, including previous versions and session records,
    // before any restored bytes can replace the destination database.
    let mut sources = store.connection.prepare("SELECT source_timezone,source_utc_offset_minutes FROM local_period_notes UNION ALL SELECT source_timezone,source_utc_offset_minutes FROM reflection_drafts UNION ALL SELECT source_timezone,source_utc_offset_minutes FROM record_revisions")?;
    for source in sources.query_map([], |row| {
        Ok((
            row.get::<_, Option<String>>(0)?,
            row.get::<_, Option<i32>>(1)?,
        ))
    })? {
        let (zone, offset) = source?;
        crate::source_time::validate(zone.as_deref(), offset)?;
    }
    for note in store.all_period_notes()? {
        crate::validate_id(&note.id)?;
        if !["focus", "break"].contains(&note.kind.as_str())
            || note.version == 0
            || note.body.len() > crate::MAX_REFLECTION_BYTES
        {
            return Err(Error::IntegrityCheck("Invalid period note".into()));
        }
    }
    let notes = store.period_notes()?;
    if notes
        .period_note
        .as_ref()
        .is_some_and(|note| note.archived_at_utc_ms.is_some())
    {
        return Err(Error::IntegrityCheck(
            "Current period note is archived".into(),
        ));
    }
    store.list_routines()?;
    check_integrity(&store.connection)?;
    if let Some(timer) = store.load_timer()? {
        let active = match timer.checkpoint.state {
            TimerState::Running(active)
            | TimerState::Paused(active)
            | TimerState::RecoveryRequired(active) => Some(active.id),
            TimerState::Ready { .. } => None,
        };
        if let Some(id) = active
            && store.get_session(&id)?.is_some()
        {
            return Err(Error::SessionIntegrity(id));
        }
    }
    for draft in store.list_drafts()? {
        crate::validate_id(&draft.id)?;
        validate_content(&draft.content)?;
        if draft.record_id.is_none() && !draft.base_revision_ids.is_empty() {
            return Err(Error::IntegrityCheck(
                "Draft has revision bases but no record".into(),
            ));
        }
        if let Some(record) = &draft.record_id {
            let reflection = store
                .get_reflection(record)?
                .ok_or_else(|| Error::ReflectionNotFound(record.clone()))?;
            if reflection.heads.is_empty() || draft.base_revision_ids.is_empty() {
                return Err(Error::RevisionIntegrity(record.clone()));
            }
            for base in &draft.base_revision_ids {
                let exists: bool = store.connection.query_row("SELECT EXISTS(SELECT 1 FROM record_revisions WHERE record_id=?1 AND revision_id=?2)", (record, base), |row| row.get(0))?;
                if !exists {
                    return Err(Error::RevisionIntegrity(base.clone()));
                }
            }
        }
    }
    let mut records = store
        .connection
        .prepare("SELECT record_id,kind FROM records")?;
    for record in records.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })? {
        let (id, kind) = record?;
        crate::validate_id(&id)?;
        if kind == "reflection" {
            let reflection = store
                .get_reflection(&id)?
                .ok_or_else(|| Error::ReflectionNotFound(id.clone()))?;
            if reflection.heads.is_empty() {
                return Err(Error::RevisionIntegrity(id));
            }
        } else {
            // Sync retains every terminal variant. Contradictory sessions have
            // no definitive projection, while identical payloads may share one.
            let variants: i64 = store.connection.query_row(
                "SELECT count(DISTINCT payload_json) FROM record_revisions WHERE record_id=?1",
                [&id],
                |row| row.get(0),
            )?;
            let projection = store.get_session(&id)?;
            if (variants == 1) != projection.is_some() {
                return Err(Error::SessionIntegrity(id));
            }
            let consistent: bool = store.connection.query_row("SELECT (SELECT count(*) FROM record_revisions WHERE record_id=?1)>0 AND (SELECT count(*) FROM revision_parents WHERE record_id=?1)=0 AND (SELECT count(*) FROM record_heads WHERE record_id=?1)=(SELECT count(*) FROM record_revisions WHERE record_id=?1) AND (?2>1 OR EXISTS(SELECT 1 FROM record_heads h JOIN terminal_sessions t ON t.session_id=h.record_id AND t.revision_id=h.revision_id WHERE h.record_id=?1))", (&id, variants), |row| row.get(0))?;
            if !consistent {
                return Err(Error::SessionIntegrity(id));
            }
        }
    }
    let mut statement = store.connection.prepare("SELECT v.revision_id,r.kind,v.payload_json,v.payload_sha256,v.record_id FROM record_revisions v JOIN records r ON r.record_id=v.record_id")?;
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, Vec<u8>>(3)?,
            row.get::<_, String>(4)?,
        ))
    })?;
    for row in rows {
        let (id, kind, payload, digest, record_id) = row?;
        crate::validate_id(&id)?;
        if Sha256::digest(payload.as_bytes()).as_slice() != digest {
            return Err(Error::RevisionIntegrity(id));
        }
        if kind == "reflection" {
            validate_content(&serde_json::from_str(&payload)?)?;
            store
                .get_reflection(&record_id)?
                .ok_or(Error::ReflectionNotFound(record_id))?;
        } else {
            let session: rhythm_domain::SessionRecord = serde_json::from_str(&payload)?;
            session.validate()?;
            if session.id != record_id {
                return Err(Error::SessionIntegrity(record_id));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
