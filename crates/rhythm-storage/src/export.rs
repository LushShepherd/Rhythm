//! A portable, one-way journal export. No provider or credential data is included.

use std::{fmt::Write as _, fs::OpenOptions, io::Write as _, path::Path};

use crate::{Error, ReflectionInput, Result, Store};

impl Store {
    /// Export every reflection revision and unfinished draft from one read snapshot.
    /// Existing files are never replaced. Text is fenced, not interpreted as Markdown;
    /// `body_utf8_bytes` allows exact recovery even when the text has no final newline.
    /// This is a journal export, not a restorable database or a session statistics report.
    pub fn export_markdown_to(&self, path: impl AsRef<Path>) -> Result<()> {
        let document = self.journal_markdown()?;
        let path = path.as_ref();
        let mut file = OpenOptions::new().write(true).create_new(true).open(path)?;
        let result = file
            .write_all(document.as_bytes())
            .and_then(|()| file.sync_all());
        drop(file);
        if let Err(error) = result {
            // Only this invocation's newly created, incomplete file is removed.
            let _ = std::fs::remove_file(path);
            return Err(Error::Io(error));
        }
        Ok(())
    }

    fn journal_markdown(&self) -> Result<String> {
        let transaction = self.connection.unchecked_transaction()?;
        let ids = {
            let mut query = transaction.prepare(
                "SELECT record_id FROM records WHERE kind='reflection' ORDER BY created_at_utc_ms,record_id",
            )?;
            query
                .query_map([], |row| row.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?
        };
        let drafts = self.list_drafts()?;
        let period_notes = self.all_period_notes()?;
        let mut out = String::from(
            "# Rhythm journal\n\nExport format: 1. All dates are UTC.\n\nThis one-way export includes every saved version, unresolved variant, and unfinished draft. It does not include timer history, settings, or account information. Use a Rhythm backup to restore the application.\n\nWords are preserved literally inside text fences, including Markdown and whitespace. The metadata records the exact UTF-8 byte length; a newline before a closing fence is formatting when it lies beyond that length.\n\n",
        );
        writeln!(
            out,
            "{} letters; {} unfinished drafts.\n",
            ids.len(),
            drafts.len()
        )
        .unwrap();
        for (index, id) in ids.iter().enumerate() {
            let reflection = self
                .get_reflection(id)?
                .ok_or_else(|| Error::ReflectionNotFound(id.clone()))?;
            writeln!(
                out,
                "## Letter {}\n\nCreated: {}\n",
                index + 1,
                utc_label(reflection.created_at_utc_ms)
            )
            .unwrap();
            let revisions = self.revision_history(id)?;
            for revision in revisions {
                let current = reflection.heads.iter().any(|head| head.id == revision.id);
                let label = match (current, reflection.heads.len() > 1) {
                    (true, true) => "Unresolved current variant",
                    (true, false) => "Current version",
                    (false, _) => "Previous version",
                };
                writeln!(
                    out,
                    "### {label}\n\nWritten: {}\n",
                    utc_label(revision.created_at_utc_ms)
                )
                .unwrap();
                let metadata = serde_json::json!({
                    "record_id": revision.record_id,
                    "revision_id": revision.id,
                    "parent_revision_ids": revision.parents,
                    "created_at_utc_ms": revision.created_at_utc_ms,
                    "source_timezone": revision.source_timezone,
                    "source_utc_offset_minutes": revision.source_utc_offset_minutes,
                    "record_created_at_utc_ms": reflection.created_at_utc_ms,
                    "status": label,
                    "kind": revision.content.kind,
                    "session_id": revision.content.session_id,
                    "reveal_at_utc_ms": revision.content.reveal_at_utc_ms,
                    "body_utf8_bytes": revision.content.body.len(),
                });
                fenced(&mut out, "json", &serde_json::to_string_pretty(&metadata)?);
                content(&mut out, &revision.content);
            }
        }
        out.push_str("## Unfinished drafts\n\n");
        if drafts.is_empty() {
            out.push_str("No unfinished drafts.\n\n");
        }
        for (index, draft) in drafts.iter().enumerate() {
            writeln!(
                out,
                "### Draft {}\n\nUpdated: {}\n",
                index + 1,
                utc_label(draft.updated_at_utc_ms)
            )
            .unwrap();
            let metadata = serde_json::json!({
                "draft_id": draft.id,
                "record_id": draft.record_id,
                "base_revision_ids": draft.base_revision_ids,
                "draft_version": draft.version,
                "created_at_utc_ms": draft.created_at_utc_ms,
                "source_timezone": draft.source_timezone,
                "source_utc_offset_minutes": draft.source_utc_offset_minutes,
                "updated_at_utc_ms": draft.updated_at_utc_ms,
                "kind": draft.content.kind,
                "session_id": draft.content.session_id,
                "reveal_at_utc_ms": draft.content.reveal_at_utc_ms,
                "body_utf8_bytes": draft.content.body.len(),
            });
            fenced(&mut out, "json", &serde_json::to_string_pretty(&metadata)?);
            content(&mut out, &draft.content);
        }
        out.push_str("## Period notes\n\n");
        for note in period_notes
            .iter()
            .filter(|note| !note.body.trim().is_empty())
        {
            writeln!(
                out,
                "### {} · {}\n",
                if note.kind == "focus" {
                    "Focus"
                } else {
                    "Break"
                },
                utc_label(note.created_at_utc_ms)
            )
            .unwrap();
            let metadata = serde_json::json!({
                "period_id": note.id, "kind": note.kind, "version": note.version,
                "created_at_utc_ms": note.created_at_utc_ms, "updated_at_utc_ms": note.updated_at_utc_ms,
                "source_timezone": note.source_timezone, "source_utc_offset_minutes": note.source_utc_offset_minutes,
                "archived_at_utc_ms": note.archived_at_utc_ms, "body_utf8_bytes": note.body.len(),
            });
            fenced(&mut out, "json", &serde_json::to_string_pretty(&metadata)?);
            fenced(&mut out, "text", &note.body);
        }
        transaction.commit()?;
        Ok(out)
    }
}

fn content(out: &mut String, content: &ReflectionInput) {
    if let Some(reveal) = content.reveal_at_utc_ms {
        writeln!(out, "Reveal date: {}\n", utc_label(reveal)).unwrap();
    }
    fenced(out, "text", &content.body);
}

fn fenced(out: &mut String, language: &str, text: &str) {
    let longest = text.split(|c| c != '`').map(str::len).max().unwrap_or(0);
    let fence = "`".repeat((longest + 1).max(3));
    writeln!(out, "{fence}{language}").unwrap();
    out.push_str(text);
    if !text.ends_with('\n') {
        out.push('\n');
    }
    writeln!(out, "{fence}\n").unwrap();
}

// Gregorian civil-date conversion from days since 1970-01-01. Euclidean division
// handles pre-epoch dates; i64 intermediates cover every stored millisecond value.
fn utc_label(milliseconds: i64) -> String {
    let days = milliseconds.div_euclid(86_400_000) + 719_468;
    let era = days.div_euclid(146_097);
    let day_of_era = days - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36524 - day_of_era / 146096) / 365;
    let mut year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = month_prime + if month_prime < 10 { 3 } else { -9 };
    year += i64::from(month <= 2);
    let time = milliseconds.rem_euclid(86_400_000);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{:03}Z",
        time / 3_600_000,
        time / 60_000 % 60,
        time / 1000 % 60,
        time % 1000
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ReflectionKind;

    #[test]
    fn export_includes_all_pages_history_variants_and_exact_draft_words() {
        let mut store = Store::open_in_memory().unwrap();
        for i in 0..105 {
            let draft = store
                .create_draft(ReflectionInput::thought(format!("page marker {i}")), i)
                .unwrap();
            store.finalize_draft(&draft.id, draft.version, i).unwrap();
        }
        let original = store
            .create_draft(ReflectionInput::thought("original words"), 200)
            .unwrap();
        let original = store
            .finalize_draft(&original.id, original.version, 200)
            .unwrap();
        let a = store.edit_reflection(&original.record_id, 201).unwrap();
        let b = store.edit_reflection(&original.record_id, 201).unwrap();
        let a = store
            .save_draft(&a.id, a.version, "variant A", 202)
            .unwrap();
        let b = store
            .save_draft(&b.id, b.version, "variant B", 202)
            .unwrap();
        store.finalize_draft(&a.id, a.version, 203).unwrap();
        store
            .finalize_draft_as_variant(&b.id, b.version, 203)
            .unwrap();
        let body = "  明天慢慢来。\r\n給自己空間 🌱\n`````\n# heading\n<script>alert('x')</script>\nno final newline  ";
        store
            .create_draft(
                ReflectionInput {
                    kind: ReflectionKind::FutureSelf,
                    body: body.into(),
                    session_id: Some("session-reference".into()),
                    reveal_at_utc_ms: Some(951_782_400_000),
                },
                204,
            )
            .unwrap();
        store
            .register_provider_binding("secret-provider-binding")
            .unwrap();
        let out = store.journal_markdown().unwrap();
        assert!(out.contains("106 letters; 1 unfinished drafts."));
        for i in 0..105 {
            assert!(out.contains(&format!("page marker {i}\n")));
        }
        assert!(out.contains("original words"));
        assert!(out.contains("variant A"));
        assert!(out.contains("variant B"));
        assert_eq!(out.matches("### Unresolved current variant").count(), 2);
        assert!(out.contains("### Previous version"));
        assert!(out.contains(&format!("``````text\n{body}\n``````")));
        assert!(out.contains(&format!("\"body_utf8_bytes\": {}", body.len())));
        assert!(out.contains("2000-02-29T00:00:00.000Z"));
        assert!(!out.contains("secret-provider-binding"));
        assert!(!out.contains(store.device_id()));
    }

    #[test]
    fn export_never_overwrites_existing_files_or_the_database() {
        let directory = tempfile::tempdir().unwrap();
        let database = directory.path().join("rhythm.sqlite3");
        let mut store = Store::open(&database).unwrap();
        store
            .create_draft(ReflectionInput::thought("safe words"), 0)
            .unwrap();
        let target = directory.path().join("journal.md");
        store.export_markdown_to(&target).unwrap();
        let original = std::fs::read(&target).unwrap();
        assert!(
            matches!(store.export_markdown_to(&target), Err(Error::Io(error)) if error.kind() == std::io::ErrorKind::AlreadyExists)
        );
        assert_eq!(std::fs::read(&target).unwrap(), original);
        assert!(store.export_markdown_to(&database).is_err());
        assert_eq!(store.list_drafts().unwrap()[0].content.body, "safe words");
        assert!(
            store
                .export_markdown_to(directory.path().join("missing").join("journal.md"))
                .is_err()
        );
    }

    #[test]
    fn utc_dates_cover_epoch_leap_days_and_negative_values() {
        assert_eq!(utc_label(0), "1970-01-01T00:00:00.000Z");
        assert_eq!(utc_label(-1), "1969-12-31T23:59:59.999Z");
        assert_eq!(utc_label(951_782_400_000), "2000-02-29T00:00:00.000Z");
        assert_eq!(utc_label(4_107_542_400_000), "2100-03-01T00:00:00.000Z");
        assert!(!utc_label(i64::MIN).is_empty());
        assert!(!utc_label(i64::MAX).is_empty());
    }

    #[test]
    fn empty_and_trailing_newline_text_is_preserved() {
        for text in ["", "\n", "word", "word\n", "word\r\n", "```\n````"] {
            let mut out = String::new();
            fenced(&mut out, "text", text);
            let start = out.find('\n').unwrap() + 1;
            assert_eq!(&out[start..start + text.len()], text);
        }
    }

    #[test]
    fn corrupt_revision_fails_before_creating_an_export_and_releases_snapshot() {
        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().join("journal.md");
        let mut store = Store::open_in_memory().unwrap();
        let draft = store
            .create_draft(ReflectionInput::thought("original"), 0)
            .unwrap();
        store.finalize_draft(&draft.id, draft.version, 1).unwrap();
        store.connection.execute_batch(
            "DROP TRIGGER immutable_revisions_update; UPDATE record_revisions SET payload_sha256=zeroblob(32)",
        ).unwrap();
        assert!(matches!(
            store.export_markdown_to(&target),
            Err(Error::RevisionIntegrity(_))
        ));
        assert!(!target.exists());
        // A failed read must not strand the worker inside a transaction.
        assert!(store.connection.is_autocommit());
        store
            .create_draft(ReflectionInput::thought("still writable"), 2)
            .unwrap();
    }
}
