use serde::{Deserialize, Serialize};

/// A personal note owned by one focus or break period. Archiving keeps the body
/// version stable so text composed at a deadline can still be saved to that period.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PeriodNote {
    pub id: String,
    pub kind: String,
    pub body: String,
    pub version: u64,
    pub created_at_utc_ms: i64,
    pub updated_at_utc_ms: i64,
    pub archived_at_utc_ms: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_timezone: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_utc_offset_minutes: Option<i32>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PeriodNoteState {
    pub notes_revision: u64,
    pub period_note: Option<PeriodNote>,
    pub archived_notes: Vec<PeriodNote>,
}

/// A bounded, stable page over archived period notes and the earlier journal.
/// Each preserved reflection head is a distinct entry; drafts retain their text.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryNote {
    pub id: String,
    pub source: String,
    pub kind: String,
    pub body: String,
    pub version: u64,
    pub created_at_utc_ms: i64,
    pub updated_at_utc_ms: i64,
    pub archived_at_utc_ms: Option<i64>,
    pub record_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_timezone: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_utc_offset_minutes: Option<i32>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NoteHistoryCursor {
    pub revision: String,
    pub at_utc_ms: i64,
    pub key: String,
    pub periods_only: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NoteHistoryPage {
    pub revision: String,
    pub entries: Vec<HistoryNote>,
    pub next_cursor: Option<NoteHistoryCursor>,
    /// The history changed between pages. Replace previously displayed entries.
    pub restarted: bool,
}

/// Journal text is stored exactly as written; it is never normalized or trimmed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReflectionKind {
    Thought,
    Intention,
    FutureSelf,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReflectionInput {
    pub kind: ReflectionKind,
    pub body: String,
    /// A logical session identifier; a draft can precede terminal session history.
    pub session_id: Option<String>,
    pub reveal_at_utc_ms: Option<i64>,
}

impl ReflectionInput {
    pub fn thought(body: impl Into<String>) -> Self {
        Self {
            kind: ReflectionKind::Thought,
            body: body.into(),
            session_id: None,
            reveal_at_utc_ms: None,
        }
    }
}

/// A device-local autosaved draft. Its version protects competing editor windows.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Draft {
    pub id: String,
    pub record_id: Option<String>,
    pub base_revision_ids: Vec<String>,
    pub content: ReflectionInput,
    pub version: u64,
    pub created_at_utc_ms: i64,
    pub updated_at_utc_ms: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_timezone: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_utc_offset_minutes: Option<i32>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReflectionRevision {
    pub id: String,
    pub record_id: String,
    pub parents: Vec<String>,
    pub content: ReflectionInput,
    pub author_device_id: String,
    pub author_sequence: u64,
    pub created_at_utc_ms: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_timezone: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_utc_offset_minutes: Option<i32>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reflection {
    pub id: String,
    /// Multiple heads are preserved variants, not duplicate journal entries.
    pub heads: Vec<ReflectionRevision>,
    pub created_at_utc_ms: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutboxItem {
    pub provider_binding_id: String,
    pub revision_id: String,
    pub remote_upload_identity: Option<String>,
}

/// Serialized local checkpoint. A storage generation is independent of timer time.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoredCheckpoint {
    pub generation: u64,
    pub json: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoredSession {
    pub id: String,
    pub json: String,
}
