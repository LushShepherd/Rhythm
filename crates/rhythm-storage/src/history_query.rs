//! Read-only search across the complete journal. Bodies are evaluated inside
//! SQLite and only a bounded result page is materialized into HistoryNote values.
use chrono::{DateTime, Duration, NaiveDate, Timelike, Utc};
use chrono_tz::Tz;
use rusqlite::{functions::FunctionFlags, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{Error, HistoryNote, Result, Store, period_notes};

const MAX_QUERY_BYTES: usize = 1024;
const MAX_WORDS: usize = 32;
const METADATA: &str = "
    SELECT 'period:' || period_id AS history_key, period_id AS id,
        'period' AS source, created_at_utc_ms AS written_at_utc_ms, kind,
        source_utc_offset_minutes, body
    FROM local_period_notes WHERE archived_at_utc_ms IS NOT NULL
    UNION ALL
    SELECT 'reflection:' || r.revision_id, r.revision_id,
        CASE WHEN EXISTS(SELECT 1 FROM record_heads h WHERE h.revision_id=r.revision_id)
            THEN 'reflection' ELSE 'reflection_version' END,
        r.created_at_utc_ms, json_extract(r.payload_json,'$.kind'),
        r.source_utc_offset_minutes, json_extract(r.payload_json,'$.body')
    FROM record_revisions r JOIN records rec ON rec.record_id=r.record_id
    WHERE rec.kind='reflection'
    UNION ALL
    SELECT 'draft:' || d.draft_id, d.draft_id, 'draft', d.created_at_utc_ms,
        json_extract(d.payload_json,'$.kind'), d.source_utc_offset_minutes,
        json_extract(d.payload_json,'$.body')
    FROM reflection_drafts d
    WHERE NOT EXISTS(
        SELECT 1 FROM local_period_notes p WHERE p.origin_draft_id=d.draft_id
        AND p.archived_at_utc_ms IS NOT NULL
        AND p.body=json_extract(d.payload_json,'$.body'))";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct NoteHistoryQuery {
    pub text: String,
    pub date_from: Option<String>,
    pub date_to: Option<String>,
    pub time_of_day: String,
    pub kind: String,
    pub source: String,
    pub order: String,
    pub time_basis: String,
    pub current_timezone: String,
}

impl Default for NoteHistoryQuery {
    fn default() -> Self {
        Self {
            text: String::new(),
            date_from: None,
            date_to: None,
            time_of_day: "all".into(),
            kind: "all".into(),
            source: "all".into(),
            order: "newest".into(),
            time_basis: "current".into(),
            current_timezone: "UTC".into(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NoteHistoryQueryCursor {
    pub revision: String,
    pub query_hash: String,
    pub at_utc_ms: i64,
    pub key: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NoteHistoryQueryPage {
    pub revision: String,
    pub entries: Vec<HistoryNote>,
    pub next_cursor: Option<NoteHistoryQueryCursor>,
    pub restarted: bool,
}

struct ValidatedQuery {
    normalized: NoteHistoryQuery,
    words: Vec<String>,
    date_from: Option<NaiveDate>,
    date_to: Option<NaiveDate>,
    timezone: Tz,
    hash: String,
}

fn invalid(message: &'static str) -> Error {
    Error::InvalidNoteArchive(message)
}

fn parse_date(value: Option<&str>) -> Result<Option<NaiveDate>> {
    let Some(value) = value.filter(|value| !value.is_empty()) else {
        return Ok(None);
    };
    if value.len() != 10
        || value.as_bytes()[4] != b'-'
        || value.as_bytes()[7] != b'-'
        || !value
            .bytes()
            .enumerate()
            .all(|(index, byte)| index == 4 || index == 7 || byte.is_ascii_digit())
    {
        return Err(invalid("search dates must use YYYY-MM-DD"));
    }
    let date = NaiveDate::parse_from_str(value, "%Y-%m-%d")
        .map_err(|_| invalid("the note search date is invalid"))?;
    if value.starts_with("0000-") {
        return Err(invalid("search dates must be between years 0001 and 9999"));
    }
    Ok(Some(date))
}

impl NoteHistoryQuery {
    fn validate(&self) -> Result<ValidatedQuery> {
        if self.text.len() > MAX_QUERY_BYTES {
            return Err(invalid("note search text is limited to 1,024 UTF-8 bytes"));
        }
        let words: Vec<_> = self
            .text
            .split_whitespace()
            .map(str::to_lowercase)
            .collect();
        if words.len() > MAX_WORDS {
            return Err(invalid("note search supports up to 32 words"));
        }
        if !["all", "morning", "noon", "afternoon", "evening", "night"]
            .contains(&self.time_of_day.as_str())
            || ![
                "all",
                "focus",
                "break",
                "thought",
                "intention",
                "future_self",
            ]
            .contains(&self.kind.as_str())
            || !["all", "period", "reflection", "reflection_version", "draft"]
                .contains(&self.source.as_str())
            || !["newest", "oldest"].contains(&self.order.as_str())
            || !["current", "original"].contains(&self.time_basis.as_str())
        {
            return Err(invalid("the note search filter is unsupported"));
        }
        if self.current_timezone.len() > 128 {
            return Err(invalid("the display timezone is invalid"));
        }
        let timezone = self
            .current_timezone
            .parse::<Tz>()
            .map_err(|_| invalid("the display timezone is invalid"))?;
        let date_from = parse_date(self.date_from.as_deref())?;
        let date_to = parse_date(self.date_to.as_deref())?;
        if date_from.zip(date_to).is_some_and(|(from, to)| from > to) {
            return Err(invalid("the start date must not follow the end date"));
        }
        let mut normalized = self.clone();
        normalized.text = words.join(" ");
        normalized.date_from = date_from.map(|date| date.format("%Y-%m-%d").to_string());
        normalized.date_to = date_to.map(|date| date.format("%Y-%m-%d").to_string());
        let hash = format!("{:x}", Sha256::digest(serde_json::to_vec(&normalized)?));
        Ok(ValidatedQuery {
            normalized,
            words,
            date_from,
            date_to,
            timezone,
            hash,
        })
    }
}

fn time_of_day(hour: u32) -> &'static str {
    match hour {
        5..=11 => "morning",
        12..=13 => "noon",
        14..=17 => "afternoon",
        18..=21 => "evening",
        _ => "night",
    }
}

impl Store {
    /// Search all notes using their creation/writing instant. UTC determines
    /// order; only the requested calendar/date filters use a display timezone.
    /// Original time uses the recorded fixed offset, including DST at creation.
    /// Legacy unknown origins remain visible without a calendar filter and are
    /// excluded from original-time date/day filters rather than guessed.
    pub fn query_note_history(
        &self,
        query: &NoteHistoryQuery,
        cursor: Option<&NoteHistoryQueryCursor>,
        limit: u32,
    ) -> Result<NoteHistoryQueryPage> {
        if !(1..=100).contains(&limit) {
            return Err(invalid("history pages contain 1 to 100 notes"));
        }
        if let Some(cursor) = cursor {
            let valid_hash =
                |hash: &str| hash.len() == 64 && hash.bytes().all(|byte| byte.is_ascii_hexdigit());
            if cursor.key.is_empty()
                || cursor.key.len() > 512
                || !valid_hash(&cursor.revision)
                || !valid_hash(&cursor.query_hash)
            {
                return Err(invalid("the note search cursor is invalid"));
            }
        }
        let validated = query.validate()?;
        let words = validated.words;
        self.connection.create_scalar_function(
            "rhythm_history_words",
            1,
            FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_DETERMINISTIC,
            move |context| {
                let body: String = context.get(0)?;
                if body.trim().is_empty() {
                    return Ok(false);
                }
                if words.is_empty() {
                    return Ok(true);
                }
                let lower = body.to_lowercase();
                Ok(words.iter().all(|word| lower.contains(word)))
            },
        )?;
        let original = validated.normalized.time_basis == "original";
        let date_from = validated.date_from;
        let date_to = validated.date_to;
        let period = validated.normalized.time_of_day.clone();
        let timezone = validated.timezone;
        self.connection.create_scalar_function(
            "rhythm_history_time",
            2,
            FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_DETERMINISTIC,
            move |context| {
                if date_from.is_none() && date_to.is_none() && period == "all" {
                    return Ok(true);
                }
                let instant: i64 = context.get(0)?;
                let offset: Option<i32> = context.get(1)?;
                let Some(utc) = DateTime::<Utc>::from_timestamp_millis(instant) else {
                    return Ok(false);
                };
                let local = if original {
                    let Some(offset) = offset.filter(|offset| (-1440..=1440).contains(offset))
                    else {
                        return Ok(false);
                    };
                    let Some(local) = utc
                        .naive_utc()
                        .checked_add_signed(Duration::minutes(i64::from(offset)))
                    else {
                        return Ok(false);
                    };
                    local
                } else {
                    utc.with_timezone(&timezone).naive_local()
                };
                Ok(date_from.is_none_or(|from| local.date() >= from)
                    && date_to.is_none_or(|to| local.date() <= to)
                    && (period == "all" || time_of_day(local.hour()) == period))
            },
        )?;
        let transaction = self.connection.unchecked_transaction()?;
        let revision = period_notes::history_revision(&transaction, false)?;
        let restarted = cursor.is_some_and(|cursor| {
            cursor.revision != revision || cursor.query_hash != validated.hash
        });
        let cursor = cursor.filter(|_| !restarted);
        let (ordering, comparison) = if validated.normalized.order == "oldest" {
            ("ASC", ">")
        } else {
            ("DESC", "<")
        };
        // These two SQL fragments are fixed allowlisted constants, never user text.
        let mut statement = transaction.prepare(&format!(
            "SELECT history_key,id,source,written_at_utc_ms FROM ({METADATA})
             WHERE (?1='all' OR kind=?1) AND (?2='all' OR source=?2)
             AND rhythm_history_words(body) AND rhythm_history_time(written_at_utc_ms,source_utc_offset_minutes)
             AND (?3 IS NULL OR written_at_utc_ms {comparison} ?3 OR (written_at_utc_ms=?3 AND history_key>?4))
             ORDER BY written_at_utc_ms {ordering},history_key LIMIT ?5"
        ))?;
        let mut rows = statement.query(params![
            validated.normalized.kind,
            validated.normalized.source,
            cursor.map(|cursor| cursor.at_utc_ms),
            cursor.map(|cursor| cursor.key.as_str()),
            limit + 1
        ])?;
        let mut entries = Vec::new();
        let mut last = None;
        let mut more = false;
        while let Some(row) = rows.next()? {
            if entries.len() == limit as usize {
                more = true;
                break;
            }
            let key: String = row.get(0)?;
            let id: String = row.get(1)?;
            let source: String = row.get(2)?;
            let at_utc_ms: i64 = row.get(3)?;
            entries.push(period_notes::history_entry(&transaction, &id, &source)?);
            last = Some(NoteHistoryQueryCursor {
                revision: revision.clone(),
                query_hash: validated.hash.clone(),
                at_utc_ms,
                key,
            });
        }
        drop(rows);
        drop(statement);
        transaction.commit()?;
        Ok(NoteHistoryQueryPage {
            revision,
            entries,
            next_cursor: if more { last } else { None },
            restarted,
        })
    }
}

#[cfg(test)]
#[path = "history_query_tests.rs"]
mod tests;
