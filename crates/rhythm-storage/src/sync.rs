//! Provider-independent, append-only exchange of authored history. The transport
//! authenticates/encrypts bytes and binds them to a vault; this layer never reads
//! credentials, contacts a provider, or replaces device-local state.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};

use rhythm_domain::{SessionRecord, TimerState};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{Error, MAX_REFLECTION_BYTES, PeriodNote, ReflectionInput, Result, Store, as_i64};

pub const MAX_SYNC_ARCHIVE_BYTES: usize = 64 * 1024 * 1024;
const MAX_SYNC_ITEMS: usize = 100_000;
const MAX_PARENTS: usize = 1_024;
const PERIOD_PREFIX: &str = "sync-period:";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SyncRevision {
    pub revision_id: String,
    pub record_id: String,
    pub kind: String,
    pub record_created_at_utc_ms: i64,
    pub parents: Vec<String>,
    pub author_device_id: String,
    pub author_sequence: u64,
    pub schema_version: u32,
    pub created_at_utc_ms: i64,
    /// Exact original bytes; never reserialize a revision's payload on import.
    pub payload_json: String,
    pub payload_sha256: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_timezone: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_utc_offset_minutes: Option<i32>,
}

/// A completed period's words are immutable variants in the exchange protocol.
/// A late save creates another variant instead of overwriting previously synced
/// words. Origin identity distinguishes independent notes with identical text.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SyncPeriodNote {
    pub origin_id: String,
    pub kind: String,
    pub body: String,
    pub created_at_utc_ms: i64,
    pub updated_at_utc_ms: i64,
    pub archived_at_utc_ms: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_timezone: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_utc_offset_minutes: Option<i32>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SyncArchive {
    pub protocol: u32,
    pub source_device_id: String,
    pub revisions: Vec<SyncRevision>,
    pub period_notes: Vec<SyncPeriodNote>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SyncObject {
    /// SHA-256 of the canonical logical content, independent of exporting device.
    pub id: String,
    pub archive: SyncArchive,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncImportReport {
    pub inserted_revisions: usize,
    pub inserted_notes: usize,
    pub conflicting_reflections: usize,
    pub conflicting_sessions: usize,
}

impl SyncArchive {
    /// Bound untrusted bytes before decoding. Unknown fields/versions fail closed.
    pub fn from_json(bytes: &[u8]) -> Result<Self> {
        if bytes.len() > MAX_SYNC_ARCHIVE_BYTES {
            return Err(invalid("the archive exceeds the 64 MiB limit"));
        }
        let archive: Self = serde_json::from_slice(bytes)?;
        archive.validate()?;
        Ok(archive)
    }

    pub fn to_json(&self) -> Result<Vec<u8>> {
        self.validate()?;
        // A body containing escaped control characters can expand sixfold in
        // JSON. Stop while writing, before allocating an oversized archive.
        let mut output = BoundedJson::default();
        let encoded = serde_json::to_writer(&mut output, self);
        if output.too_large {
            return Err(invalid("the archive exceeds the 64 MiB limit"));
        }
        encoded?;
        Ok(output.bytes)
    }

    fn validate(&self) -> Result<()> {
        if !matches!(self.protocol, 1 | 2) || (self.protocol == 1 && self.has_source_time()) {
            return Err(invalid("unsupported protocol version"));
        }
        crate::validate_id(&self.source_device_id)?;
        if self.revisions.len().saturating_add(self.period_notes.len()) > MAX_SYNC_ITEMS {
            return Err(invalid("the archive exceeds the 100,000-item limit"));
        }
        let mut bytes = 0_usize;
        let mut ids = HashSet::new();
        let mut sequences = HashSet::new();
        for revision in &self.revisions {
            revision.validate()?;
            if !ids.insert(&revision.revision_id)
                || !sequences.insert((&revision.author_device_id, revision.author_sequence))
            {
                return Err(invalid("duplicate revision identity or author sequence"));
            }
            bytes = bytes.saturating_add(revision.payload_json.len());
            bytes = bytes.saturating_add(revision.parents.iter().map(String::len).sum::<usize>());
            if bytes > MAX_SYNC_ARCHIVE_BYTES {
                return Err(invalid("the archive exceeds the 64 MiB limit"));
            }
        }
        for note in &self.period_notes {
            note.validate()?;
            bytes = bytes.saturating_add(note.body.len());
            if bytes > MAX_SYNC_ARCHIVE_BYTES {
                return Err(invalid("the archive exceeds the 64 MiB limit"));
            }
        }
        Ok(())
    }

    fn has_source_time(&self) -> bool {
        self.revisions.iter().any(|revision| {
            revision.source_timezone.is_some() || revision.source_utc_offset_minutes.is_some()
        }) || self
            .period_notes
            .iter()
            .any(|note| note.source_timezone.is_some() || note.source_utc_offset_minutes.is_some())
    }

    fn select_protocol(&mut self) {
        self.protocol = if self.has_source_time() { 2 } else { 1 };
    }
}

impl SyncRevision {
    fn validate(&self) -> Result<()> {
        crate::source_time::validate(
            self.source_timezone.as_deref(),
            self.source_utc_offset_minutes,
        )?;
        for id in [&self.revision_id, &self.record_id, &self.author_device_id] {
            crate::validate_id(id)?;
        }
        if self.schema_version != 1
            || self.author_sequence == 0
            || self.author_sequence >= i64::MAX as u64
        {
            return Err(invalid("unsupported revision schema or author sequence"));
        }
        as_i64(self.author_sequence)?;
        if self.payload_json.len() > MAX_REFLECTION_BYTES * 8 {
            return Err(invalid("a revision exceeds the payload size limit"));
        }
        if self.payload_sha256 != hash(self.payload_json.as_bytes()) {
            return Err(invalid("revision content does not match its digest"));
        }
        if self.parents.len() > MAX_PARENTS {
            return Err(invalid("a revision has too many parents"));
        }
        let mut parents = HashSet::new();
        for parent in &self.parents {
            crate::validate_id(parent)?;
            if parent == &self.revision_id || !parents.insert(parent) {
                return Err(invalid(
                    "a revision has a repeated or self-referencing parent",
                ));
            }
        }
        match self.kind.as_str() {
            "reflection" => {
                let value: ReflectionInput = serde_json::from_str(&self.payload_json)?;
                crate::validate_content(&value)?;
                if value.body.trim().is_empty() {
                    return Err(Error::EmptyReflection);
                }
            }
            "session" => {
                if !self.parents.is_empty() {
                    return Err(invalid("terminal sessions cannot have revision parents"));
                }
                let session: SessionRecord = serde_json::from_str(&self.payload_json)?;
                session.validate()?;
                if session.id != self.record_id
                    || session.started_utc_ms != self.record_created_at_utc_ms
                    || session.detected_utc_ms != self.created_at_utc_ms
                {
                    return Err(invalid(
                        "session identity or dates do not match its revision",
                    ));
                }
            }
            _ => return Err(invalid("unsupported record kind")),
        }
        Ok(())
    }
}

impl SyncPeriodNote {
    fn validate(&self) -> Result<()> {
        crate::source_time::validate(
            self.source_timezone.as_deref(),
            self.source_utc_offset_minutes,
        )?;
        if !is_digest(&self.origin_id)
            || !matches!(self.kind.as_str(), "focus" | "break")
            || self.body.len() > MAX_REFLECTION_BYTES
            || self.body.trim().is_empty()
            || [
                self.created_at_utc_ms,
                self.updated_at_utc_ms,
                self.archived_at_utc_ms,
            ]
            .into_iter()
            .any(|date| !(-8_640_000_000_000_000..=8_640_000_000_000_000).contains(&date))
        {
            return Err(invalid("invalid archived period note"));
        }
        Ok(())
    }

    fn from_note(note: &PeriodNote) -> Result<Self> {
        let origin_id = note
            .id
            .strip_prefix(PERIOD_PREFIX)
            .and_then(|value| value.split_once(':'))
            .filter(|(origin, variant)| is_digest(origin) && is_digest(variant))
            .map_or_else(|| hash(note.id.as_bytes()), |(origin, _)| origin.to_owned());
        Ok(Self {
            origin_id,
            kind: note.kind.clone(),
            body: note.body.clone(),
            created_at_utc_ms: note.created_at_utc_ms,
            updated_at_utc_ms: note.updated_at_utc_ms,
            archived_at_utc_ms: note
                .archived_at_utc_ms
                .ok_or(invalid("active notes cannot be synchronized"))?,
            source_timezone: note.source_timezone.clone(),
            source_utc_offset_minutes: note.source_utc_offset_minutes,
        })
    }

    fn local_id(&self) -> Result<String> {
        Ok(format!(
            "{PERIOD_PREFIX}{}:{}",
            self.origin_id,
            hash(&serde_json::to_vec(self)?)
        ))
    }
}

impl Store {
    /// One consistent read of all authored revisions and archived period notes.
    /// Drafts, active notes, timers, routines, preferences and credentials stay local.
    /// No partial archive is returned when a size limit is reached.
    pub fn export_sync_archive(&self) -> Result<SyncArchive> {
        let transaction = self.connection.unchecked_transaction()?;
        let archive = read_archive(&transaction, self.device_id.clone())?;
        archive.to_json()?;
        transaction.commit()?;
        Ok(archive)
    }

    /// Independent immutable objects avoid uploading the complete journal after
    /// every new note. A reflection object carries its complete ancestry, so it
    /// is safe to receive objects in any order. Previously uploaded objects must
    /// be retained by the transport, including after later edits or a restore.
    pub fn export_sync_objects(&self) -> Result<Vec<SyncObject>> {
        let transaction = self.connection.unchecked_transaction()?;
        let archive = read_archive(&transaction, "logical-record".into())?;
        let mut groups: BTreeMap<String, SyncArchive> = BTreeMap::new();
        for revision in archive.revisions {
            groups
                .entry(format!("record:{}", revision.record_id))
                .or_insert_with(empty_object_archive)
                .revisions
                .push(revision);
        }
        for note in archive.period_notes {
            groups
                .entry(note.local_id()?)
                .or_insert_with(empty_object_archive)
                .period_notes
                .push(note);
        }
        let objects = groups
            .into_values()
            .map(|mut archive| {
                archive.select_protocol();
                let id = hash(&archive.to_json()?);
                Ok(SyncObject { id, archive })
            })
            .collect::<Result<Vec<_>>>()?;
        transaction.commit()?;
        Ok(objects)
    }

    /// Atomically merge accepted history. Missing ancestry, malformed payloads,
    /// identity collisions, cycles or active-session collisions roll back the
    /// complete object, leaving the transport free to retry its retained bytes.
    pub fn import_sync_archive(&mut self, archive: &SyncArchive) -> Result<SyncImportReport> {
        archive.to_json()?;
        let active_id = self
            .load_timer()?
            .and_then(|saved| match saved.checkpoint.state {
                TimerState::Running(active)
                | TimerState::Paused(active)
                | TimerState::RecoveryRequired(active) => Some(active.id),
                TimerState::Ready { .. } => None,
            });
        if archive.revisions.iter().any(|revision| {
            revision.kind == "session" && active_id.as_ref() == Some(&revision.record_id)
        }) {
            return Err(invalid(
                "remote history conflicts with the active local session",
            ));
        }
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        validate_ancestry(&transaction, &archive.revisions)?;
        let mut report = SyncImportReport::default();
        let mut affected = BTreeSet::new();
        for revision in &archive.revisions {
            let record: Option<(String, i64)> = transaction
                .query_row(
                    "SELECT kind,created_at_utc_ms FROM records WHERE record_id=?1",
                    [&revision.record_id],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()?;
            match record {
                Some((kind, created))
                    if kind != revision.kind || created != revision.record_created_at_utc_ms =>
                {
                    return Err(invalid("a record identity has conflicting metadata"));
                }
                None => {
                    transaction.execute(
                        "INSERT INTO records(record_id,kind,created_at_utc_ms) VALUES(?1,?2,?3)",
                        params![
                            revision.record_id,
                            revision.kind,
                            revision.record_created_at_utc_ms
                        ],
                    )?;
                }
                _ => {}
            }
            if let Some(existing) = read_revision(&transaction, &revision.revision_id)? {
                let mut normalized = revision.clone();
                normalized.parents.sort();
                if existing != normalized {
                    return Err(invalid(
                        "an immutable revision identity has conflicting content",
                    ));
                }
                continue;
            }
            transaction.execute(
                "INSERT INTO record_revisions(revision_id,record_id,author_device_id,author_sequence,schema_version,created_at_utc_ms,payload_json,payload_sha256,source_timezone,source_utc_offset_minutes) VALUES(?1,?2,?3,?4,1,?5,?6,?7,?8,?9)",
                params![revision.revision_id,revision.record_id,revision.author_device_id,as_i64(revision.author_sequence)?,revision.created_at_utc_ms,revision.payload_json,Sha256::digest(revision.payload_json.as_bytes()).to_vec(),revision.source_timezone,revision.source_utc_offset_minutes],
            )?;
            affected.insert((revision.record_id.clone(), revision.kind.clone()));
            report.inserted_revisions += 1;
        }
        // Insert edges after every vertex, independent of arrival order.
        for revision in &archive.revisions {
            for parent in &revision.parents {
                transaction.execute("INSERT OR IGNORE INTO revision_parents(record_id,revision_id,parent_revision_id) VALUES(?1,?2,?3)", params![revision.record_id,revision.revision_id,parent])?;
            }
        }
        for (id, kind) in &affected {
            transaction.execute("DELETE FROM record_heads WHERE record_id=?1", [id])?;
            transaction.execute("INSERT INTO record_heads(record_id,revision_id) SELECT record_id,revision_id FROM record_revisions r WHERE record_id=?1 AND NOT EXISTS(SELECT 1 FROM revision_parents p WHERE p.parent_revision_id=r.revision_id)", [id])?;
            if kind == "session" {
                rebuild_session(&transaction, id)?;
            }
        }
        let existing_notes = read_period_notes(&transaction)?;
        let mut known = existing_notes
            .iter()
            .map(SyncPeriodNote::local_id)
            .collect::<Result<HashSet<_>>>()?;
        for note in &archive.period_notes {
            let id = note.local_id()?;
            if !known.insert(id.clone()) {
                continue;
            }
            // Never replace an existing local note, even under a conflicting ID.
            transaction.execute("INSERT INTO local_period_notes(period_id,kind,body,version,created_at_utc_ms,updated_at_utc_ms,archived_at_utc_ms,source_timezone,source_utc_offset_minutes) VALUES(?1,?2,?3,1,?4,?5,?6,?7,?8)", params![id,note.kind,note.body,note.created_at_utc_ms,note.updated_at_utc_ms,note.archived_at_utc_ms,note.source_timezone,note.source_utc_offset_minutes])?;
            report.inserted_notes += 1;
        }
        // Restored/cloned identity histories cannot cause future sequence reuse.
        transaction.execute("UPDATE local_identity SET next_sequence=max(next_sequence,COALESCE((SELECT max(author_sequence)+1 FROM record_revisions WHERE author_device_id=local_identity.device_id),1))", [])?;
        if report.inserted_notes != 0 || report.inserted_revisions != 0 {
            crate::period_notes::bump(&transaction)?;
        }
        report.conflicting_reflections = usize::try_from(transaction.query_row("SELECT count(*) FROM (SELECT h.record_id FROM record_heads h JOIN records r ON r.record_id=h.record_id WHERE r.kind='reflection' GROUP BY h.record_id HAVING count(*)>1)", [], |row| row.get::<_, i64>(0))?).map_err(|_| Error::IntegerRange)?;
        report.conflicting_sessions = usize::try_from(transaction.query_row("SELECT count(*) FROM (SELECT v.record_id FROM record_revisions v JOIN records r ON r.record_id=v.record_id WHERE r.kind='session' GROUP BY v.record_id HAVING count(DISTINCT v.payload_json)>1)", [], |row| row.get::<_, i64>(0))?).map_err(|_| Error::IntegerRange)?;
        let state = crate::period_notes::read_state(&transaction)?;
        transaction.commit()?;
        self.period_note_state = state;
        Ok(report)
    }
}

fn empty_object_archive() -> SyncArchive {
    SyncArchive {
        protocol: 1,
        source_device_id: "logical-record".into(),
        revisions: Vec::new(),
        period_notes: Vec::new(),
    }
}

fn read_archive(connection: &Connection, source_device_id: String) -> Result<SyncArchive> {
    let mut statement =
        connection.prepare("SELECT revision_id FROM record_revisions ORDER BY revision_id")?;
    let ids = statement
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    if ids.len() > MAX_SYNC_ITEMS {
        return Err(invalid("the archive exceeds the 100,000-item limit"));
    }
    let mut revisions = Vec::with_capacity(ids.len());
    let mut bytes = 0_usize;
    for id in ids {
        let revision = read_revision(connection, &id)?
            .ok_or(invalid("a revision disappeared during export"))?;
        bytes = bytes.saturating_add(revision.payload_json.len());
        bytes = bytes.saturating_add(revision.parents.iter().map(String::len).sum::<usize>());
        if bytes > MAX_SYNC_ARCHIVE_BYTES {
            return Err(invalid("the archive exceeds the 64 MiB limit"));
        }
        revisions.push(revision);
    }
    let period_notes = read_period_notes(connection)?;
    let mut archive = SyncArchive {
        protocol: 1,
        source_device_id,
        revisions,
        period_notes,
    };
    archive.select_protocol();
    archive.validate()?;
    Ok(archive)
}

fn read_period_notes(connection: &Connection) -> Result<Vec<SyncPeriodNote>> {
    let mut statement = connection.prepare("SELECT period_id,kind,body,version,created_at_utc_ms,updated_at_utc_ms,archived_at_utc_ms,source_timezone,source_utc_offset_minutes FROM local_period_notes WHERE archived_at_utc_ms IS NOT NULL AND origin_draft_id IS NULL ORDER BY period_id")?;
    let mut rows = statement.query([])?;
    let mut unique = BTreeMap::new();
    let mut bytes = 0_usize;
    while let Some(row) = rows.next()? {
        let note = PeriodNote {
            id: row.get(0)?,
            kind: row.get(1)?,
            body: row.get(2)?,
            version: crate::as_u64(row.get(3)?)?,
            created_at_utc_ms: row.get(4)?,
            updated_at_utc_ms: row.get(5)?,
            archived_at_utc_ms: row.get(6)?,
            source_timezone: row.get(7)?,
            source_utc_offset_minutes: row.get(8)?,
        };
        if note.body.trim().is_empty() {
            continue;
        }
        let note = SyncPeriodNote::from_note(&note)?;
        bytes = bytes.saturating_add(note.body.len());
        if unique.len() >= MAX_SYNC_ITEMS || bytes > MAX_SYNC_ARCHIVE_BYTES {
            return Err(invalid(
                "archived notes exceed the synchronization size limit",
            ));
        }
        unique.insert(note.local_id()?, note);
    }
    Ok(unique.into_values().collect())
}

fn read_revision(connection: &Connection, id: &str) -> Result<Option<SyncRevision>> {
    let revision = connection.query_row(
        "SELECT v.revision_id,v.record_id,r.kind,r.created_at_utc_ms,v.author_device_id,v.author_sequence,v.schema_version,v.created_at_utc_ms,v.payload_json,v.payload_sha256,v.source_timezone,v.source_utc_offset_minutes FROM record_revisions v JOIN records r ON r.record_id=v.record_id WHERE v.revision_id=?1", [id],
        |row| Ok(SyncRevision { revision_id: row.get(0)?, record_id: row.get(1)?, kind: row.get(2)?, record_created_at_utc_ms: row.get(3)?, parents: Vec::new(), author_device_id: row.get(4)?, author_sequence: row_u64(row, 5)?, schema_version: row.get(6)?, created_at_utc_ms: row.get(7)?, payload_json: row.get(8)?, payload_sha256: row.get::<_, Vec<u8>>(9)?.iter().map(|byte| format!("{byte:02x}")).collect(), source_timezone: row.get(10)?, source_utc_offset_minutes: row.get(11)? }),
    ).optional()?;
    revision.map(|mut revision| {
        let mut parents = connection.prepare("SELECT parent_revision_id FROM revision_parents WHERE revision_id=?1 ORDER BY parent_revision_id")?;
        revision.parents = parents.query_map([id], |row| row.get(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
        revision.validate()?;
        Ok(revision)
    }).transpose()
}

fn validate_ancestry(connection: &Connection, revisions: &[SyncRevision]) -> Result<()> {
    let by_id: HashMap<&str, &SyncRevision> = revisions
        .iter()
        .map(|revision| (revision.revision_id.as_str(), revision))
        .collect();
    let mut waiting: HashMap<&str, usize> = HashMap::new();
    let mut children: HashMap<&str, Vec<&str>> = HashMap::new();
    for revision in revisions {
        let mut count = 0;
        for parent in &revision.parents {
            if let Some(incoming) = by_id.get(parent.as_str()) {
                if incoming.record_id != revision.record_id {
                    return Err(invalid("a parent belongs to a different record"));
                }
                children
                    .entry(parent)
                    .or_default()
                    .push(&revision.revision_id);
                count += 1;
            } else {
                let record: Option<String> = connection
                    .query_row(
                        "SELECT record_id FROM record_revisions WHERE revision_id=?1",
                        [parent],
                        |row| row.get(0),
                    )
                    .optional()?;
                if record.as_ref() != Some(&revision.record_id) {
                    return Err(invalid(
                        "a revision parent is missing or belongs to a different record",
                    ));
                }
            }
        }
        waiting.insert(&revision.revision_id, count);
    }
    let mut ready: VecDeque<&str> = waiting
        .iter()
        .filter_map(|(id, count)| (*count == 0).then_some(*id))
        .collect();
    let mut accepted = 0;
    while let Some(parent) = ready.pop_front() {
        accepted += 1;
        for child in children.get(parent).into_iter().flatten() {
            let count = waiting
                .get_mut(child)
                .expect("every child is in the archive");
            *count -= 1;
            if *count == 0 {
                ready.push_back(child);
            }
        }
    }
    if accepted != revisions.len() {
        return Err(invalid("revision ancestry contains a cycle"));
    }
    Ok(())
}

fn rebuild_session(connection: &Connection, id: &str) -> Result<()> {
    let variants: i64 = connection.query_row(
        "SELECT count(DISTINCT payload_json) FROM record_revisions WHERE record_id=?1",
        [id],
        |row| row.get(0),
    )?;
    connection.execute("DELETE FROM terminal_sessions WHERE session_id=?1", [id])?;
    if variants == 1 {
        connection.execute("INSERT INTO terminal_sessions(session_id,revision_id,payload_json) SELECT record_id,revision_id,payload_json FROM record_revisions WHERE record_id=?1 ORDER BY revision_id LIMIT 1", [id])?;
    }
    Ok(())
}

fn invalid(message: &'static str) -> Error {
    Error::InvalidSyncArchive(message)
}

#[derive(Default)]
struct BoundedJson {
    bytes: Vec<u8>,
    too_large: bool,
}

impl std::io::Write for BoundedJson {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if bytes.len() > MAX_SYNC_ARCHIVE_BYTES.saturating_sub(self.bytes.len()) {
            self.too_large = true;
            return Err(std::io::Error::other("synchronization archive size limit"));
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
fn row_u64(row: &rusqlite::Row<'_>, index: usize) -> rusqlite::Result<u64> {
    let value: i64 = row.get(index)?;
    u64::try_from(value).map_err(|_| rusqlite::Error::IntegralValueOutOfRange(index, value))
}
fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn is_digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

#[cfg(test)]
mod tests;
