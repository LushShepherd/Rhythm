use std::{
    path::{Path, PathBuf},
    time::Duration,
};

use rusqlite::{Connection, TransactionBehavior, backup::Backup};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::{Error, Result};

pub(crate) const MIGRATIONS: &[(i64, &str)] = &[
    (1, include_str!("../migrations/001_journal.sql")),
    (2, include_str!("../migrations/002_timer_and_outbox.sql")),
    (3, include_str!("../migrations/003_routines.sql")),
    (4, include_str!("../migrations/004_routine_windows.sql")),
    (5, include_str!("../migrations/005_period_notes.sql")),
    (6, include_str!("../migrations/006_desktop_profile.sql")),
    (7, include_str!("../migrations/007_note_source_time.sql")),
];

pub(crate) fn checksum(sql: &str) -> String {
    format!("{:x}", Sha256::digest(sql.as_bytes()))
}

pub(crate) fn migrate(connection: &mut Connection, path: Option<&Path>) -> Result<Option<PathBuf>> {
    let user_version: i64 =
        connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
    let latest = MIGRATIONS.last().map_or(0, |(version, _)| *version);
    if user_version > latest {
        return Err(Error::NewerSchema {
            found: user_version,
            supported: latest,
        });
    }
    let has_metadata: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='schema_migrations')",
        [], |row| row.get(0),
    )?;
    if !has_metadata {
        let tables: i64 = connection.query_row(
            "SELECT count(*) FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%'",
            [],
            |row| row.get(0),
        )?;
        if tables != 0 || user_version != 0 {
            return Err(Error::UnrecognizedDatabase);
        }
        connection.execute_batch(
            "CREATE TABLE schema_migrations (version INTEGER PRIMARY KEY CHECK(version > 0), checksum TEXT NOT NULL);",
        )?;
    }
    let applied: Vec<(i64, String)> = {
        let mut statement = connection
            .prepare("SELECT version, checksum FROM schema_migrations ORDER BY version")?;
        statement
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<rusqlite::Result<_>>()?
    };
    for (index, (version, stored_checksum)) in applied.iter().enumerate() {
        if *version > latest {
            return Err(Error::NewerSchema {
                found: *version,
                supported: latest,
            });
        }
        let Some((expected_version, sql)) = MIGRATIONS.get(index) else {
            return Err(Error::InvalidMigrationHistory);
        };
        if version != expected_version {
            return Err(Error::InvalidMigrationHistory);
        }
        if stored_checksum != &checksum(sql) {
            return Err(Error::MigrationChecksum { version: *version });
        }
    }
    let last_applied = applied.last().map_or(0, |(version, _)| *version);
    if user_version != last_applied {
        return Err(Error::InvalidMigrationHistory);
    }
    if last_applied == latest {
        return Ok(None);
    }
    // A consistent snapshot includes committed WAL content; copying the main file would not.
    let backup_path = if last_applied > 0 {
        if let Some(path) = path {
            let backup_path =
                path.with_extension(format!("pre-v{latest}-{}.sqlite3", Uuid::new_v4()));
            let mut destination = Connection::open(&backup_path)?;
            Backup::new(connection, &mut destination)?.run_to_completion(
                200,
                Duration::from_millis(5),
                None,
            )?;
            let integrity: String =
                destination.query_row("PRAGMA integrity_check", [], |row| row.get(0))?;
            if integrity != "ok" {
                return Err(Error::IntegrityCheck(integrity));
            }
            Some(backup_path)
        } else {
            None
        }
    } else {
        None
    };
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    for (version, sql) in MIGRATIONS.iter().skip(applied.len()) {
        transaction.execute_batch(sql)?;
        transaction.execute(
            "INSERT INTO schema_migrations(version, checksum) VALUES (?1, ?2)",
            (version, checksum(sql)),
        )?;
    }
    transaction.pragma_update(None, "user_version", latest)?;
    transaction.commit()?;
    Ok(backup_path)
}
