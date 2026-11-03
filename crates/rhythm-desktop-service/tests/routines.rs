use rhythm_desktop_service::{DesktopRequest as Request, DesktopService, ServiceError};
use rhythm_domain::{
    Clock, ClockSample, Invitation, PeriodKind, RoutineInput, RoutineWindow, TimerState,
};
use std::sync::{Arc, Mutex};

const MONDAY_9AM: i64 = 1_793_610_000_000;
#[derive(Clone)]
struct TestClock(Arc<Mutex<ClockSample>>);
impl TestClock {
    fn new() -> Self {
        Self(Arc::new(Mutex::new(ClockSample {
            epoch: "test".into(),
            monotonic_ms: 0,
            utc_ms: MONDAY_9AM - 60_000,
        })))
    }
    fn advance(&self, ms: u64) {
        let mut sample = self.0.lock().unwrap();
        sample.monotonic_ms += ms;
        sample.utc_ms += ms as i64;
    }
}
impl Clock for TestClock {
    fn sample(&self) -> ClockSample {
        self.0.lock().unwrap().clone()
    }
}
fn routine(name: &str) -> RoutineInput {
    RoutineInput {
        name: name.into(),
        weekdays: vec![1, 2, 3, 4, 5],
        local_time: Some("09:00".into()),
        windows: vec![],
        timezone: "UTC".into(),
        focus_ms: 60_000,
        enabled: true,
    }
}
fn add(service: &mut DesktopService<TestClock>, name: &str) {
    service
        .execute(Request::SaveRoutine {
            id: None,
            routine: routine(name),
        })
        .unwrap();
}

#[test]
fn simultaneous_routines_wait_for_one_explicit_start_with_a_one_time_preset() {
    let directory = tempfile::tempdir().unwrap();
    let clock = TestClock::new();
    let mut service =
        DesktopService::open(directory.path().join("journal.sqlite3"), clock.clone()).unwrap();
    add(&mut service, "First");
    add(&mut service, "Second");
    assert!(
        service
            .snapshot()
            .unwrap()
            .routine_state
            .invitations
            .is_empty()
    );
    clock.advance(60_000);
    let state = service.poll_routines();
    assert_eq!(state.invitations.len(), 2);
    assert!(service.ready_for_invitation());
    assert_eq!(service.poll_routines(), state);
    let invite = &state.invitations[0];
    let result = service
        .execute(Request::AcceptInvitation {
            routine_id: invite.routine_id.clone(),
            due_at_utc_ms: invite.due_at_utc_ms,
        })
        .unwrap();
    assert!(
        matches!(result.view.checkpoint.state,TimerState::Running(ref active) if active.planned_ms == 60_000)
    );
    assert_eq!(result.view.checkpoint.settings.focus_ms, 1_500_000);
    assert!(result.view.routine_state.invitations.is_empty());
    service.execute(Request::EndEarly).unwrap();
    assert!(matches!(
        service.execute(Request::AcceptInvitation {
            routine_id: invite.routine_id.clone(),
            due_at_utc_ms: invite.due_at_utc_ms
        }),
        Err(ServiceError::InvitationUnavailable)
    ));
}

#[test]
fn active_period_is_preserved_and_expired_invitations_cannot_start() {
    let directory = tempfile::tempdir().unwrap();
    let clock = TestClock::new();
    let mut service =
        DesktopService::open(directory.path().join("journal.sqlite3"), clock.clone()).unwrap();
    add(&mut service, "Morning");
    service
        .execute(Request::StartFocus {
            duration_ms: Some(3_600_000),
        })
        .unwrap();
    let before = service.snapshot().unwrap().checkpoint;
    clock.advance(60_000);
    let invite = service.poll_routines().invitations[0].clone();
    assert_eq!(service.snapshot().unwrap().checkpoint, before);
    assert!(matches!(
        service.execute(Request::AcceptInvitation {
            routine_id: invite.routine_id.clone(),
            due_at_utc_ms: invite.due_at_utc_ms
        }),
        Err(ServiceError::InvitationWhileActive)
    ));
    clock.advance(900_000);
    assert!(service.poll_routines().invitations.is_empty());
    service.execute(Request::EndEarly).unwrap();
    assert!(matches!(
        service.execute(Request::AcceptInvitation {
            routine_id: invite.routine_id,
            due_at_utc_ms: invite.due_at_utc_ms
        }),
        Err(ServiceError::InvitationUnavailable)
    ));
}

#[test]
fn dismiss_and_restart_do_not_replay_delivered_invitations() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("journal.sqlite3");
    let clock = TestClock::new();
    let mut service = DesktopService::open(&path, clock.clone()).unwrap();
    add(&mut service, "Morning");
    clock.advance(60_000);
    let invite = service.poll_routines().invitations[0].clone();
    service
        .execute(Request::DismissInvitation {
            routine_id: invite.routine_id,
            due_at_utc_ms: invite.due_at_utc_ms,
        })
        .unwrap();
    assert!(service.poll_routines().invitations.is_empty());
    drop(service);
    let reopened = DesktopService::open(&path, clock).unwrap();
    assert!(
        reopened
            .snapshot()
            .unwrap()
            .routine_state
            .invitations
            .is_empty()
    );
    assert_eq!(reopened.snapshot().unwrap().routine_state.routines.len(), 1);
}

#[test]
fn routine_edits_disable_pending_invitation_without_changing_current_period() {
    let directory = tempfile::tempdir().unwrap();
    let clock = TestClock::new();
    let mut service =
        DesktopService::open(directory.path().join("journal.sqlite3"), clock.clone()).unwrap();
    add(&mut service, "Morning");
    clock.advance(60_000);
    let invite = service.poll_routines().invitations[0].clone();
    service
        .execute(Request::StartFocus { duration_ms: None })
        .unwrap();
    let before = service.snapshot().unwrap().checkpoint;
    let mut changed = routine("Updated");
    changed.enabled = false;
    changed.focus_ms = 120_000;
    let response = service
        .execute(Request::SaveRoutine {
            id: Some(invite.routine_id.clone()),
            routine: changed,
        })
        .unwrap();
    assert_eq!(response.view.checkpoint, before);
    assert!(response.view.routine_state.invitations.is_empty());
    assert_eq!(
        response.view.routine_state.routine_next[0].next_utc_ms,
        None
    );
    service
        .execute(Request::DeleteRoutine {
            id: invite.routine_id,
        })
        .unwrap();
    assert!(
        service
            .snapshot()
            .unwrap()
            .routine_state
            .routines
            .is_empty()
    );
}

#[test]
fn invalid_routine_is_rejected_without_publishing_it() {
    let directory = tempfile::tempdir().unwrap();
    let clock = TestClock::new();
    let mut service =
        DesktopService::open(directory.path().join("journal.sqlite3"), clock).unwrap();
    let before = service.snapshot().unwrap();
    let mut invalid = routine("Morning");
    invalid.timezone = "Invalid/Zone".into();
    assert!(
        service
            .execute(Request::SaveRoutine {
                id: None,
                routine: invalid
            })
            .is_err()
    );
    assert_eq!(service.snapshot().unwrap(), before);
}

#[test]
fn timezone_failure_leaves_routines_editable_and_timer_usable() {
    let directory = tempfile::tempdir().unwrap();
    let mut service =
        DesktopService::open(directory.path().join("journal.sqlite3"), TestClock::new()).unwrap();
    service.set_system_timezone("UTC".into());
    let mut input = routine("Follow device");
    input.timezone = "system".into();
    service
        .execute(Request::SaveRoutine {
            id: None,
            routine: input,
        })
        .unwrap();
    service.set_system_timezone("Invalid/Device".into());
    let state = service.routine_view();
    assert_eq!(state.routines.len(), 1);
    assert!(state.routine_error.is_some());
    assert!(state.routine_next.is_empty());
    service
        .execute(Request::StartFocus { duration_ms: None })
        .unwrap();
    service.execute(Request::EndEarly).unwrap();
    service
        .execute(Request::SaveRoutine {
            id: Some(state.routines[0].id.clone()),
            routine: routine("Fixed zone"),
        })
        .unwrap();
    assert!(service.poll_routines().routine_error.is_none());
}

#[test]
fn device_zone_change_invalidates_an_old_pending_offer() {
    let directory = tempfile::tempdir().unwrap();
    let clock = TestClock::new();
    let mut service =
        DesktopService::open(directory.path().join("journal.sqlite3"), clock.clone()).unwrap();
    service.set_system_timezone("UTC".into());
    let mut input = routine("Follow device");
    input.timezone = "system".into();
    service
        .execute(Request::SaveRoutine {
            id: None,
            routine: input,
        })
        .unwrap();
    clock.advance(60_000);
    let old = service.poll_routines().invitations[0].clone();
    service.set_system_timezone("America/New_York".into());
    assert!(service.routine_view().invitations.is_empty());
    assert!(matches!(
        service.execute(Request::AcceptInvitation {
            routine_id: old.routine_id,
            due_at_utc_ms: old.due_at_utc_ms
        }),
        Err(ServiceError::InvitationUnavailable)
    ));
    assert!(service.ready_for_invitation());
}

fn daily_windows() -> RoutineInput {
    RoutineInput {
        local_time: None,
        windows: vec![
            RoutineWindow {
                start: "08:30".into(),
                end: "12:00".into(),
            },
            RoutineWindow {
                start: "14:30".into(),
                end: "18:00".into(),
            },
        ],
        ..routine("Weekday rhythm")
    }
}

fn add_windows(service: &mut DesktopService<TestClock>, input: RoutineInput) -> Invitation {
    service
        .execute(Request::SaveRoutine {
            id: None,
            routine: input,
        })
        .unwrap()
        .view
        .routine_state
        .routine_windows[0]
        .clone()
}

fn accept(window: &Invitation) -> Request {
    Request::AcceptInvitation {
        routine_id: window.routine_id.clone(),
        due_at_utc_ms: window.due_at_utc_ms,
    }
}

#[test]
fn daily_window_supports_explicit_focus_and_short_long_rest_cycles() {
    let directory = tempfile::tempdir().unwrap();
    let clock = TestClock::new();
    let mut service =
        DesktopService::open(directory.path().join("journal.sqlite3"), clock.clone()).unwrap();
    service
        .execute(Request::CreateDraft {
            content: rhythm_storage::ReflectionInput::thought("Keep this journal · 简体 · 繁體"),
        })
        .unwrap();
    let before = service.snapshot().unwrap();
    let window = add_windows(&mut service, daily_windows());
    assert_eq!(window.due_at_utc_ms, MONDAY_9AM - 30 * 60_000);
    assert_eq!(window.expires_at_utc_ms, MONDAY_9AM + 3 * 3_600_000);
    assert!(service.ready_for_invitation());
    for cycle in 0..4 {
        let started = service
            .execute(if cycle == 0 {
                accept(&window)
            } else {
                Request::StartFocus { duration_ms: None }
            })
            .unwrap();
        assert!(matches!(
            started.view.checkpoint.state,
            TimerState::Running(ref active)
                if active.kind == PeriodKind::Focus && active.planned_ms == 60_000
        ));
        assert_eq!(
            started.view.routine_state.routine_windows,
            vec![window.clone()]
        );
        assert_eq!(
            started.view.routine_state.selected_routine_window,
            Some(window.clone())
        );
        assert!(started.view.routine_state.invitations.is_empty());
        clock.advance(60_000);
        let rest_kind = if cycle < 3 {
            PeriodKind::ShortRest
        } else {
            PeriodKind::LongRest
        };
        let completed = service.poll().unwrap();
        assert_eq!(
            completed.checkpoint.state,
            TimerState::Ready { next: rest_kind }
        );
        assert!(service.poll().unwrap().terminal.is_none());
        let rest = service
            .execute(Request::StartRest { duration_ms: None })
            .unwrap();
        let expected_duration = if cycle < 3 { 300_000 } else { 1_800_000 };
        assert!(matches!(
            rest.view.checkpoint.state,
            TimerState::Running(ref active)
                if active.kind == rest_kind && active.planned_ms == expected_duration
        ));
        clock.advance(expected_duration);
        assert_eq!(
            service.poll().unwrap().checkpoint.state,
            TimerState::Ready {
                next: PeriodKind::Focus
            }
        );
    }
    let after = service.snapshot().unwrap();
    assert_eq!(after.drafts, before.drafts);
    assert_eq!(after.reflections, before.reflections);
    assert_eq!(after.sessions.len(), 8);
    assert_eq!(after.checkpoint.settings, before.checkpoint.settings);
}

#[test]
fn ending_a_window_does_not_cut_the_running_period_or_change_saved_defaults() {
    let directory = tempfile::tempdir().unwrap();
    let clock = TestClock::new();
    let mut service =
        DesktopService::open(directory.path().join("journal.sqlite3"), clock.clone()).unwrap();
    clock.advance(3 * 3_600_000); // 11:59, just before the morning window closes.
    let mut input = daily_windows();
    input.focus_ms = 30 * 60_000;
    let window = add_windows(&mut service, input);
    let started = service.execute(accept(&window)).unwrap();
    clock.advance(60_000); // Exactly 12:00 is outside the window.
    let state = service.poll_routines();
    assert!(state.routine_windows.is_empty());
    assert!(state.invitations.is_empty());
    assert!(state.selected_routine_window.is_none());
    assert_eq!(
        service.snapshot().unwrap().checkpoint,
        started.view.checkpoint
    );
    assert!(service.poll().unwrap().terminal.is_none());
    assert!(matches!(
        service.execute(accept(&window)),
        Err(ServiceError::InvitationWhileActive)
    ));
    clock.advance(29 * 60_000);
    let finished = service.poll().unwrap();
    assert_eq!(finished.terminal.unwrap().elapsed_ms, 30 * 60_000);
    assert!(matches!(
        service.execute(accept(&window)),
        Err(ServiceError::InvitationUnavailable)
    ));
    let ordinary = service
        .execute(Request::StartFocus { duration_ms: None })
        .unwrap();
    assert!(matches!(
        ordinary.view.checkpoint.state,
        TimerState::Running(ref active) if active.planned_ms == 1_500_000
    ));
    assert_eq!(ordinary.view.checkpoint.settings.focus_ms, 1_500_000);
}

#[test]
fn dismissing_morning_is_durable_and_does_not_suppress_afternoon() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("journal.sqlite3");
    let clock = TestClock::new();
    let mut service = DesktopService::open(&path, clock.clone()).unwrap();
    let morning = add_windows(&mut service, daily_windows());
    service.execute(accept(&morning)).unwrap();
    let running = service.snapshot().unwrap().checkpoint;
    let dismissed = service
        .execute(Request::DismissInvitation {
            routine_id: morning.routine_id.clone(),
            due_at_utc_ms: morning.due_at_utc_ms,
        })
        .unwrap();
    assert_eq!(dismissed.view.checkpoint, running);
    assert!(dismissed.view.routine_state.routine_windows.is_empty());
    assert!(
        dismissed
            .view
            .routine_state
            .selected_routine_window
            .is_none()
    );
    service.execute(Request::EndEarly).unwrap();
    drop(service);
    let mut service = DesktopService::open(&path, clock.clone()).unwrap();
    assert!(service.routine_view().routine_windows.is_empty());
    assert!(service.routine_view().invitations.is_empty());
    clock.advance(5 * 3_600_000 + 31 * 60_000); // 14:30.
    let afternoon = service.poll_routines();
    assert!(service.ready_for_invitation());
    assert_eq!(afternoon.routine_windows.len(), 1);
    assert_eq!(afternoon.invitations, afternoon.routine_windows);
    assert_eq!(afternoon.routine_windows[0].window_key, "14:30");
    assert_eq!(afternoon.routine_windows[0].routine_id, morning.routine_id);
    service
        .execute(accept(&afternoon.routine_windows[0]))
        .unwrap();
}

#[test]
fn reopening_a_delivered_window_restores_context_without_replaying_notification() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("journal.sqlite3");
    let clock = TestClock::new();
    let mut service = DesktopService::open(&path, clock.clone()).unwrap();
    let window = add_windows(&mut service, daily_windows());
    assert_eq!(service.routine_view().invitations, vec![window.clone()]);
    service.execute(accept(&window)).unwrap();
    service.execute(Request::EndEarly).unwrap();
    drop(service);
    let mut service = DesktopService::open(&path, clock).unwrap();
    let state = service.routine_view();
    assert!(state.invitations.is_empty());
    assert_eq!(state.routine_windows, vec![window.clone()]);
    assert!(state.selected_routine_window.is_none());
    assert!(service.ready_for_invitation());
    assert!(service.snapshot().unwrap().sessions.len() == 1);
    service.execute(accept(&window)).unwrap();
    assert_eq!(service.routine_view().selected_routine_window, Some(window));
}

#[test]
fn selected_window_respects_one_time_focus_override_and_explicit_reacceptance() {
    let directory = tempfile::tempdir().unwrap();
    let clock = TestClock::new();
    let mut service =
        DesktopService::open(directory.path().join("journal.sqlite3"), clock).unwrap();
    let window = add_windows(&mut service, daily_windows());
    service.execute(accept(&window)).unwrap();
    service.execute(Request::EndEarly).unwrap();
    let overridden = service
        .execute(Request::StartFocus {
            duration_ms: Some(120_000),
        })
        .unwrap();
    assert!(matches!(overridden.view.checkpoint.state,
        TimerState::Running(ref active) if active.planned_ms == 120_000));
    service.execute(Request::EndEarly).unwrap();
    let continued = service.execute(accept(&window)).unwrap();
    assert!(matches!(continued.view.checkpoint.state,
        TimerState::Running(ref active) if active.planned_ms == 60_000));
}

#[test]
fn editing_or_deleting_selected_window_preserves_current_period_and_clears_selection() {
    let directory = tempfile::tempdir().unwrap();
    let clock = TestClock::new();
    let mut service =
        DesktopService::open(directory.path().join("journal.sqlite3"), clock).unwrap();
    let window = add_windows(&mut service, daily_windows());
    service.execute(accept(&window)).unwrap();
    let before = service.snapshot().unwrap().checkpoint;
    let mut updated = daily_windows();
    updated.focus_ms = 120_000;
    let saved = service
        .execute(Request::SaveRoutine {
            id: Some(window.routine_id.clone()),
            routine: updated,
        })
        .unwrap();
    assert_eq!(saved.view.checkpoint, before);
    assert!(saved.view.routine_state.selected_routine_window.is_none());
    assert_eq!(
        saved.view.routine_state.routine_windows[0].focus_ms,
        120_000
    );
    service.execute(Request::EndEarly).unwrap();
    let new_window = service.routine_view().routine_windows[0].clone();
    service.execute(accept(&new_window)).unwrap();
    let running = service.snapshot().unwrap().checkpoint;
    let deleted = service
        .execute(Request::DeleteRoutine {
            id: window.routine_id,
        })
        .unwrap();
    assert_eq!(deleted.view.checkpoint, running);
    assert!(deleted.view.routine_state.selected_routine_window.is_none());
    assert!(deleted.view.routine_state.routine_windows.is_empty());
}

#[test]
fn changing_device_zone_clears_selected_window_without_touching_timer() {
    let directory = tempfile::tempdir().unwrap();
    let clock = TestClock::new();
    let mut service =
        DesktopService::open(directory.path().join("journal.sqlite3"), clock).unwrap();
    service.set_system_timezone("UTC".into());
    let mut input = daily_windows();
    input.timezone = "system".into();
    let window = add_windows(&mut service, input);
    service.execute(accept(&window)).unwrap();
    let before = service.snapshot().unwrap().checkpoint;
    service.set_system_timezone("America/New_York".into());
    assert!(service.routine_view().routine_windows.is_empty());
    assert!(service.routine_view().selected_routine_window.is_none());
    assert_eq!(service.snapshot().unwrap().checkpoint, before);
}
