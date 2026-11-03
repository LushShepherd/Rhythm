//! Device-local routine definitions and durable at-most-once invitation delivery.
use crate::{Error, Result, Store};
use rhythm_domain::{Invitation, MAX_ROUTINES, Routine, RoutineInput};
use rusqlite::{Connection, TransactionBehavior, params};
use uuid::Uuid;

impl Store {
    pub fn list_routines(&self) -> Result<Vec<Routine>> {
        read_routines(&self.connection)
    }

    /// Preview the next undelivered window. A routine can have several starts on
    /// one date; changing only its end must not replay an already claimed start.
    pub fn next_routine_invitation(
        &self,
        routine: &Routine,
        now_utc_ms: i64,
        system_zone: &str,
    ) -> Result<Option<Invitation>> {
        let mut query = self.connection.prepare(
            "SELECT local_date,window_key FROM local_routine_deliveries WHERE routine_id=?1",
        )?;
        let delivered = query
            .query_map([&routine.id], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?
            .collect::<rusqlite::Result<std::collections::HashSet<_>>>()?;
        let mut after = now_utc_ms;
        for _ in 0..=delivered.len() {
            let Some(next) = routine.next_invitation(after, system_zone)? else {
                return Ok(None);
            };
            if !delivered.contains(&(next.local_date.clone(), next.window_key.clone())) {
                return Ok(Some(next));
            }
            after = next.due_at_utc_ms;
        }
        Ok(None)
    }

    pub fn save_routine(&mut self, id: Option<String>, input: RoutineInput) -> Result<Routine> {
        input.validate()?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let id = match id {
            Some(id) => {
                crate::validate_id(&id)?;
                let changed = transaction.execute(
                    "UPDATE local_routines SET input_json=?1 WHERE routine_id=?2",
                    params![serde_json::to_string(&input)?, id],
                )?;
                if changed == 0 {
                    return Err(Error::RoutineNotFound(id));
                }
                id
            }
            None => {
                let count: i64 =
                    transaction
                        .query_row("SELECT count(*) FROM local_routines", [], |row| row.get(0))?;
                if count >= MAX_ROUTINES as i64 {
                    return Err(Error::TooManyRoutines);
                }
                let id = Uuid::new_v4().to_string();
                transaction.execute(
                    "INSERT INTO local_routines(routine_id,input_json) VALUES(?1,?2)",
                    params![id, serde_json::to_string(&input)?],
                )?;
                id
            }
        };
        transaction.commit()?;
        Ok(Routine { id, input })
    }

    pub fn delete_routine(&mut self, id: &str) -> Result<()> {
        crate::validate_id(id)?;
        if self
            .connection
            .execute("DELETE FROM local_routines WHERE routine_id=?1", [id])?
            == 0
        {
            return Err(Error::RoutineNotFound(id.into()));
        }
        Ok(())
    }

    /// Claim currently fresh invitations in the same transaction that reads their
    /// definitions. Repeated polls, restarts, clock rollback and the second instance
    /// of a repeated DST time cannot deliver the same routine/date/window again.
    /// Callers publish reminders only after this transaction commits.
    pub fn take_due_invitations(
        &mut self,
        now_utc_ms: i64,
        system_zone: &str,
    ) -> Result<Vec<Invitation>> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let routines = read_routines(&transaction)?;
        let mut invitations = Vec::new();
        for routine in routines {
            if let Some(invitation) = routine.due_invitation(now_utc_ms, system_zone)? {
                let inserted = transaction.execute("INSERT OR IGNORE INTO local_routine_deliveries(routine_id,local_date,window_key,due_at_utc_ms) VALUES(?1,?2,?3,?4)", params![invitation.routine_id, invitation.local_date, invitation.window_key, invitation.due_at_utc_ms])?;
                if inserted == 1 {
                    invitations.push(invitation);
                }
            }
        }
        invitations.sort_by(|a, b| {
            a.due_at_utc_ms
                .cmp(&b.due_at_utc_ms)
                .then_with(|| a.routine_id.cmp(&b.routine_id))
        });
        transaction.commit()?;
        Ok(invitations)
    }

    /// Current, undismissed custom windows remain available after accepting a
    /// reminder or restarting. Reading these never claims or repeats a notification.
    pub fn active_routine_windows(
        &self,
        now_utc_ms: i64,
        system_zone: &str,
    ) -> Result<Vec<Invitation>> {
        let mut active = Vec::new();
        for routine in read_routines(&self.connection)? {
            if let Some(invitation) = routine.due_invitation(now_utc_ms, system_zone)?
                && invitation.window.is_some()
            {
                let dismissed: bool = self.connection.query_row(
                    "SELECT EXISTS(SELECT 1 FROM local_routine_deliveries WHERE routine_id=?1 AND local_date=?2 AND window_key=?3 AND dismissed=1)",
                    params![invitation.routine_id, invitation.local_date, invitation.window_key],
                    |row| row.get(0),
                )?;
                if !dismissed {
                    active.push(invitation);
                }
            }
        }
        active.sort_by(|a, b| {
            a.due_at_utc_ms
                .cmp(&b.due_at_utc_ms)
                .then_with(|| a.routine_id.cmp(&b.routine_id))
        });
        Ok(active)
    }

    /// Dismiss only this window on this local date, preserving later windows.
    /// Upserting also claims it, so a dismissed window cannot notify afterward.
    pub fn dismiss_routine_window(&mut self, invitation: &Invitation) -> Result<()> {
        self.connection.execute(
            "INSERT INTO local_routine_deliveries(routine_id,local_date,window_key,due_at_utc_ms,dismissed) VALUES(?1,?2,?3,?4,1) ON CONFLICT(routine_id,local_date,window_key) DO UPDATE SET dismissed=1",
            params![invitation.routine_id, invitation.local_date, invitation.window_key, invitation.due_at_utc_ms],
        )?;
        Ok(())
    }
}

fn read_routines(connection: &Connection) -> Result<Vec<Routine>> {
    let mut statement = connection
        .prepare("SELECT routine_id,input_json FROM local_routines ORDER BY routine_id")?;
    let rows = statement.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    let mut routines = Vec::new();
    for row in rows {
        let (id, json) = row?;
        let routine = Routine {
            id,
            input: serde_json::from_str(&json)?,
        };
        routine.validate()?;
        routines.push(routine);
    }
    if routines.len() > MAX_ROUTINES {
        return Err(Error::TooManyRoutines);
    }
    Ok(routines)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn input() -> RoutineInput {
        RoutineInput {
            name: "Morning".into(),
            weekdays: vec![1, 2, 3, 4, 5],
            local_time: Some("09:00".into()),
            windows: vec![],
            timezone: "UTC".into(),
            focus_ms: 1_500_000,
            enabled: true,
        }
    }
    // Monday November 2, 2026, 09:00 UTC.
    const MONDAY: i64 = 1_793_610_000_000;
    #[test]
    fn definitions_and_once_per_date_delivery_survive_reopening() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("routines.sqlite3");
        let mut store = Store::open(&path).unwrap();
        let routine = store.save_routine(None, input()).unwrap();
        assert_eq!(store.list_routines().unwrap(), vec![routine.clone()]);
        assert!(
            store
                .take_due_invitations(MONDAY - 1, "UTC")
                .unwrap()
                .is_empty()
        );
        let due = store.take_due_invitations(MONDAY, "UTC").unwrap();
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].local_date, "2026-11-02");
        drop(store);
        let mut store = Store::open(&path).unwrap();
        assert!(
            store
                .take_due_invitations(MONDAY + 1000, "UTC")
                .unwrap()
                .is_empty()
        );
        assert!(
            store
                .take_due_invitations(MONDAY, "UTC")
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            store
                .take_due_invitations(MONDAY + 86_400_000, "UTC")
                .unwrap()
                .len(),
            1
        );
        let mut edited = routine.input.clone();
        edited.name = "Renamed".into();
        store
            .save_routine(Some(routine.id.clone()), edited)
            .unwrap();
        assert!(
            store
                .take_due_invitations(MONDAY + 86_400_000, "UTC")
                .unwrap()
                .is_empty()
        );
        store.delete_routine(&routine.id).unwrap();
        assert!(store.list_routines().unwrap().is_empty());
    }
    #[test]
    fn simultaneous_invitations_are_returned_together_and_missed_ones_are_not_replayed() {
        let mut store = Store::open_in_memory().unwrap();
        store.save_routine(None, input()).unwrap();
        store.save_routine(None, input()).unwrap();
        assert!(
            store
                .take_due_invitations(MONDAY + 900_001, "UTC")
                .unwrap()
                .is_empty()
        );
        let due = store
            .take_due_invitations(MONDAY + 86_400_000, "UTC")
            .unwrap();
        assert_eq!(due.len(), 2);
        assert!(store.load_timer().unwrap().is_none());
    }
    #[test]
    fn invalid_edit_preserves_definition_and_unknown_id_cannot_create() {
        let mut store = Store::open_in_memory().unwrap();
        let routine = store.save_routine(None, input()).unwrap();
        let mut invalid = input();
        invalid.weekdays = vec![];
        assert!(
            store
                .save_routine(Some(routine.id.clone()), invalid)
                .is_err()
        );
        assert!(matches!(
            store.save_routine(Some("missing".into()), input()),
            Err(Error::RoutineNotFound(_))
        ));
        assert_eq!(store.list_routines().unwrap(), vec![routine]);
    }
    #[test]
    fn next_preview_skips_delivered_date_after_edit_or_clock_rollback() {
        let mut store = Store::open_in_memory().unwrap();
        let routine = store.save_routine(None, input()).unwrap();
        store.take_due_invitations(MONDAY, "UTC").unwrap();
        let mut candidate = routine;
        candidate.input.local_time = Some("10:00".into());
        let next = store
            .next_routine_invitation(&candidate, MONDAY + 1, "UTC")
            .unwrap()
            .unwrap();
        assert_eq!(next.local_date, "2026-11-03");
        assert_eq!(next.due_at_utc_ms, MONDAY + 86_400_000 + 3_600_000);
        assert_eq!(
            store
                .next_routine_invitation(&candidate, MONDAY - 3_600_000, "UTC")
                .unwrap()
                .unwrap(),
            next
        );
    }
    #[test]
    fn backup_restore_preserves_routines_and_delivery_history() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("backup.sqlite3");
        let mut source = Store::open_in_memory().unwrap();
        let routine = source.save_routine(None, input()).unwrap();
        assert_eq!(source.take_due_invitations(MONDAY, "UTC").unwrap().len(), 1);
        source.backup_to(&path).unwrap();
        let mut destination = Store::open_in_memory().unwrap();
        destination.restore_from(&path).unwrap();
        assert_eq!(destination.list_routines().unwrap(), vec![routine]);
        assert!(
            destination
                .take_due_invitations(MONDAY, "UTC")
                .unwrap()
                .is_empty()
        );
    }
    #[test]
    fn failed_evaluation_does_not_consume_other_invitations() {
        let mut store = Store::open_in_memory().unwrap();
        store.save_routine(None, input()).unwrap();
        let mut system = input();
        system.timezone = "system".into();
        store.save_routine(None, system).unwrap();
        assert!(store.take_due_invitations(MONDAY, "missing-zone").is_err());
        assert_eq!(store.take_due_invitations(MONDAY, "UTC").unwrap().len(), 2);
    }
    #[test]
    fn version_two_upgrade_preserves_journal_and_makes_pre_migration_backup() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("version-two.sqlite3");
        let mut source = Store::open_in_memory().unwrap();
        let mut draft = source
            .create_draft(
                crate::ReflectionInput::thought("Keep these words 简体 繁體"),
                10,
            )
            .unwrap();
        draft.source_timezone = None;
        draft.source_utc_offset_minutes = None;
        source.connection.execute_batch("ALTER TABLE reflection_drafts DROP COLUMN source_timezone; ALTER TABLE reflection_drafts DROP COLUMN source_utc_offset_minutes; ALTER TABLE record_revisions DROP COLUMN source_timezone; ALTER TABLE record_revisions DROP COLUMN source_utc_offset_minutes;").unwrap();
        source.connection.execute_batch("DROP TABLE local_desktop_profile; DROP TABLE local_period_note_state; DROP TABLE local_period_notes; DROP TABLE local_routine_deliveries; DROP TABLE local_routines; DELETE FROM schema_migrations WHERE version>=3; PRAGMA user_version=2;").unwrap();
        source.backup_to(&path).unwrap();
        let upgraded = Store::open(&path).unwrap();
        assert_eq!(upgraded.list_drafts().unwrap(), vec![draft]);
        assert!(upgraded.list_routines().unwrap().is_empty());
        let before = Connection::open(upgraded.migration_backup().unwrap()).unwrap();
        let version: i64 = before
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap();
        assert_eq!(version, 2);
    }

    fn window_input() -> RoutineInput {
        let mut input = input();
        input.local_time = None;
        input.windows = vec![
            rhythm_domain::RoutineWindow {
                start: "08:30".into(),
                end: "12:00".into(),
            },
            rhythm_domain::RoutineWindow {
                start: "14:30".into(),
                end: "18:00".into(),
            },
        ];
        input
    }

    #[test]
    fn split_windows_claim_separately_and_reopen_as_context_without_renotifying() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("windows.sqlite3");
        let mut store = Store::open(&path).unwrap();
        let routine = store.save_routine(None, window_input()).unwrap();
        let morning = store.take_due_invitations(MONDAY, "UTC").unwrap();
        assert_eq!(morning.len(), 1);
        assert_eq!(morning[0].window_key, "08:30");
        assert_eq!(
            store
                .active_routine_windows(MONDAY + 7_200_000, "UTC")
                .unwrap(),
            morning
        );
        let afternoon_time = MONDAY + 19_800_000;
        assert_eq!(
            store
                .next_routine_invitation(&routine, MONDAY, "UTC")
                .unwrap()
                .unwrap()
                .due_at_utc_ms,
            afternoon_time
        );
        drop(store);
        let mut store = Store::open(&path).unwrap();
        assert!(
            store
                .take_due_invitations(MONDAY + 1, "UTC")
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            store.active_routine_windows(MONDAY + 1, "UTC").unwrap(),
            morning
        );
        assert!(
            store
                .active_routine_windows(MONDAY + 10_800_000, "UTC")
                .unwrap()
                .is_empty()
        );
        let afternoon = store.take_due_invitations(afternoon_time, "UTC").unwrap();
        assert_eq!(afternoon.len(), 1);
        assert_eq!(afternoon[0].window_key, "14:30");
        assert!(
            store
                .take_due_invitations(MONDAY, "UTC")
                .unwrap()
                .is_empty()
        );
        let next = store
            .next_routine_invitation(&routine, MONDAY - 3_600_000, "UTC")
            .unwrap()
            .unwrap();
        assert_eq!(next.local_date, "2026-11-03");
        assert_eq!(next.window_key, "08:30");
    }

    #[test]
    fn window_dismissal_is_durable_and_preserves_later_windows_and_backups() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("windows.sqlite3");
        let backup = directory.path().join("backup.sqlite3");
        let mut store = Store::open(&path).unwrap();
        let routine = store.save_routine(None, window_input()).unwrap();
        let morning = store
            .active_routine_windows(MONDAY, "UTC")
            .unwrap()
            .remove(0);
        store.dismiss_routine_window(&morning).unwrap();
        store.backup_to(&backup).unwrap();
        drop(store);
        let mut store = Store::open(&path).unwrap();
        assert!(
            store
                .active_routine_windows(MONDAY, "UTC")
                .unwrap()
                .is_empty()
        );
        assert!(
            store
                .take_due_invitations(MONDAY, "UTC")
                .unwrap()
                .is_empty()
        );
        let afternoon_time = MONDAY + 19_800_000;
        assert_eq!(
            store
                .active_routine_windows(afternoon_time, "UTC")
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            store
                .take_due_invitations(afternoon_time, "UTC")
                .unwrap()
                .len(),
            1
        );
        let mut restored = Store::open_in_memory().unwrap();
        restored.restore_from(&backup).unwrap();
        assert_eq!(restored.list_routines().unwrap(), vec![routine]);
        assert!(
            restored
                .active_routine_windows(MONDAY, "UTC")
                .unwrap()
                .is_empty()
        );
        assert!(
            restored
                .take_due_invitations(MONDAY, "UTC")
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            restored
                .take_due_invitations(afternoon_time, "UTC")
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            restored
                .active_routine_windows(MONDAY + 86_400_000, "UTC")
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn end_time_edit_keeps_claimed_start_and_updates_available_window() {
        let mut store = Store::open_in_memory().unwrap();
        let routine = store.save_routine(None, window_input()).unwrap();
        store.take_due_invitations(MONDAY, "UTC").unwrap();
        let mut edited = routine.input;
        edited.windows[0].end = "13:00".into();
        store.save_routine(Some(routine.id), edited).unwrap();
        assert!(
            store
                .take_due_invitations(MONDAY, "UTC")
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            store
                .active_routine_windows(MONDAY + 10_800_000, "UTC")
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn schema_three_upgrade_and_restore_preserve_legacy_rules_claims_and_journal() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("version-three.sqlite3");
        let mut source = Store::open_in_memory().unwrap();
        let mut draft = source
            .create_draft(
                crate::ReflectionInput::thought("Keep these words 简体 繁體"),
                10,
            )
            .unwrap();
        draft.source_timezone = None;
        draft.source_utc_offset_minutes = None;
        source.connection.execute_batch("ALTER TABLE reflection_drafts DROP COLUMN source_timezone; ALTER TABLE reflection_drafts DROP COLUMN source_utc_offset_minutes; ALTER TABLE record_revisions DROP COLUMN source_timezone; ALTER TABLE record_revisions DROP COLUMN source_utc_offset_minutes;").unwrap();
        let routine = source.save_routine(None, input()).unwrap();
        source.take_due_invitations(MONDAY, "UTC").unwrap();
        source.connection.execute_batch(
            "CREATE TEMP TABLE old_deliveries AS SELECT routine_id,local_date,due_at_utc_ms FROM local_routine_deliveries;
             DROP TABLE local_routine_deliveries;
             CREATE TABLE local_routine_deliveries (
                 routine_id TEXT NOT NULL REFERENCES local_routines(routine_id) ON DELETE CASCADE,
                 local_date TEXT NOT NULL,
                 due_at_utc_ms INTEGER NOT NULL,
                 PRIMARY KEY (routine_id, local_date)
             );
             INSERT INTO local_routine_deliveries SELECT * FROM old_deliveries;
             DROP TABLE old_deliveries;
             DROP TABLE local_period_note_state;
             DROP TABLE local_period_notes;
             DROP TABLE local_desktop_profile;
             DELETE FROM schema_migrations WHERE version>=4;
             PRAGMA user_version=3;"
        ).unwrap();
        source.backup_to(&path).unwrap();
        let mut restored = Store::open_in_memory().unwrap();
        restored.restore_from(&path).unwrap();
        assert_eq!(restored.list_drafts().unwrap(), vec![draft.clone()]);
        assert_eq!(restored.list_routines().unwrap(), vec![routine.clone()]);
        assert!(
            restored
                .take_due_invitations(MONDAY, "UTC")
                .unwrap()
                .is_empty()
        );
        let mut upgraded = Store::open(&path).unwrap();
        assert_eq!(upgraded.list_drafts().unwrap(), vec![draft]);
        assert_eq!(upgraded.list_routines().unwrap(), vec![routine]);
        assert!(
            upgraded
                .take_due_invitations(MONDAY, "UTC")
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            upgraded
                .take_due_invitations(MONDAY + 86_400_000, "UTC")
                .unwrap()
                .len(),
            1
        );
        let before = Connection::open(upgraded.migration_backup().unwrap()).unwrap();
        let version: i64 = before
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap();
        assert_eq!(version, 3);
        let count: i64 = before
            .query_row("SELECT count(*) FROM local_routine_deliveries", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(count, 1);
    }
}
