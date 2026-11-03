//! Local SQLite persistence for Rhythm. No network or GUI code lives here.
//!
//! Successful writes are committed before returning. Finalization, history, outbox
//! insertion, and draft removal share one transaction; failed saves retain drafts.

mod backup;
mod desktop_profile;
mod export;
mod history_query;
mod migrations;
mod models;
mod period_notes;
mod routines;
mod source_time;
mod sync;

pub use desktop_profile::{
    DesktopProfile, PendingDesktopProfile, PortableBackground, PortableBackgrounds, PortableImage,
};
pub use history_query::{NoteHistoryQuery, NoteHistoryQueryCursor, NoteHistoryQueryPage};
pub use models::*;
pub use sync::{
    MAX_SYNC_ARCHIVE_BYTES, SyncArchive, SyncImportReport, SyncObject, SyncPeriodNote, SyncRevision,
};

use std::{
    path::{Path, PathBuf},
    time::Duration,
};

use rhythm_application::{PersistedTimer, TimerRepository};
use rhythm_domain::{Checkpoint, SessionRecord, TimerEngine, TimerError, TimerState};
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use sha2::{Digest, Sha256};
use thiserror::Error;
use uuid::Uuid;

pub const MAX_REFLECTION_BYTES: usize = 1_048_576;
pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Error)]
pub enum Error {
    #[error("The file operation could not finish: {0}")]
    Io(#[from] std::io::Error),
    #[error("Choose a backup file other than the active journal")]
    ActiveDatabase,
    #[error("The complete backup preferences are invalid: {0}")]
    InvalidDesktopProfile(&'static str),
    #[error("The selected backup preferences changed after preview. Select the file again.")]
    BackupProfileChanged,
    #[error("The database stayed busy; try again after other journal operations finish")]
    BackupBusy,
    #[error(transparent)]
    Routine(#[from] rhythm_domain::RoutineError),
    #[error("Routine {0} was not found")]
    RoutineNotFound(String),
    #[error("At most 100 routines can be saved")]
    TooManyRoutines,
    #[error("Timer state could not be validated: {0}")]
    TimerValidation(#[from] TimerError),
    #[error("SQLite could not complete the operation: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("Stored data could not be decoded: {0}")]
    Json(#[from] serde_json::Error),
    #[error("Database schema {found} is newer than supported schema {supported}")]
    NewerSchema { found: i64, supported: i64 },
    #[error("Migration {version} differs from the applied migration")]
    MigrationChecksum { version: i64 },
    #[error("Database migration history is inconsistent")]
    InvalidMigrationHistory,
    #[error("This file is not a recognized Rhythm database")]
    UnrecognizedDatabase,
    #[error("Database integrity check failed: {0}")]
    IntegrityCheck(String),
    #[error("Draft {0} was not found")]
    DraftNotFound(String),
    #[error("Reflection {0} was not found")]
    ReflectionNotFound(String),
    #[error("Draft was changed by another editor (expected version {expected}, current {actual})")]
    DraftConflict { expected: u64, actual: u64 },
    #[error("This period note no longer exists")]
    PeriodNoteNotFound,
    #[error(
        "This period note changed in another editor. Your current words have not been replaced."
    )]
    PeriodNoteConflict,
    #[error("The imported note archive is invalid: {0}")]
    InvalidNoteArchive(&'static str),
    #[error("The synchronization archive is invalid: {0}")]
    InvalidSyncArchive(&'static str),
    #[error("Reflection {record_id} has changed; the draft is preserved for review")]
    ReflectionConflict {
        record_id: String,
        expected: Vec<String>,
        actual: Vec<String>,
    },
    #[error("Timer checkpoint changed; reload before applying this action")]
    TimerConflict,
    #[error("Session {0} already has different terminal data")]
    SessionConflict(String),
    #[error("A saved reflection must contain some text; an empty draft may remain unsaved")]
    EmptyReflection,
    #[error("Reflection exceeds the {MAX_REFLECTION_BYTES}-byte local limit")]
    ReflectionTooLarge,
    #[error("A required identifier is empty or too long")]
    InvalidIdentifier,
    #[error("A stored integer is outside the supported range")]
    IntegerRange,
    #[error("Stored revision integrity check failed for {0}")]
    RevisionIntegrity(String),
    #[error("Stored session projection does not match its immutable revision: {0}")]
    SessionIntegrity(String),
}

/// One connection with short serialized write transactions. Use it on the application's
/// storage worker; do not block a UI thread with filesystem operations.
pub struct Store {
    connection: Connection,
    device_id: String,
    migration_backup: Option<PathBuf>,
    period_note_state: PeriodNoteState,
}

impl Store {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        Self::initialize(Connection::open(path)?, Some(path))
    }

    pub fn open_in_memory() -> Result<Self> {
        Self::initialize(Connection::open_in_memory()?, None)
    }

    fn initialize(mut connection: Connection, path: Option<&Path>) -> Result<Self> {
        connection.busy_timeout(Duration::from_secs(5))?;
        connection.pragma_update(None, "foreign_keys", true)?;
        connection.pragma_update(None, "synchronous", "FULL")?;
        let migration_backup = migrations::migrate(&mut connection, path)?;
        if path.is_some() {
            connection.pragma_update(None, "journal_mode", "WAL")?;
        }
        connection.execute(
            "INSERT OR IGNORE INTO local_identity(singleton,device_id,next_sequence) VALUES(1,?1,1)",
            [Uuid::new_v4().to_string()],
        )?;
        let device_id = connection.query_row(
            "SELECT device_id FROM local_identity WHERE singleton=1",
            [],
            |row| row.get(0),
        )?;
        let mut store = Self {
            connection,
            device_id,
            migration_backup,
            period_note_state: PeriodNoteState::default(),
        };
        store.initialize_period_notes()?;
        Ok(store)
    }

    pub fn device_id(&self) -> &str {
        &self.device_id
    }

    pub fn migration_backup(&self) -> Option<&Path> {
        self.migration_backup.as_deref()
    }

    pub fn create_draft(&mut self, content: ReflectionInput, now_utc_ms: i64) -> Result<Draft> {
        validate_content(&content)?;
        let (source_timezone, source_utc_offset_minutes) = source_time::system(now_utc_ms);
        let draft = Draft {
            id: Uuid::new_v4().to_string(),
            record_id: None,
            base_revision_ids: Vec::new(),
            content,
            version: 1,
            created_at_utc_ms: now_utc_ms,
            updated_at_utc_ms: now_utc_ms,
            source_timezone,
            source_utc_offset_minutes,
        };
        insert_draft(&self.connection, &draft)?;
        Ok(draft)
    }

    /// Start editing the sole current version. Multiple variants require an explicit
    /// reviewed-head selection through `edit_reflection_heads`.
    pub fn edit_reflection(&mut self, record_id: &str, now_utc_ms: i64) -> Result<Draft> {
        let reflection = self
            .get_reflection(record_id)?
            .ok_or_else(|| Error::ReflectionNotFound(record_id.into()))?;
        if reflection.heads.len() != 1 {
            return Err(Error::ReflectionConflict {
                record_id: record_id.into(),
                expected: Vec::new(),
                actual: reflection
                    .heads
                    .iter()
                    .map(|head| head.id.clone())
                    .collect(),
            });
        }
        let head = &reflection.heads[0];
        self.edit_reflection_heads(
            record_id,
            std::slice::from_ref(&head.id),
            head.content.clone(),
            now_utc_ms,
        )
    }

    /// Begin a deliberate resolution of exactly the current versions the user reviewed.
    pub fn edit_reflection_heads(
        &mut self,
        record_id: &str,
        reviewed_heads: &[String],
        content: ReflectionInput,
        now_utc_ms: i64,
    ) -> Result<Draft> {
        validate_content(&content)?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        ensure_reflection(&transaction, record_id)?;
        let actual = head_ids(&transaction, record_id)?;
        let expected = sorted_ids(reviewed_heads);
        if actual.is_empty() {
            return Err(Error::ReflectionNotFound(record_id.into()));
        }
        if actual != expected {
            return Err(Error::ReflectionConflict {
                record_id: record_id.into(),
                expected,
                actual,
            });
        }
        let (source_timezone, source_utc_offset_minutes) = source_time::system(now_utc_ms);
        let draft = Draft {
            id: Uuid::new_v4().to_string(),
            record_id: Some(record_id.into()),
            base_revision_ids: actual,
            content,
            version: 1,
            created_at_utc_ms: now_utc_ms,
            updated_at_utc_ms: now_utc_ms,
            source_timezone,
            source_utc_offset_minutes,
        };
        insert_draft(&transaction, &draft)?;
        transaction.commit()?;
        Ok(draft)
    }

    pub fn save_draft(
        &mut self,
        draft_id: &str,
        expected_version: u64,
        body: &str,
        now_utc_ms: i64,
    ) -> Result<Draft> {
        if body.len() > MAX_REFLECTION_BYTES {
            return Err(Error::ReflectionTooLarge);
        }
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut draft = read_draft(&transaction, draft_id)?
            .ok_or_else(|| Error::DraftNotFound(draft_id.into()))?;
        check_draft_version(&draft, expected_version)?;
        draft.content.body = body.into();
        draft.version = next_version(draft.version)?;
        draft.updated_at_utc_ms = now_utc_ms;
        transaction.execute(
            "UPDATE reflection_drafts SET payload_json=?1, version=?2, updated_at_utc_ms=?3 WHERE draft_id=?4",
            params![serde_json::to_string(&draft.content)?, as_i64(draft.version)?, now_utc_ms, draft_id],
        )?;
        transaction.commit()?;
        Ok(draft)
    }

    pub fn get_draft(&self, draft_id: &str) -> Result<Option<Draft>> {
        read_draft(&self.connection, draft_id)
    }

    pub fn list_drafts(&self) -> Result<Vec<Draft>> {
        let mut statement = self.connection.prepare(
            "SELECT draft_id FROM reflection_drafts ORDER BY updated_at_utc_ms DESC, draft_id",
        )?;
        let ids = statement
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        ids.into_iter()
            .map(|id| read_draft(&self.connection, &id)?.ok_or(Error::DraftNotFound(id)))
            .collect()
    }

    pub fn discard_draft(&mut self, draft_id: &str, expected_version: u64) -> Result<()> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let draft = read_draft(&transaction, draft_id)?
            .ok_or_else(|| Error::DraftNotFound(draft_id.into()))?;
        check_draft_version(&draft, expected_version)?;
        transaction.execute(
            "DELETE FROM reflection_drafts WHERE draft_id=?1",
            [draft_id],
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub fn finalize_draft(
        &mut self,
        draft_id: &str,
        expected_version: u64,
        now_utc_ms: i64,
    ) -> Result<ReflectionRevision> {
        self.finalize(draft_id, expected_version, now_utc_ms, false)
    }

    /// Explicitly preserve a stale draft as a parallel variant. This is a user decision,
    /// never an automatic fallback when `finalize_draft` reports a conflict.
    pub fn finalize_draft_as_variant(
        &mut self,
        draft_id: &str,
        expected_version: u64,
        now_utc_ms: i64,
    ) -> Result<ReflectionRevision> {
        self.finalize(draft_id, expected_version, now_utc_ms, true)
    }

    fn finalize(
        &mut self,
        draft_id: &str,
        expected_version: u64,
        now_utc_ms: i64,
        preserve_variant: bool,
    ) -> Result<ReflectionRevision> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let draft = read_draft(&transaction, draft_id)?
            .ok_or_else(|| Error::DraftNotFound(draft_id.into()))?;
        check_draft_version(&draft, expected_version)?;
        validate_content(&draft.content)?;
        if draft.content.body.trim().is_empty() {
            return Err(Error::EmptyReflection);
        }
        let record_id = match &draft.record_id {
            Some(record_id) => {
                ensure_reflection(&transaction, record_id)?;
                let actual = head_ids(&transaction, record_id)?;
                if !preserve_variant && actual != sorted_ids(&draft.base_revision_ids) {
                    return Err(Error::ReflectionConflict {
                        record_id: record_id.clone(),
                        expected: draft.base_revision_ids.clone(),
                        actual,
                    });
                }
                record_id.clone()
            }
            None => {
                let id = Uuid::new_v4().to_string();
                transaction.execute("INSERT INTO records(record_id,kind,created_at_utc_ms) VALUES(?1,'reflection',?2)", (&id, now_utc_ms))?;
                id
            }
        };
        let (id, _) = insert_revision(
            &transaction,
            &record_id,
            &draft.base_revision_ids,
            &serde_json::to_string(&draft.content)?,
            now_utc_ms,
            &self.device_id,
        )?;
        transaction.execute(
            "DELETE FROM reflection_drafts WHERE draft_id=?1",
            [draft_id],
        )?;
        let revision = read_revision(&transaction, &id)?;
        transaction.commit()?;
        Ok(revision)
    }

    pub fn get_reflection(&self, record_id: &str) -> Result<Option<Reflection>> {
        let created_at_utc_ms: Option<i64> = self
            .connection
            .query_row(
                "SELECT created_at_utc_ms FROM records WHERE record_id=?1 AND kind='reflection'",
                [record_id],
                |row| row.get(0),
            )
            .optional()?;
        created_at_utc_ms
            .map(|created_at_utc_ms| {
                let heads = head_ids(&self.connection, record_id)?
                    .into_iter()
                    .map(|id| read_revision(&self.connection, &id))
                    .collect::<Result<Vec<_>>>()?;
                Ok(Reflection {
                    id: record_id.into(),
                    heads,
                    created_at_utc_ms,
                })
            })
            .transpose()
    }

    /// Stable creation-time order. The bound avoids accidentally loading a whole journal.
    pub fn list_reflections(&self, limit: u32, offset: u32) -> Result<Vec<Reflection>> {
        let mut statement = self.connection.prepare("SELECT record_id FROM records WHERE kind='reflection' ORDER BY created_at_utc_ms DESC,record_id LIMIT ?1 OFFSET ?2")?;
        let ids = statement
            .query_map((limit.min(1_000), offset), |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        ids.into_iter()
            .map(|id| {
                self.get_reflection(&id)?
                    .ok_or(Error::ReflectionNotFound(id))
            })
            .collect()
    }

    pub fn revision_history(&self, record_id: &str) -> Result<Vec<ReflectionRevision>> {
        let mut statement = self.connection.prepare("SELECT r.revision_id FROM record_revisions r JOIN records rec ON rec.record_id=r.record_id WHERE r.record_id=?1 AND rec.kind='reflection' ORDER BY r.author_sequence,r.revision_id")?;
        let ids = statement
            .query_map([record_id], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        ids.into_iter()
            .map(|id| read_revision(&self.connection, &id))
            .collect()
    }

    /// Register a local binding and backfill every revision, including ancestors.
    /// This does not authorize or contact a provider.
    pub fn register_provider_binding(&mut self, binding_id: &str) -> Result<()> {
        validate_id(binding_id)?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute("UPDATE provider_bindings SET active=0 WHERE active=1", [])?;
        transaction.execute("INSERT INTO provider_bindings(binding_id,active) VALUES(?1,1) ON CONFLICT(binding_id) DO UPDATE SET active=1", [binding_id])?;
        transaction.execute("INSERT OR IGNORE INTO sync_outbox(provider_binding_id,revision_id) SELECT ?1,revision_id FROM record_revisions", [binding_id])?;
        transaction.commit()?;
        Ok(())
    }

    pub fn disconnect_provider(&mut self) -> Result<()> {
        self.connection
            .execute("UPDATE provider_bindings SET active=0", [])?;
        Ok(())
    }

    pub fn outbox_items(&self, binding_id: &str) -> Result<Vec<OutboxItem>> {
        let mut statement = self.connection.prepare("SELECT provider_binding_id,revision_id,remote_upload_identity FROM sync_outbox WHERE provider_binding_id=?1 ORDER BY revision_id")?;
        Ok(statement
            .query_map([binding_id], |row| {
                Ok(OutboxItem {
                    provider_binding_id: row.get(0)?,
                    revision_id: row.get(1)?,
                    remote_upload_identity: row.get(2)?,
                })
            })?
            .collect::<rusqlite::Result<_>>()?)
    }

    pub fn load_timer(&self) -> Result<Option<PersistedTimer>> {
        self.load_checkpoint()?
            .map(|stored| {
                let checkpoint: Checkpoint = serde_json::from_str(&stored.json)?;
                TimerEngine::from_checkpoint(checkpoint.clone())?;
                Ok(PersistedTimer {
                    revision: stored.generation,
                    checkpoint,
                })
            })
            .transpose()
    }

    pub fn save_timer(
        &mut self,
        expected_revision: Option<u64>,
        checkpoint: &Checkpoint,
        terminal: Option<&SessionRecord>,
    ) -> Result<u64> {
        TimerEngine::from_checkpoint(checkpoint.clone())?;
        if let Some(session) = terminal {
            session.validate()?;
        }
        let active_id = match &checkpoint.state {
            TimerState::Running(period)
            | TimerState::Paused(period)
            | TimerState::RecoveryRequired(period) => Some(period.id.as_str()),
            TimerState::Ready { .. } => None,
        };
        let session = terminal
            .map(|session| -> Result<StoredSession> {
                Ok(StoredSession {
                    id: session.id.clone(),
                    json: serde_json::to_string(session)?,
                })
            })
            .transpose()?;
        self.commit_checkpoint_with_active(
            expected_revision,
            &serde_json::to_string(checkpoint)?,
            session.as_ref(),
            active_id,
            Some((checkpoint, terminal)),
        )
    }

    pub fn get_session(&self, session_id: &str) -> Result<Option<SessionRecord>> {
        type SessionRow = (String, String, Vec<u8>, String, String);
        let row: Option<SessionRow> = self.connection.query_row(
            "SELECT t.payload_json,v.payload_json,v.payload_sha256,v.record_id,r.kind FROM terminal_sessions t JOIN record_revisions v ON v.revision_id=t.revision_id JOIN records r ON r.record_id=v.record_id WHERE t.session_id=?1",
            [session_id], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?)),
        ).optional()?;
        row.map(|(projection, payload, digest, record_id, kind)| {
            if projection != payload
                || record_id != session_id
                || kind != "session"
                || Sha256::digest(payload.as_bytes()).to_vec() != digest
            {
                return Err(Error::SessionIntegrity(session_id.into()));
            }
            let session: SessionRecord = serde_json::from_str(&payload)?;
            session.validate()?;
            if session.id != session_id {
                return Err(Error::SessionIntegrity(session_id.into()));
            }
            Ok(session)
        })
        .transpose()
    }

    pub fn list_sessions(&self, limit: u32, offset: u32) -> Result<Vec<SessionRecord>> {
        let mut statement = self.connection.prepare("SELECT t.session_id FROM terminal_sessions t JOIN records r ON r.record_id=t.session_id ORDER BY r.created_at_utc_ms DESC,r.record_id LIMIT ?1 OFFSET ?2")?;
        let ids = statement
            .query_map((limit.min(1_000), offset), |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        ids.into_iter()
            .map(|id| self.get_session(&id)?.ok_or(Error::SessionIntegrity(id)))
            .collect()
    }

    fn load_checkpoint(&self) -> Result<Option<StoredCheckpoint>> {
        let row: Option<(i64, String)> = self
            .connection
            .query_row(
                "SELECT generation,checkpoint_json FROM local_timer_checkpoint WHERE singleton=1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        row.map(|(generation, json)| {
            Ok(StoredCheckpoint {
                generation: as_u64(generation)?,
                json,
            })
        })
        .transpose()
    }

    #[cfg(test)]
    fn commit_checkpoint(
        &mut self,
        expected_revision: Option<u64>,
        checkpoint_json: &str,
        terminal: Option<&StoredSession>,
    ) -> Result<u64> {
        self.commit_checkpoint_with_active(expected_revision, checkpoint_json, terminal, None, None)
    }

    fn commit_checkpoint_with_active(
        &mut self,
        expected_revision: Option<u64>,
        checkpoint_json: &str,
        terminal: Option<&StoredSession>,
        active_session_id: Option<&str>,
        note_transition: Option<(&Checkpoint, Option<&SessionRecord>)>,
    ) -> Result<u64> {
        serde_json::from_str::<serde_json::Value>(checkpoint_json)?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current: Option<i64> = transaction
            .query_row(
                "SELECT generation FROM local_timer_checkpoint WHERE singleton=1",
                [],
                |row| row.get(0),
            )
            .optional()?;
        let current = current.map(as_u64).transpose()?;
        if current != expected_revision {
            return Err(Error::TimerConflict);
        }
        if let Some(active_id) = active_session_id {
            let terminal_exists: bool = transaction.query_row(
                "SELECT EXISTS(SELECT 1 FROM terminal_sessions WHERE session_id=?1)",
                [active_id],
                |row| row.get(0),
            )?;
            if terminal_exists {
                return Err(Error::SessionConflict(active_id.into()));
            }
        }
        let next = next_version(current.unwrap_or(0))?;
        if let Some(session) = terminal {
            validate_id(&session.id)?;
            let decoded: serde_json::Value = serde_json::from_str(&session.json)?;
            let existing: Option<String> = transaction
                .query_row(
                    "SELECT payload_json FROM terminal_sessions WHERE session_id=?1",
                    [&session.id],
                    |row| row.get(0),
                )
                .optional()?;
            match existing {
                Some(existing) if existing != session.json => {
                    return Err(Error::SessionConflict(session.id.clone()));
                }
                Some(_) => {}
                None => {
                    let started = decoded
                        .get("started_utc_ms")
                        .and_then(serde_json::Value::as_i64)
                        .unwrap_or(0);
                    let detected = decoded
                        .get("detected_utc_ms")
                        .and_then(serde_json::Value::as_i64)
                        .unwrap_or(started);
                    transaction.execute("INSERT INTO records(record_id,kind,created_at_utc_ms) VALUES(?1,'session',?2)", (&session.id,started))?;
                    let (revision_id, _) = insert_revision(
                        &transaction,
                        &session.id,
                        &[],
                        &session.json,
                        detected,
                        &self.device_id,
                    )?;
                    transaction.execute("INSERT INTO terminal_sessions(session_id,revision_id,payload_json) VALUES(?1,?2,?3)", (&session.id,revision_id,&session.json))?;
                }
            }
        }
        transaction.execute("INSERT INTO local_timer_checkpoint(singleton,generation,checkpoint_json) VALUES(1,?1,?2) ON CONFLICT(singleton) DO UPDATE SET generation=excluded.generation,checkpoint_json=excluded.checkpoint_json", (as_i64(next)?,checkpoint_json))?;
        if let Some((checkpoint, terminal)) = note_transition {
            period_notes::reconcile(&transaction, checkpoint, terminal)?;
        }
        let note_state = period_notes::read_state(&transaction)?;
        transaction.commit()?;
        self.period_note_state = note_state;
        Ok(next)
    }
}

impl TimerRepository for Store {
    type Error = Error;
    fn load_timer(&self) -> Result<Option<PersistedTimer>> {
        Store::load_timer(self)
    }
    fn save_timer(
        &mut self,
        expected_revision: Option<u64>,
        checkpoint: &Checkpoint,
        terminal: Option<&SessionRecord>,
    ) -> Result<u64> {
        Store::save_timer(self, expected_revision, checkpoint, terminal)
    }
}

fn validate_id(id: &str) -> Result<()> {
    if id.is_empty() || id.len() > 256 {
        Err(Error::InvalidIdentifier)
    } else {
        Ok(())
    }
}

fn validate_content(content: &ReflectionInput) -> Result<()> {
    if content.body.len() > MAX_REFLECTION_BYTES {
        return Err(Error::ReflectionTooLarge);
    }
    if let Some(id) = &content.session_id {
        validate_id(id)?;
    }
    Ok(())
}

fn as_i64(value: u64) -> Result<i64> {
    i64::try_from(value).map_err(|_| Error::IntegerRange)
}
fn as_u64(value: i64) -> Result<u64> {
    u64::try_from(value).map_err(|_| Error::IntegerRange)
}
fn next_version(value: u64) -> Result<u64> {
    let next = value.checked_add(1).ok_or(Error::IntegerRange)?;
    as_i64(next)?;
    Ok(next)
}

fn sorted_ids(ids: &[String]) -> Vec<String> {
    let mut ids = ids.to_vec();
    ids.sort();
    ids.dedup();
    ids
}

fn check_draft_version(draft: &Draft, expected: u64) -> Result<()> {
    if draft.version != expected {
        Err(Error::DraftConflict {
            expected,
            actual: draft.version,
        })
    } else {
        Ok(())
    }
}

fn insert_draft(connection: &Connection, draft: &Draft) -> Result<()> {
    source_time::validate(
        draft.source_timezone.as_deref(),
        draft.source_utc_offset_minutes,
    )?;
    connection.execute("INSERT INTO reflection_drafts(draft_id,record_id,bases_json,payload_json,version,created_at_utc_ms,updated_at_utc_ms,source_timezone,source_utc_offset_minutes) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)", params![draft.id,draft.record_id,serde_json::to_string(&draft.base_revision_ids)?,serde_json::to_string(&draft.content)?,as_i64(draft.version)?,draft.created_at_utc_ms,draft.updated_at_utc_ms,draft.source_timezone,draft.source_utc_offset_minutes])?;
    Ok(())
}

fn read_draft(connection: &Connection, id: &str) -> Result<Option<Draft>> {
    type Row = (
        String,
        Option<String>,
        String,
        String,
        i64,
        i64,
        i64,
        Option<String>,
        Option<i32>,
    );
    let row: Option<Row> = connection.query_row("SELECT draft_id,record_id,bases_json,payload_json,version,created_at_utc_ms,updated_at_utc_ms,source_timezone,source_utc_offset_minutes FROM reflection_drafts WHERE draft_id=?1", [id], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?,row.get(5)?,row.get(6)?,row.get(7)?,row.get(8)?))).optional()?;
    row.map(
        |(
            id,
            record_id,
            bases,
            payload,
            version,
            created_at_utc_ms,
            updated_at_utc_ms,
            source_timezone,
            source_utc_offset_minutes,
        )| {
            source_time::validate(source_timezone.as_deref(), source_utc_offset_minutes)?;
            Ok(Draft {
                id,
                record_id,
                base_revision_ids: serde_json::from_str(&bases)?,
                content: serde_json::from_str(&payload)?,
                version: as_u64(version)?,
                created_at_utc_ms,
                updated_at_utc_ms,
                source_timezone,
                source_utc_offset_minutes,
            })
        },
    )
    .transpose()
}

fn head_ids(connection: &Connection, record_id: &str) -> Result<Vec<String>> {
    let mut statement = connection
        .prepare("SELECT revision_id FROM record_heads WHERE record_id=?1 ORDER BY revision_id")?;
    Ok(statement
        .query_map([record_id], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?)
}

fn ensure_reflection(connection: &Connection, record_id: &str) -> Result<()> {
    let exists: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM records WHERE record_id=?1 AND kind='reflection')",
        [record_id],
        |row| row.get(0),
    )?;
    if exists {
        Ok(())
    } else {
        Err(Error::ReflectionNotFound(record_id.into()))
    }
}

fn insert_revision(
    transaction: &Transaction<'_>,
    record_id: &str,
    parents: &[String],
    payload: &str,
    created_at: i64,
    device_id: &str,
) -> Result<(String, u64)> {
    let sequence: i64 = transaction.query_row(
        "SELECT next_sequence FROM local_identity WHERE singleton=1",
        [],
        |row| row.get(0),
    )?;
    let next = sequence.checked_add(1).ok_or(Error::IntegerRange)?;
    transaction.execute(
        "UPDATE local_identity SET next_sequence=?1 WHERE singleton=1",
        [next],
    )?;
    let revision_id = Uuid::new_v4().to_string();
    let digest = Sha256::digest(payload.as_bytes()).to_vec();
    let (source_timezone, source_utc_offset_minutes) = source_time::system(created_at);
    transaction.execute("INSERT INTO record_revisions(revision_id,record_id,author_device_id,author_sequence,schema_version,created_at_utc_ms,payload_json,payload_sha256,source_timezone,source_utc_offset_minutes) VALUES(?1,?2,?3,?4,1,?5,?6,?7,?8,?9)", params![revision_id,record_id,device_id,sequence,created_at,payload,digest,source_timezone,source_utc_offset_minutes])?;
    for parent in sorted_ids(parents) {
        transaction.execute("INSERT INTO revision_parents(record_id,revision_id,parent_revision_id) VALUES(?1,?2,?3)", (record_id,&revision_id,&parent))?;
        transaction.execute(
            "DELETE FROM record_heads WHERE record_id=?1 AND revision_id=?2",
            (record_id, &parent),
        )?;
    }
    transaction.execute(
        "INSERT INTO record_heads(record_id,revision_id) VALUES(?1,?2)",
        (record_id, &revision_id),
    )?;
    transaction.execute("INSERT INTO sync_outbox(provider_binding_id,revision_id) SELECT binding_id,?1 FROM provider_bindings WHERE active=1", [&revision_id])?;
    Ok((revision_id, as_u64(sequence)?))
}

fn read_revision(connection: &Connection, revision_id: &str) -> Result<ReflectionRevision> {
    type RevisionRow = (
        String,
        String,
        i64,
        i64,
        String,
        Vec<u8>,
        Option<String>,
        Option<i32>,
    );
    let (record_id,author_device_id,sequence,created_at_utc_ms,payload,digest,source_timezone,source_utc_offset_minutes): RevisionRow = connection.query_row("SELECT record_id,author_device_id,author_sequence,created_at_utc_ms,payload_json,payload_sha256,source_timezone,source_utc_offset_minutes FROM record_revisions WHERE revision_id=?1", [revision_id], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?,row.get(5)?,row.get(6)?,row.get(7)?)))?;
    source_time::validate(source_timezone.as_deref(), source_utc_offset_minutes)?;
    if Sha256::digest(payload.as_bytes()).as_slice() != digest.as_slice() {
        return Err(Error::RevisionIntegrity(revision_id.into()));
    }
    let mut statement = connection.prepare("SELECT parent_revision_id FROM revision_parents WHERE revision_id=?1 ORDER BY parent_revision_id")?;
    let parents = statement
        .query_map([revision_id], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    Ok(ReflectionRevision {
        id: revision_id.into(),
        record_id,
        parents,
        content: serde_json::from_str(&payload)?,
        author_device_id,
        author_sequence: as_u64(sequence)?,
        created_at_utc_ms,
        source_timezone,
        source_utc_offset_minutes,
    })
}

#[cfg(test)]
mod tests;
