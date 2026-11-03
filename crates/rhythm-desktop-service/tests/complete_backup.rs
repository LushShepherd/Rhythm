use rhythm_desktop_service::{DesktopProfile, DesktopRequest, DesktopService};
use rhythm_domain::{Clock, ClockSample, PeriodKind, SessionOutcome, TimerState};
use rhythm_storage::{ReflectionInput, Store};
use std::sync::{Arc, Mutex};

#[derive(Clone)]
struct TestClock(Arc<Mutex<ClockSample>>);
impl Clock for TestClock {
    fn sample(&self) -> ClockSample {
        self.0.lock().unwrap().clone()
    }
}

fn profile(theme: &str) -> DesktopProfile {
    serde_json::from_value(serde_json::json!({"schema":1,"preferences":{"rhythm.theme":theme},
        "backgrounds":{"schema":1,"focus":{"choice":"forest","custom":null},"break":{"choice":"dusk","custom":null}}})).unwrap()
}

#[test]
fn desktop_complete_restore_keeps_full_safety_copy_and_waits_in_break_before_profile_ack() {
    let directory = tempfile::tempdir().unwrap();
    let backup = directory.path().join("complete.sqlite3");
    let destination_path = directory.path().join("destination.sqlite3");
    let clock = TestClock(Arc::new(Mutex::new(ClockSample {
        epoch: "old".into(),
        monotonic_ms: 1000,
        utc_ms: 1_800_000_000_000,
    })));
    let mut source =
        DesktopService::open(directory.path().join("source.sqlite3"), clock.clone()).unwrap();
    source
        .execute(DesktopRequest::StartFocus {
            duration_ms: Some(60000),
        })
        .unwrap();
    let note = source.snapshot().unwrap().note_state.period_note.unwrap();
    source
        .execute(DesktopRequest::SavePeriodNote {
            id: note.id.clone(),
            version: note.version,
            body: "  简体\n繁體 🌿  ".into(),
        })
        .unwrap();
    {
        let mut sample = clock.0.lock().unwrap();
        sample.monotonic_ms += 1200;
        sample.utc_ms += 1200;
    }
    let active = source.poll().unwrap();
    let TimerState::Running(active) = active.checkpoint.state else {
        panic!("running");
    };
    let saved_profile = profile("light");
    source.backup_complete_to(&backup, &saved_profile).unwrap();
    let mut destination = DesktopService::open(&destination_path, clock).unwrap();
    let draft = destination
        .execute(DesktopRequest::CreateDraft {
            content: ReflectionInput::thought("current local writing"),
        })
        .unwrap()
        .draft
        .unwrap();
    let current_profile = profile("dark");
    let expected = destination.inspect_backup_profile(&backup).unwrap();
    let result = destination
        .restore_complete_from(&backup, &current_profile, &expected)
        .unwrap();
    assert_eq!(
        Store::inspect_backup_profile(&result.safety_backup).unwrap(),
        Some(current_profile)
    );
    let safety = Store::open(&result.safety_backup).unwrap();
    assert_eq!(safety.list_drafts().unwrap(), vec![draft]);
    let view = destination.snapshot().unwrap();
    assert!(matches!(
        view.checkpoint.state,
        TimerState::Ready {
            next: PeriodKind::Focus
        }
    ));
    let ended = view
        .sessions
        .iter()
        .find(|session| session.id == active.id)
        .unwrap();
    assert_eq!(ended.elapsed_ms, 1200);
    assert_eq!(ended.outcome, SessionOutcome::EndedEarly);
    assert!(ended.recovered);
    assert_eq!(ended.ended_utc_ms, None);
    assert_eq!(view.checkpoint.completed_focus, 0);
    assert!(view.routine_state.invitations.is_empty());
    let archived = view
        .note_state
        .archived_notes
        .iter()
        .find(|entry| entry.id == note.id)
        .unwrap();
    assert_eq!(archived.body, "  简体\n繁體 🌿  ");
    let pending = result.pending_profile.unwrap();
    assert_eq!(pending.profile, saved_profile);
    assert_eq!(
        destination.pending_desktop_profile().unwrap(),
        Some(pending.clone())
    );
    assert!(
        destination
            .acknowledge_desktop_profile_restore(&pending.token)
            .unwrap()
    );
    assert!(destination.pending_desktop_profile().unwrap().is_none());
}
