use std::sync::{Arc, Mutex};

use rhythm_desktop_service::{
    DesktopRequest as Request, DesktopService, HISTORY_PAGE_SIZE, ServiceError,
};
use rhythm_domain::{
    Clock, ClockSample, PeriodKind, RecoveryChoice, SessionOutcome, Settings, TimerState,
};
use rhythm_storage::{ReflectionInput, Store};

#[derive(Clone)]
struct TestClock(Arc<Mutex<ClockSample>>);

impl TestClock {
    fn new(epoch: &str) -> Self {
        Self(Arc::new(Mutex::new(ClockSample {
            epoch: epoch.into(),
            monotonic_ms: 1_000,
            utc_ms: 1_800_000_000_000,
        })))
    }

    fn advance(&self, milliseconds: u64) {
        let mut clock = self.0.lock().unwrap();
        clock.monotonic_ms += milliseconds;
        clock.utc_ms += i64::try_from(milliseconds).unwrap();
    }

    fn adjust_wall_clock(&self, milliseconds: i64) {
        self.0.lock().unwrap().utc_ms += milliseconds;
    }
}

impl Clock for TestClock {
    fn sample(&self) -> ClockSample {
        self.0.lock().unwrap().clone()
    }
}

fn write_words(service: &mut DesktopService<TestClock>, words: &str) {
    service
        .execute(Request::CreateDraft {
            content: ReflectionInput {
                kind: rhythm_storage::ReflectionKind::Thought,
                body: words.into(),
                session_id: None,
                reveal_at_utc_ms: None,
            },
        })
        .unwrap();
}

fn save_period_words(
    service: &mut DesktopService<TestClock>,
    body: &str,
) -> rhythm_storage::PeriodNote {
    let current = service.snapshot().unwrap().note_state.period_note.unwrap();
    service
        .execute(Request::SavePeriodNote {
            id: current.id,
            version: current.version,
            body: body.into(),
        })
        .unwrap()
        .period_note
        .unwrap()
}

#[test]
fn desktop_interruption_excludes_downtime_archives_notes_and_never_completes_rest() {
    for rest in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("interruption.sqlite3");
        let clock = TestClock::new("awake");
        let mut service = DesktopService::open(&path, clock.clone()).unwrap();
        service
            .execute(if rest {
                Request::SwitchToBreak {
                    duration_ms: Some(60_000),
                }
            } else {
                Request::SwitchToFocus {
                    duration_ms: Some(60_000),
                }
            })
            .unwrap();
        let note = save_period_words(&mut service, "Saved before sleep.\n慢慢来。\n慢慢來。");
        clock.advance(12_000);
        service.poll().unwrap();
        // Simulate a late/missed power callback, beyond the original deadline.
        clock.advance(3_600_000);
        let update = service.interrupt_for_desktop().unwrap();
        let terminal = update.terminal.unwrap();
        assert_eq!(terminal.elapsed_ms, 12_000);
        assert_eq!(terminal.outcome, SessionOutcome::EndedEarly);
        assert!(terminal.recovered);
        assert_eq!(terminal.ended_utc_ms, None);
        assert_eq!(update.checkpoint.completed_focus, 0);
        assert_eq!(update.checkpoint.completed_short_rests, 0);
        assert_eq!(
            update.checkpoint.state,
            TimerState::Ready {
                next: PeriodKind::Focus
            }
        );
        let view = service.snapshot().unwrap();
        assert_eq!(view.note_state.archived_notes[0].body, note.body);
        assert_eq!(view.note_state.period_note.unwrap().kind, "break");
        // A delayed UI flush remains attached to the interrupted period.
        let late = service
            .execute(Request::SavePeriodNote {
                id: note.id.clone(),
                version: note.version,
                body: "Last keystroke before sleep".into(),
            })
            .unwrap()
            .period_note
            .unwrap();
        assert!(late.archived_at_utc_ms.is_some());
        for _ in 0..3 {
            clock.advance(60_000);
            assert!(service.interrupt_for_desktop().unwrap().terminal.is_none());
            assert!(service.poll().unwrap().terminal.is_none());
        }
        assert_eq!(service.snapshot().unwrap().sessions.len(), 1);
        drop(service);
        let mut reopened = DesktopService::open(&path, TestClock::new("reopened")).unwrap();
        assert!(reopened.interrupt_for_desktop().unwrap().terminal.is_none());
        assert_eq!(
            reopened.snapshot().unwrap().note_state.archived_notes[0].body,
            late.body
        );
        reopened
            .execute(Request::SwitchToFocus { duration_ms: None })
            .unwrap();
        assert!(
            matches!(reopened.current_timer().checkpoint.state, TimerState::Running(active) if active.kind == PeriodKind::Focus)
        );
    }
}

#[test]
fn desktop_reopening_ends_interrupted_focus_without_recovery_choice_or_downtime() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("reopen.sqlite3");
    let clock = TestClock::new("old-process");
    let mut service = DesktopService::open(&path, clock.clone()).unwrap();
    service
        .execute(Request::SwitchToFocus {
            duration_ms: Some(60_000),
        })
        .unwrap();
    let note = save_period_words(&mut service, "Keep this note");
    clock.advance(8_000);
    service.poll().unwrap();
    drop(service);
    let new_clock = TestClock::new("new-process");
    new_clock.advance(86_400_000);
    let mut service = DesktopService::open(&path, new_clock).unwrap();
    let terminal = service.interrupt_for_desktop().unwrap().terminal.unwrap();
    assert_eq!(terminal.elapsed_ms, 8_000);
    assert_eq!(terminal.outcome, SessionOutcome::EndedEarly);
    assert_eq!(
        service.current_timer().checkpoint.state,
        TimerState::Ready {
            next: PeriodKind::Focus
        }
    );
    assert_eq!(
        service.snapshot().unwrap().note_state.archived_notes[0].body,
        note.body
    );
}

#[test]
fn focus_extension_is_durable_and_preserves_the_current_note() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("extension.sqlite3");
    let clock = TestClock::new("extension");
    let mut service = DesktopService::open(&path, clock.clone()).unwrap();
    service
        .execute(Request::SwitchToFocus {
            duration_ms: Some(1000),
        })
        .unwrap();
    let note = save_period_words(&mut service, "Keep this thought");
    clock.advance(500);
    let response = service
        .execute(Request::ExtendFocus { additional_ms: 500 })
        .unwrap();
    assert_eq!(response.view.note_state.period_note.unwrap(), note);
    assert!(response.view.sessions.is_empty());
    drop(service);
    let mut reopened = DesktopService::open(&path, clock.clone()).unwrap();
    let state = reopened.snapshot().unwrap();
    let TimerState::Running(active) = state.checkpoint.state else {
        panic!("expected focus")
    };
    assert_eq!(active.planned_ms, 1500);
    assert_eq!(active.elapsed_ms, 500);
    assert_eq!(state.note_state.period_note.unwrap(), note);
    clock.advance(1000);
    let update = reopened.poll().unwrap();
    assert_eq!(update.terminal.unwrap().elapsed_ms, 1500);
}

#[test]
fn archive_import_is_idempotent_preserves_conflicts_and_keeps_current_words() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("archive-import.sqlite3");
    let clock = TestClock::new("archive-import");
    let mut service = DesktopService::open(&path, clock.clone()).unwrap();
    service
        .execute(Request::SwitchToFocus {
            duration_ms: Some(1000),
        })
        .unwrap();
    let current = save_period_words(&mut service, "Current focus words");
    let first = rhythm_storage::PeriodNote {
        id: current.id.clone(), // A remote collision must never touch this editor.
        kind: "break".into(),
        body: "A gentle walk · 慢慢来\n🌿".into(),
        source_timezone: None,
        source_utc_offset_minutes: None,
        version: 3,
        created_at_utc_ms: 100,
        updated_at_utc_ms: 150,
        archived_at_utc_ms: Some(200),
    };
    let mut divergent = first.clone();
    divergent.body = "A different copy".into();
    let imported = service
        .execute(Request::ImportPeriodNotes {
            notes: vec![first.clone(), divergent.clone()],
        })
        .unwrap()
        .view;
    assert_eq!(imported.note_state.period_note, Some(current.clone()));
    assert_eq!(imported.note_state.archived_notes.len(), 2);
    assert!(
        imported
            .note_state
            .archived_notes
            .iter()
            .all(|note| note.id.starts_with("import:"))
    );
    assert!(
        imported
            .note_state
            .archived_notes
            .iter()
            .any(|note| note.body == first.body)
    );
    assert!(
        imported
            .note_state
            .archived_notes
            .iter()
            .any(|note| note.body == divergent.body)
    );
    assert!(imported.sessions.is_empty());
    let repeated = service
        .execute(Request::ImportPeriodNotes {
            notes: vec![first, divergent],
        })
        .unwrap()
        .view;
    assert_eq!(repeated, imported);
    drop(service);
    let reopened = DesktopService::open(&path, clock).unwrap();
    assert_eq!(reopened.snapshot().unwrap().note_state, imported.note_state);
}

#[test]
fn malformed_archive_batch_is_atomic() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("bad-import.sqlite3");
    let mut service = DesktopService::open(&path, TestClock::new("bad-import")).unwrap();
    let before = service.snapshot().unwrap();
    let valid = rhythm_storage::PeriodNote {
        id: "remote-note".into(),
        kind: "focus".into(),
        body: "valid".into(),
        source_timezone: None,
        source_utc_offset_minutes: None,
        version: 1,
        created_at_utc_ms: 100,
        updated_at_utc_ms: 100,
        archived_at_utc_ms: Some(200),
    };
    let mut invalid = valid.clone();
    invalid.archived_at_utc_ms = None;
    assert!(
        service
            .execute(Request::ImportPeriodNotes {
                notes: vec![valid.clone(), invalid]
            })
            .is_err()
    );
    assert_eq!(service.snapshot().unwrap(), before);
    let mut oversized = valid.clone();
    oversized.body = "文".repeat(33_334);
    assert!(
        service
            .execute(Request::ImportPeriodNotes {
                notes: vec![valid.clone(), oversized]
            })
            .is_err()
    );
    assert_eq!(service.snapshot().unwrap(), before);
    assert!(
        service
            .execute(Request::ImportPeriodNotes {
                notes: vec![valid; 1001]
            })
            .is_err()
    );
    assert_eq!(service.snapshot().unwrap(), before);
}

#[test]
fn each_manual_period_switch_archives_its_note_and_starts_empty() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("notes.sqlite3");
    let clock = TestClock::new("period-notes");
    let mut service = DesktopService::open(&path, clock.clone()).unwrap();
    assert!(service.snapshot().unwrap().note_state.period_note.is_none());
    service
        .execute(Request::SwitchToFocus {
            duration_ms: Some(1000),
        })
        .unwrap();
    let focus_note = save_period_words(&mut service, "Focus only\n专注 · 專注");
    clock.advance(200);
    let response = service
        .execute(Request::SwitchToBreak {
            duration_ms: Some(1000),
        })
        .unwrap();
    let current = response.view.note_state.period_note.as_ref().unwrap();
    assert_eq!(current.kind, "break");
    assert_eq!(current.body, "");
    assert_eq!(response.view.note_state.archived_notes.len(), 1);
    assert_eq!(
        response.view.note_state.archived_notes[0].body,
        focus_note.body
    );
    assert_eq!(response.view, service.snapshot().unwrap());
    let repeated = service
        .execute(Request::SwitchToBreak { duration_ms: None })
        .unwrap();
    assert_eq!(repeated.view.note_state, response.view.note_state);
    let break_note = save_period_words(&mut service, "Break only");
    clock.advance(100);
    let next = service
        .execute(Request::SwitchToFocus { duration_ms: None })
        .unwrap();
    assert_eq!(next.view.note_state.archived_notes.len(), 2);
    assert_eq!(next.view.note_state.archived_notes[0].body, break_note.body);
    assert_eq!(next.view.note_state.period_note.unwrap().body, "");
}

#[test]
fn deadline_keeps_late_composition_in_the_original_archive_and_rotates_untimed_break() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("deadline.sqlite3");
    let clock = TestClock::new("period-notes");
    let mut service = DesktopService::open(&path, clock.clone()).unwrap();
    service
        .execute(Request::SwitchToFocus {
            duration_ms: Some(1000),
        })
        .unwrap();
    let original = service.snapshot().unwrap().note_state.period_note.unwrap();
    clock.advance(1000);
    let completed = service.poll().unwrap();
    let next_id = completed.note_period_id.unwrap();
    assert_ne!(next_id, original.id);
    assert_eq!(
        service.snapshot().unwrap().note_state.archived_notes.len(),
        0
    );
    let late = service
        .execute(Request::SavePeriodNote {
            id: original.id.clone(),
            version: original.version,
            body: "IME completed after deadline: 留住這些話".into(),
        })
        .unwrap();
    assert!(late.period_note.unwrap().archived_at_utc_ms.is_some());
    assert_eq!(
        late.view.note_state.period_note.as_ref().unwrap().id,
        next_id
    );
    assert_eq!(late.view.note_state.period_note.as_ref().unwrap().body, "");
    assert_eq!(late.view.note_state.archived_notes.len(), 1);
    assert!(
        service
            .execute(Request::SavePeriodNote {
                id: original.id,
                version: original.version,
                body: "stale".into()
            })
            .is_err()
    );
    assert!(service.poll().unwrap().terminal.is_none());
    let untimed = save_period_words(&mut service, "A thought while resting beyond the timer");
    clock.advance(100);
    let next = service
        .execute(Request::SwitchToFocus { duration_ms: None })
        .unwrap();
    assert!(
        next.view
            .note_state
            .archived_notes
            .iter()
            .any(|note| note.id == untimed.id && note.body == untimed.body)
    );
    assert_eq!(next.view, service.snapshot().unwrap());
}

#[test]
fn archive_failure_rolls_back_the_entire_timer_switch_and_note_save_failures_preserve_words() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("failure.sqlite3");
    let clock = TestClock::new("period-notes");
    let mut service = DesktopService::open(&path, clock.clone()).unwrap();
    service
        .execute(Request::SwitchToFocus {
            duration_ms: Some(1000),
        })
        .unwrap();
    let saved = save_period_words(&mut service, "Keep me");
    let before = service.snapshot().unwrap();
    let connection = rusqlite::Connection::open(&path).unwrap();
    connection.execute_batch("CREATE TRIGGER reject_period_note BEFORE UPDATE ON local_period_notes BEGIN SELECT RAISE(ABORT,'simulated note failure'); END;").unwrap();
    clock.advance(200);
    assert!(
        service
            .execute(Request::SwitchToBreak { duration_ms: None })
            .is_err()
    );
    assert_eq!(service.snapshot().unwrap(), before);
    assert!(
        service
            .execute(Request::SavePeriodNote {
                id: saved.id,
                version: saved.version,
                body: "unsaved".into()
            })
            .is_err()
    );
    assert_eq!(service.snapshot().unwrap(), before);
    connection
        .execute_batch("DROP TRIGGER reject_period_note;")
        .unwrap();
    assert!(
        service
            .execute(Request::SwitchToBreak { duration_ms: None })
            .unwrap()
            .terminal
            .is_some()
    );
    assert_eq!(
        service.snapshot().unwrap().note_state.archived_notes[0].body,
        "Keep me"
    );
}

#[test]
fn old_shared_words_migrate_once_and_period_notes_survive_quit_backup_export_and_restore() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("migration.sqlite3");
    {
        let mut store = Store::open(&path).unwrap();
        store
            .create_draft(ReflectionInput::thought("Old shared words"), 10)
            .unwrap();
    }
    let clock = TestClock::new("period-notes");
    let mut service = DesktopService::open(&path, clock.clone()).unwrap();
    let initial = service.snapshot().unwrap();
    assert_eq!(initial.note_state.archived_notes.len(), 1);
    assert_eq!(
        initial.note_state.archived_notes[0].body,
        "Old shared words"
    );
    assert_eq!(initial.drafts.len(), 1);
    assert!(initial.note_state.period_note.is_none());
    service
        .execute(Request::SwitchToFocus {
            duration_ms: Some(1000),
        })
        .unwrap();
    let note = save_period_words(&mut service, "New per-period words\n中文");
    clock.advance(200);
    service.finish_for_quit().unwrap();
    drop(service);
    let mut reopened = DesktopService::open(&path, clock).unwrap();
    let state = reopened.snapshot().unwrap().note_state;
    assert_eq!(state.archived_notes.len(), 2);
    assert!(
        state
            .archived_notes
            .iter()
            .any(|item| item.id == note.id && item.body == note.body)
    );
    assert_eq!(state.period_note.as_ref().unwrap().body, "");
    let backup = directory.path().join("backup.sqlite3");
    let export = directory.path().join("notes.md");
    reopened.backup_to(&backup).unwrap();
    reopened.export_markdown_to(&export).unwrap();
    assert!(
        std::fs::read_to_string(export)
            .unwrap()
            .contains(&note.body)
    );
    save_period_words(&mut reopened, "Later untimed note");
    reopened.restore_from(&backup).unwrap();
    assert_eq!(reopened.snapshot().unwrap().note_state, state);
}

#[test]
fn direct_state_switches_return_committed_views_and_keep_shared_notes() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("rhythm.sqlite3");
    let clock = TestClock::new("process-a");
    let mut service = DesktopService::open(&path, clock.clone()).unwrap();
    write_words(&mut service, "A shared thought. 留一點空間。");
    let drafts = service.drafts().unwrap();
    let request: Request =
        serde_json::from_str(r#"{"op":"switch_to_focus","data":{"duration_ms":1000}}"#).unwrap();
    let started = service.execute(request).unwrap();
    clock.advance(250);
    let request: Request =
        serde_json::from_str(r#"{"op":"switch_to_break","data":{"duration_ms":null}}"#).unwrap();
    let switched = service.execute(request.clone()).unwrap();
    assert_eq!(switched.view.revision, started.view.revision + 1);
    assert_eq!(switched.view, service.snapshot().unwrap());
    assert_eq!(switched.view.drafts, drafts);
    assert_eq!(switched.terminal.as_ref().unwrap().elapsed_ms, 250);
    assert!(
        matches!(switched.view.checkpoint.state, TimerState::Running(ref active)
        if active.kind == PeriodKind::ShortRest && active.planned_ms == 300_000)
    );
    let repeated = service.execute(request).unwrap();
    assert!(repeated.terminal.is_none());
    assert_eq!(repeated.view, switched.view);
    drop(service);
    let reopened = DesktopService::open(&path, TestClock::new("process-b")).unwrap();
    assert_eq!(reopened.drafts().unwrap(), drafts);
    assert_eq!(reopened.snapshot().unwrap().sessions.len(), 1);
}

#[test]
fn restore_preserves_safety_copy_and_replaces_live_application_state() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("source.sqlite3");
    let backup = directory.path().join("backup.sqlite3");
    let current = directory.path().join("current.sqlite3");
    let clock = TestClock::new("same-process");
    let mut original = DesktopService::open(&source, clock.clone()).unwrap();
    write_words(&mut original, "Dear me\n简体 · 繁體");
    original
        .execute(Request::StartFocus {
            duration_ms: Some(60_000),
        })
        .unwrap();
    clock.advance(1_000);
    original.poll().unwrap();
    original.backup_to(&backup).unwrap();

    let mut service = DesktopService::open(&current, clock.clone()).unwrap();
    write_words(&mut service, "Keep my previous draft");
    let before = service.snapshot().unwrap();
    let restored = service.restore_from(&backup).unwrap();
    let safety = Store::open(&restored.safety_backup).unwrap();
    assert_eq!(safety.list_drafts().unwrap(), before.drafts);
    let after = service.snapshot().unwrap();
    assert!(after.revision > before.revision);
    assert_eq!(after.drafts[0].content.body, "Dear me\n简体 · 繁體");
    assert!(matches!(
        after.checkpoint.state,
        TimerState::RecoveryRequired(_)
    ));
    assert!(service.poll().unwrap().terminal.is_none());
    service
        .execute(Request::Recover {
            choice: RecoveryChoice::End,
        })
        .unwrap();
    service
        .execute(Request::StartFocus {
            duration_ms: Some(1_000),
        })
        .unwrap();
    clock.advance(1_000);
    assert!(service.poll().unwrap().terminal.is_some());
    let export = directory.path().join("journal.md");
    service.export_markdown_to(&export).unwrap();
    assert!(
        std::fs::read_to_string(export)
            .unwrap()
            .contains("简体 · 繁體")
    );
}

#[test]
fn restore_rejects_active_period_and_invalid_input_without_changing_view() {
    let directory = tempfile::tempdir().unwrap();
    let mut service = DesktopService::open(
        directory.path().join("current.sqlite3"),
        TestClock::new("a"),
    )
    .unwrap();
    write_words(&mut service, "Keep this draft");
    let invalid = directory.path().join("invalid.sqlite3");
    std::fs::write(&invalid, b"not a backup").unwrap();
    let before = service.snapshot().unwrap();
    assert!(service.restore_from(&invalid).is_err());
    assert_eq!(service.snapshot().unwrap(), before);
    service
        .execute(Request::StartFocus { duration_ms: None })
        .unwrap();
    let active = service.snapshot().unwrap();
    assert!(matches!(
        service.restore_from(&invalid),
        Err(ServiceError::RestoreWhileActive)
    ));
    assert_eq!(service.snapshot().unwrap(), active);
    service.execute(Request::Pause).unwrap();
    assert!(matches!(
        service.restore_from(&invalid),
        Err(ServiceError::RestoreWhileActive)
    ));
}

#[test]
fn explicit_timer_actions_commit_and_pause_excludes_elapsed_time() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("rhythm.sqlite3");
    let clock = TestClock::new("process-a");
    let mut service = DesktopService::open(&path, clock.clone()).unwrap();
    let started = service
        .execute(Request::StartFocus {
            duration_ms: Some(10_000),
        })
        .unwrap();
    let TimerState::Running(active) = started.view.checkpoint.state else {
        panic!("focus did not start")
    };
    assert!(uuid::Uuid::parse_str(&active.id).is_ok());
    clock.advance(2_000);
    let paused = service.execute(Request::Pause).unwrap();
    assert!(
        matches!(paused.view.checkpoint.state, TimerState::Paused(ref active) if active.elapsed_ms == 2_000)
    );
    clock.advance(60_000);
    service.execute(Request::Resume).unwrap();
    clock.advance(8_000);
    let finished = service.poll().unwrap();
    let terminal = finished.terminal.unwrap();
    assert_eq!(terminal.id, active.id);
    assert_eq!(terminal.elapsed_ms, 10_000);
    assert_eq!(terminal.outcome, SessionOutcome::Completed);
    assert_eq!(terminal.intervals.len(), 2);
    assert!(matches!(
        finished.checkpoint.state,
        TimerState::Ready {
            next: PeriodKind::ShortRest
        }
    ));
    assert!(service.poll().unwrap().terminal.is_none());
    drop(service);
    let persisted = Store::open(&path).unwrap();
    assert_eq!(persisted.list_sessions(100, 0).unwrap(), vec![terminal]);
}

#[test]
fn suspend_inclusive_resume_completes_once_at_the_effective_deadline() {
    let directory = tempfile::tempdir().unwrap();
    let clock = TestClock::new("process-a");
    let mut service =
        DesktopService::open(directory.path().join("rhythm.sqlite3"), clock.clone()).unwrap();
    let start = clock.sample().utc_ms;
    service
        .execute(Request::StartFocus {
            duration_ms: Some(5_000),
        })
        .unwrap();
    clock.advance(1_000);
    service.poll().unwrap();
    // The native elapsed clock includes sleep; no JavaScript/native worker ticks happen here.
    clock.advance(120_000);
    let wake = service.poll().unwrap();
    let terminal = wake.terminal.unwrap();
    assert_eq!(terminal.elapsed_ms, 5_000);
    assert_eq!(terminal.ended_utc_ms, Some(start + 5_000));
    assert_eq!(terminal.detected_utc_ms, start + 121_000);
    assert_eq!(wake.checkpoint.completed_focus, 1);
    let revision = wake.revision;
    for _ in 0..3 {
        let repeat = service.poll().unwrap();
        assert!(repeat.terminal.is_none());
        assert_eq!(repeat.revision, revision);
        assert!(matches!(repeat.checkpoint.state, TimerState::Ready { .. }));
    }
    assert_eq!(service.snapshot().unwrap().sessions.len(), 1);
}

#[test]
fn restart_requires_a_choice_and_does_not_credit_the_unobserved_gap() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("rhythm.sqlite3");
    let clock = TestClock::new("process-a");
    let mut service = DesktopService::open(&path, clock.clone()).unwrap();
    service
        .execute(Request::StartFocus {
            duration_ms: Some(10_000),
        })
        .unwrap();
    clock.advance(2_000);
    service.poll().unwrap();
    drop(service);

    let restarted_clock = TestClock::new("process-b");
    restarted_clock.advance(7_200_000);
    let mut restarted = DesktopService::open(&path, restarted_clock.clone()).unwrap();
    let view = restarted.snapshot().unwrap();
    assert!(
        matches!(view.checkpoint.state, TimerState::RecoveryRequired(ref active) if active.elapsed_ms == 2_000)
    );
    assert!(view.sessions.is_empty());
    assert!(restarted.poll().unwrap().terminal.is_none());
    restarted
        .execute(Request::Recover {
            choice: RecoveryChoice::Continue,
        })
        .unwrap();
    restarted_clock.advance(8_000);
    let terminal = restarted.poll().unwrap().terminal.unwrap();
    assert_eq!(terminal.elapsed_ms, 10_000);
    assert!(terminal.recovered);
    assert_eq!(terminal.outcome, SessionOutcome::Completed);
}

#[test]
fn ending_restart_recovery_keeps_only_verified_time() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("rhythm.sqlite3");
    let clock = TestClock::new("process-a");
    let mut service = DesktopService::open(&path, clock.clone()).unwrap();
    service
        .execute(Request::StartFocus {
            duration_ms: Some(10_000),
        })
        .unwrap();
    clock.advance(2_000);
    service.poll().unwrap();
    drop(service);
    let mut restarted = DesktopService::open(&path, TestClock::new("process-b")).unwrap();
    let terminal = restarted
        .execute(Request::Recover {
            choice: RecoveryChoice::End,
        })
        .unwrap()
        .terminal
        .unwrap();
    assert_eq!(terminal.elapsed_ms, 2_000);
    assert_eq!(terminal.outcome, SessionOutcome::EndedEarly);
    assert_eq!(terminal.ended_utc_ms, None);
    assert_eq!(restarted.snapshot().unwrap().checkpoint.completed_focus, 0);
}

#[test]
fn utc_clock_correction_does_not_finish_focus_early() {
    let directory = tempfile::tempdir().unwrap();
    let clock = TestClock::new("process-a");
    let mut service =
        DesktopService::open(directory.path().join("rhythm.sqlite3"), clock.clone()).unwrap();
    service
        .execute(Request::StartFocus {
            duration_ms: Some(10_000),
        })
        .unwrap();
    clock.advance(2_000);
    clock.adjust_wall_clock(3_600_000);
    let update = service.poll().unwrap();
    assert!(update.terminal.is_none());
    assert!(
        matches!(update.checkpoint.state, TimerState::Running(ref active) if active.elapsed_ms == 2_000)
    );
    clock.advance(8_000);
    assert_eq!(service.poll().unwrap().terminal.unwrap().ended_utc_ms, None);
}

#[test]
fn autosaved_unicode_draft_survives_restart_and_edit_preserves_history() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("rhythm.sqlite3");
    let clock = TestClock::new("process-a");
    let mut service = DesktopService::open(&path, clock.clone()).unwrap();
    let draft = service
        .execute(Request::CreateDraft {
            content: ReflectionInput::thought(""),
        })
        .unwrap()
        .draft
        .unwrap();
    let text = "  留一點空間。\n今天也可以慢慢來。\nA little room. 🌿  ";
    clock.advance(10);
    let draft = service
        .execute(Request::SaveDraft {
            id: draft.id,
            version: draft.version,
            body: text.into(),
        })
        .unwrap()
        .draft
        .unwrap();
    drop(service);

    let mut reopened = DesktopService::open(&path, TestClock::new("process-b")).unwrap();
    assert_eq!(reopened.snapshot().unwrap().drafts, vec![draft.clone()]);
    let finalized = reopened
        .execute(Request::FinalizeDraft {
            id: draft.id,
            version: draft.version,
        })
        .unwrap();
    assert!(finalized.view.drafts.is_empty());
    let reflection = &finalized.view.reflections[0];
    assert_eq!(reflection.heads[0].content.body, text);
    let record_id = reflection.id.clone();
    let draft = reopened
        .execute(Request::EditReflection {
            id: record_id.clone(),
        })
        .unwrap()
        .draft
        .unwrap();
    let draft = reopened
        .execute(Request::SaveDraft {
            id: draft.id,
            version: draft.version,
            body: "改寫，原文仍在。".into(),
        })
        .unwrap()
        .draft
        .unwrap();
    let edited = reopened
        .execute(Request::FinalizeDraft {
            id: draft.id,
            version: draft.version,
        })
        .unwrap();
    assert_eq!(
        edited.view.reflections[0].heads[0].content.body,
        "改寫，原文仍在。"
    );
    drop(reopened);
    let history = Store::open(&path)
        .unwrap()
        .revision_history(&record_id)
        .unwrap();
    assert_eq!(history.len(), 2);
    assert_eq!(history[0].content.body, text);
    assert_eq!(history[1].parents, vec![history[0].id.clone()]);
}

#[test]
fn stale_autosave_is_rejected_and_durable_draft_is_preserved() {
    let directory = tempfile::tempdir().unwrap();
    let mut service = DesktopService::open(
        directory.path().join("rhythm.sqlite3"),
        TestClock::new("process-a"),
    )
    .unwrap();
    let draft = service
        .execute(Request::CreateDraft {
            content: ReflectionInput::thought("first"),
        })
        .unwrap()
        .draft
        .unwrap();
    let latest = service
        .execute(Request::SaveDraft {
            id: draft.id.clone(),
            version: draft.version,
            body: "latest".into(),
        })
        .unwrap()
        .draft
        .unwrap();
    let failure = service
        .execute(Request::SaveDraft {
            id: draft.id,
            version: draft.version,
            body: "stale".into(),
        })
        .unwrap_err();
    assert!(matches!(
        failure,
        ServiceError::Storage(rhythm_storage::Error::DraftConflict { .. })
    ));
    assert_eq!(service.snapshot().unwrap().drafts, vec![latest]);
}

#[test]
fn invalid_start_does_not_mutate_timer_and_settings_apply_to_future_periods() {
    let directory = tempfile::tempdir().unwrap();
    let mut service = DesktopService::open(
        directory.path().join("rhythm.sqlite3"),
        TestClock::new("process-a"),
    )
    .unwrap();
    let before = service.snapshot().unwrap();
    assert!(
        service
            .execute(Request::StartFocus {
                duration_ms: Some(0)
            })
            .is_err()
    );
    assert_eq!(service.snapshot().unwrap(), before);
    service
        .execute(Request::StartFocus {
            duration_ms: Some(10_000),
        })
        .unwrap();
    let settings = Settings {
        focus_ms: 20_000,
        ..Settings::default()
    };
    let view = service
        .execute(Request::UpdateSettings {
            settings: settings.clone(),
        })
        .unwrap()
        .view;
    assert_eq!(view.checkpoint.settings, settings);
    assert!(
        matches!(view.checkpoint.state, TimerState::Running(ref active) if active.planned_ms == 10_000)
    );
}

#[test]
fn history_snapshot_is_bounded_without_hiding_drafts() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("rhythm.sqlite3");
    let clock = TestClock::new("process-a");
    let mut store = Store::open(&path).unwrap();
    for index in 0..HISTORY_PAGE_SIZE + 1 {
        let draft = store
            .create_draft(
                ReflectionInput::thought(format!("Note {index}")),
                i64::from(index),
            )
            .unwrap();
        store
            .finalize_draft(&draft.id, draft.version, i64::from(index))
            .unwrap();
    }
    let draft = store
        .create_draft(ReflectionInput::thought("unfinished"), 1_000)
        .unwrap();
    drop(store);
    let view = DesktopService::open(&path, clock)
        .unwrap()
        .snapshot()
        .unwrap();
    assert_eq!(view.reflections.len(), HISTORY_PAGE_SIZE as usize);
    assert_eq!(
        view.reflections[0].heads[0].content.body,
        format!("Note {HISTORY_PAGE_SIZE}")
    );
    assert_eq!(view.drafts, vec![draft]);
}

#[test]
fn wire_requests_accept_only_known_operations_and_reject_extra_fields() {
    let start: Request =
        serde_json::from_str(r#"{"op":"start_focus","data":{"duration_ms":1000}}"#).unwrap();
    assert_eq!(
        start,
        Request::StartFocus {
            duration_ms: Some(1_000)
        }
    );
    assert!(
        serde_json::from_str::<Request>(r#"{"op":"execute_sql","data":{"sql":"SELECT 1"}}"#)
            .is_err()
    );
    assert!(
        serde_json::from_str::<Request>(
            r#"{"op":"start_focus","data":{"duration_ms":1000,"session_id":"injected"}}"#
        )
        .is_err()
    );
}

#[test]
fn explicit_quit_records_a_single_terminal_even_when_repeated() {
    for paused in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("rhythm.sqlite3");
        let clock = TestClock::new("process-a");
        let mut service = DesktopService::open(&path, clock.clone()).unwrap();
        service
            .execute(Request::StartFocus {
                duration_ms: Some(10_000),
            })
            .unwrap();
        clock.advance(2_000);
        if paused {
            service.execute(Request::Pause).unwrap();
            clock.advance(60_000);
        }
        let quit = service.finish_for_quit().unwrap();
        let terminal = quit.terminal.unwrap();
        assert_eq!(terminal.outcome, SessionOutcome::EndedEarly);
        assert_eq!(terminal.elapsed_ms, 2_000);
        let repeat = service.finish_for_quit().unwrap();
        assert!(repeat.terminal.is_none());
        assert_eq!(repeat.revision, quit.revision);
        assert_eq!(service.snapshot().unwrap().sessions, vec![terminal]);
    }
}

#[test]
fn quit_at_deadline_keeps_completion_and_quit_during_recovery_keeps_verified_time() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("rhythm.sqlite3");
    let clock = TestClock::new("process-a");
    let mut service = DesktopService::open(&path, clock.clone()).unwrap();
    service
        .execute(Request::StartFocus {
            duration_ms: Some(1_000),
        })
        .unwrap();
    clock.advance(2_000);
    assert_eq!(
        service.finish_for_quit().unwrap().terminal.unwrap().outcome,
        SessionOutcome::Completed
    );
    service
        .execute(Request::StartFocus {
            duration_ms: Some(10_000),
        })
        .unwrap();
    clock.advance(2_000);
    service.poll().unwrap();
    drop(service);
    let mut service = DesktopService::open(&path, TestClock::new("process-b")).unwrap();
    let terminal = service.finish_for_quit().unwrap().terminal.unwrap();
    assert_eq!(terminal.elapsed_ms, 2_000);
    assert_eq!(terminal.outcome, SessionOutcome::EndedEarly);
    assert!(terminal.recovered);
    assert!(service.finish_for_quit().unwrap().terminal.is_none());
    assert_eq!(service.snapshot().unwrap().sessions.len(), 2);
}

#[test]
fn unreadable_journal_blocks_a_new_ui_mutation_but_not_durable_quit() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("rhythm.sqlite3");
    let clock = TestClock::new("process-a");
    let mut service = DesktopService::open(&path, clock.clone()).unwrap();
    service
        .execute(Request::StartFocus {
            duration_ms: Some(10_000),
        })
        .unwrap();
    service
        .execute(Request::CreateDraft {
            content: ReflectionInput::thought("unfinished"),
        })
        .unwrap();
    let connection = rusqlite::Connection::open(&path).unwrap();
    connection
        .execute("UPDATE reflection_drafts SET payload_json='{}'", [])
        .unwrap();
    assert!(
        service
            .execute(Request::CreateDraft {
                content: ReflectionInput::thought("must not save")
            })
            .is_err()
    );
    assert_eq!(
        connection
            .query_row::<i64, _, _>("SELECT count(*) FROM reflection_drafts", [], |row| row
                .get(0))
            .unwrap(),
        1
    );
    clock.advance(2_000);
    let terminal = service.finish_for_quit().unwrap().terminal.unwrap();
    assert_eq!(terminal.elapsed_ms, 2_000);
    assert!(service.finish_for_quit().unwrap().terminal.is_none());
    assert_eq!(
        Store::open(&path)
            .unwrap()
            .get_session(&terminal.id)
            .unwrap(),
        Some(terminal)
    );
}

#[test]
fn committed_timer_response_is_not_lost_to_a_later_journal_read_error() {
    use std::sync::atomic::{AtomicBool, Ordering};

    struct CorruptOnSampleClock {
        clock: TestClock,
        path: std::path::PathBuf,
        armed: Arc<AtomicBool>,
    }

    impl Clock for CorruptOnSampleClock {
        fn sample(&self) -> ClockSample {
            if self.armed.swap(false, Ordering::SeqCst) {
                // Simulate an unrelated row becoming unreadable after the preflight view
                // has loaded, but before the timer's successful durable mutation.
                rusqlite::Connection::open(&self.path)
                    .unwrap()
                    .execute("UPDATE reflection_drafts SET payload_json='{}'", [])
                    .unwrap();
            }
            self.clock.sample()
        }
    }

    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("rhythm.sqlite3");
    let clock = TestClock::new("process-a");
    let armed = Arc::new(AtomicBool::new(false));
    let mut service = DesktopService::open(
        &path,
        CorruptOnSampleClock {
            clock: clock.clone(),
            path: path.clone(),
            armed: Arc::clone(&armed),
        },
    )
    .unwrap();
    service
        .execute(Request::StartFocus {
            duration_ms: Some(1_000),
        })
        .unwrap();
    service
        .execute(Request::CreateDraft {
            content: ReflectionInput::thought("unfinished"),
        })
        .unwrap();
    clock.advance(1_000);
    armed.store(true, Ordering::SeqCst);
    let response = service.execute(Request::EndEarly).unwrap();
    let terminal = response.terminal.unwrap();
    assert_eq!(terminal.outcome, SessionOutcome::Completed);
    assert_eq!(response.view.sessions, vec![terminal.clone()]);
    assert!(service.snapshot().is_err());
    assert_eq!(
        Store::open(&path)
            .unwrap()
            .get_session(&terminal.id)
            .unwrap(),
        Some(terminal)
    );
    assert!(service.poll().unwrap().terminal.is_none());
}

#[test]
fn mutation_responses_match_fresh_views_including_history_ordering() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("rhythm.sqlite3");
    let clock = TestClock::new("process-a");
    let mut service = DesktopService::open(&path, clock.clone()).unwrap();
    let mut first_record = None;
    // More than a page verifies that editing a record outside the captured view does not
    // move its creation date or accidentally displace a newer entry.
    for index in 0..HISTORY_PAGE_SIZE + 1 {
        let response = service
            .execute(Request::CreateDraft {
                content: ReflectionInput::thought(format!("Note {index}")),
            })
            .unwrap();
        assert_eq!(response.view, service.snapshot().unwrap());
        let draft = response.draft.unwrap();
        let response = service
            .execute(Request::FinalizeDraft {
                id: draft.id,
                version: draft.version,
            })
            .unwrap();
        assert_eq!(response.view, service.snapshot().unwrap());
        if index == 0 {
            first_record = Some(response.view.reflections[0].id.clone());
        }
        clock.advance(1);
    }
    let response = service
        .execute(Request::EditReflection {
            id: first_record.unwrap(),
        })
        .unwrap();
    assert_eq!(response.view, service.snapshot().unwrap());
    let draft = response.draft.unwrap();
    let response = service
        .execute(Request::SaveDraft {
            id: draft.id,
            version: draft.version,
            body: "revised oldest".into(),
        })
        .unwrap();
    assert_eq!(response.view, service.snapshot().unwrap());
    let draft = response.draft.unwrap();
    let response = service
        .execute(Request::FinalizeDraft {
            id: draft.id,
            version: draft.version,
        })
        .unwrap();
    assert_eq!(response.view, service.snapshot().unwrap());
    let response = service
        .execute(Request::CreateDraft {
            content: ReflectionInput::thought("discard"),
        })
        .unwrap();
    let draft = response.draft.unwrap();
    let response = service
        .execute(Request::DiscardDraft {
            id: draft.id,
            version: draft.version,
        })
        .unwrap();
    assert_eq!(response.view, service.snapshot().unwrap());
    for _ in 0..3 {
        service
            .execute(Request::StartFocus {
                duration_ms: Some(1_000),
            })
            .unwrap();
        clock.advance(1_000);
        let response = service.execute(Request::EndEarly).unwrap();
        assert_eq!(response.view, service.snapshot().unwrap());
        clock.adjust_wall_clock(-10_000);
    }
}

#[test]
fn desktop_interruption_suppresses_missed_routines_without_ending_new_focus() {
    use rhythm_domain::{RoutineInput, RoutineWindow};

    const MONDAY_9AM: i64 = 1_793_610_000_000;
    for (daily_window, missed_suspend) in
        [(false, false), (false, true), (true, false), (true, true)]
    {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("missed-routine.sqlite3");
        let clock = TestClock::new("before-sleep");
        clock.adjust_wall_clock(MONDAY_9AM - 60_000 - clock.sample().utc_ms);
        let mut service = DesktopService::open(&path, clock.clone()).unwrap();
        service
            .execute(Request::SaveRoutine {
                id: None,
                routine: RoutineInput {
                    name: "Morning".into(),
                    weekdays: vec![1, 2, 3, 4, 5],
                    local_time: (!daily_window).then(|| "09:00".into()),
                    windows: if daily_window {
                        vec![RoutineWindow {
                            start: "09:00".into(),
                            end: "12:00".into(),
                        }]
                    } else {
                        vec![]
                    },
                    timezone: "UTC".into(),
                    focus_ms: 60_000,
                    enabled: true,
                },
            })
            .unwrap();
        let routine_id = service.routine_view().routines[0].id.clone();
        service
            .execute(Request::SwitchToFocus {
                duration_ms: Some(600_000),
            })
            .unwrap();
        clock.advance(12_000);
        service.poll().unwrap();
        if !missed_suspend {
            service.interrupt_for_desktop().unwrap();
        }

        // No routine polling occurs during sleep. The first resume operation
        // claims the 09:00 delivery silently even though its window is still open.
        clock.advance(120_000);
        if missed_suspend {
            service.interrupt_for_desktop().unwrap();
        } else {
            service.suppress_missed_desktop_invitations();
        }
        assert!(service.poll_routines().invitations.is_empty());
        assert_eq!(
            service.current_timer().checkpoint.state,
            TimerState::Ready {
                next: PeriodKind::Focus
            }
        );
        assert_eq!(service.snapshot().unwrap().sessions[0].elapsed_ms, 12_000);
        assert_eq!(
            service
                .snapshot()
                .unwrap()
                .note_state
                .period_note
                .unwrap()
                .kind,
            "break"
        );

        let started = service
            .execute(Request::SwitchToFocus { duration_ms: None })
            .unwrap();
        let active = match started.view.checkpoint.state {
            TimerState::Running(active) => active,
            state => panic!("expected deliberate Focus, got {state:?}"),
        };
        assert_eq!(active.kind, PeriodKind::Focus);
        assert_eq!(active.planned_ms, Settings::default().focus_ms);
        // A late resume callback must not interrupt the newly chosen period.
        service.suppress_missed_desktop_invitations();
        service.suppress_missed_desktop_invitations();
        assert_eq!(
            service.current_timer().checkpoint.state,
            TimerState::Running(active)
        );
        assert_eq!(service.snapshot().unwrap().sessions.len(), 1);
        assert!(service.poll_routines().invitations.is_empty());
        service.execute(Request::EndEarly).unwrap();
        drop(service);

        // Delivery was claimed in storage, so reopening cannot replay it.
        clock.0.lock().unwrap().epoch = "after-reopen".into();
        let mut service = DesktopService::open(&path, clock.clone()).unwrap();
        assert!(service.poll_routines().invitations.is_empty());
        assert!(service.ready_for_invitation());

        // Suppression applies to the missed occurrence, not tomorrow's schedule.
        clock.advance(u64::try_from(MONDAY_9AM + 86_400_000 - clock.sample().utc_ms).unwrap());
        let future = service.poll_routines();
        assert_eq!(future.invitations.len(), 1);
        let invitation = &future.invitations[0];
        assert_eq!(invitation.routine_id, routine_id);
        assert_eq!(invitation.due_at_utc_ms, MONDAY_9AM + 86_400_000);
        assert!(service.ready_for_invitation());
        let started = service
            .execute(Request::AcceptInvitation {
                routine_id: invitation.routine_id.clone(),
                due_at_utc_ms: invitation.due_at_utc_ms,
            })
            .unwrap();
        assert!(
            matches!(started.view.checkpoint.state, TimerState::Running(active)
                if active.kind == PeriodKind::Focus && active.planned_ms == 60_000)
        );
    }
}
