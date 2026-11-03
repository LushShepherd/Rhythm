//! Simulated native-clock observations through the real application and SQLite.
//! These tests do not suspend the host, wait for real deadlines, or change its clock.

use rhythm_application::Rhythm;
use rhythm_domain::{
    Clock, ClockSample, Command, PeriodKind, SessionOutcome, Settings, TimerState,
};
use rhythm_storage::Store;
use std::cell::RefCell;
use std::rc::Rc;

const MINUTE: u64 = 60_000;
const START_UTC: i64 = 1_700_000_000_000;

#[derive(Clone)]
struct SimulatedWindowsClock(Rc<RefCell<ClockSample>>);

impl SimulatedWindowsClock {
    fn new() -> Self {
        Self(Rc::new(RefCell::new(ClockSample {
            epoch: "process-one".into(),
            monotonic_ms: 10_000,
            utc_ms: START_UTC,
        })))
    }

    // Awake work advances both the native awake clock and wall time.
    fn elapse(&self, milliseconds: u64) {
        let mut sample = self.0.borrow_mut();
        sample.monotonic_ms += milliseconds;
        sample.utc_ms += i64::try_from(milliseconds).unwrap();
    }

    fn sleep(&self, milliseconds: u64) {
        let mut sample = self.0.borrow_mut();
        sample.utc_ms += i64::try_from(milliseconds).unwrap();
        sample.epoch.push_str(":interrupted");
    }
}

impl Clock for SimulatedWindowsClock {
    fn sample(&self) -> ClockSample {
        self.0.borrow().clone()
    }
}

fn start(store: Store, clock: &SimulatedWindowsClock) -> Rhythm<Store, SimulatedWindowsClock> {
    let mut app = Rhythm::open(store, clock.clone(), Settings::default()).unwrap();
    app.dispatch(Command::StartFocus {
        session_id: "focus-before-sleep".into(),
        duration_ms: None,
    })
    .unwrap();
    app
}

#[test]
fn sleep_ends_focus_at_verified_elapsed_and_waits_in_break() {
    let clock = SimulatedWindowsClock::new();
    let mut app = start(Store::open_in_memory().unwrap(), &clock);
    clock.elapse(5 * MINUTE);
    assert!(app.dispatch(Command::Poll).unwrap().is_none());
    clock.sleep(MINUTE);
    let session = app.dispatch(Command::Interrupt).unwrap().unwrap();
    assert_eq!(session.elapsed_ms, 5 * MINUTE);
    assert_eq!(session.outcome, SessionOutcome::EndedEarly);
    assert!(session.recovered);
    assert_eq!(session.ended_utc_ms, None);
    assert_eq!(
        app.state(),
        &TimerState::Ready {
            next: PeriodKind::Focus
        }
    );
    assert_eq!(app.checkpoint().completed_focus, 0);
    assert_eq!(app.repository().list_sessions(10, 0).unwrap().len(), 1);
}

#[test]
fn missing_callback_epoch_protects_a_late_wake_and_duplicates_do_not_restart() {
    let clock = SimulatedWindowsClock::new();
    let mut app = start(Store::open_in_memory().unwrap(), &clock);
    clock.elapse(2 * MINUTE);
    app.dispatch(Command::Poll).unwrap();
    // The paired clock detector changes epoch even without the OS callback.
    clock.sleep(40 * MINUTE);
    assert!(app.dispatch(Command::Poll).unwrap().is_none());
    assert!(matches!(app.state(), TimerState::RecoveryRequired(_)));
    let session = app.dispatch(Command::Interrupt).unwrap().unwrap();
    assert_eq!(session.elapsed_ms, 2 * MINUTE);
    assert_eq!(session.outcome, SessionOutcome::EndedEarly);
    assert_eq!(session.ended_utc_ms, None);
    assert_eq!(session.detected_utc_ms, START_UTC + (42 * MINUTE) as i64);
    assert_eq!(
        app.repository().get_session(&session.id).unwrap(),
        Some(session)
    );
    for _ in 0..4 {
        assert!(app.dispatch(Command::Interrupt).unwrap().is_none());
        assert!(app.dispatch(Command::Poll).unwrap().is_none());
    }
    assert_eq!(app.repository().list_sessions(10, 0).unwrap().len(), 1);
    assert_eq!(
        app.state(),
        &TimerState::Ready {
            next: PeriodKind::Focus
        }
    );
}

#[test]
fn a_paused_period_is_interrupted_instead_of_resumed_after_sleep() {
    let clock = SimulatedWindowsClock::new();
    let mut app = start(Store::open_in_memory().unwrap(), &clock);
    clock.elapse(3 * MINUTE);
    app.dispatch(Command::Pause).unwrap();
    clock.sleep(40 * MINUTE);
    let session = app.dispatch(Command::Interrupt).unwrap().unwrap();
    assert_eq!(session.elapsed_ms, 3 * MINUTE);
    assert_eq!(session.intervals.len(), 1);
    assert_eq!(session.intervals[0].elapsed_ms, 3 * MINUTE);
    assert_eq!(
        app.state(),
        &TimerState::Ready {
            next: PeriodKind::Focus
        }
    );
    assert!(app.dispatch(Command::Resume).is_err());
}

#[test]
fn wall_clock_correction_and_awake_delay_are_not_sleep() {
    let clock = SimulatedWindowsClock::new();
    let mut app = start(Store::open_in_memory().unwrap(), &clock);
    clock.elapse(5 * MINUTE);
    clock.0.borrow_mut().utc_ms += (120 * MINUTE) as i64;
    app.dispatch(Command::Poll).unwrap();
    let TimerState::Running(active) = app.state() else {
        panic!("period must remain running")
    };
    assert_eq!(active.elapsed_ms, 5 * MINUTE);
    clock.elapse(20 * MINUTE);
    let session = app.dispatch(Command::Poll).unwrap().unwrap();
    assert_eq!(session.elapsed_ms, 25 * MINUTE);
    assert_eq!(session.ended_utc_ms, None);
    assert!(!session.intervals[0].mapping_certain);
}

#[test]
fn desktop_startup_ends_recovery_without_crediting_closed_app_time() {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("recovery.sqlite3");
    let clock = SimulatedWindowsClock::new();
    let mut app = start(Store::open(&database).unwrap(), &clock);
    clock.elapse(3 * MINUTE);
    app.dispatch(Command::Poll).unwrap();
    drop(app);
    // Even a plausible same-boot tick count is insufficient: this adapter does
    // not expose a verified boot identifier, so every new process changes epoch.
    clock.sleep(120 * MINUTE);
    clock.0.borrow_mut().epoch = "new-process".into();
    let mut reopened = Rhythm::open(
        Store::open(&database).unwrap(),
        clock.clone(),
        Settings::default(),
    )
    .unwrap();
    let TimerState::RecoveryRequired(active) = reopened.state() else {
        panic!("restart must require a recovery choice")
    };
    assert_eq!(active.elapsed_ms, 3 * MINUTE);
    assert!(
        reopened
            .repository()
            .list_sessions(10, 0)
            .unwrap()
            .is_empty()
    );
    let session = reopened.dispatch(Command::Interrupt).unwrap().unwrap();
    assert_eq!(session.elapsed_ms, 3 * MINUTE);
    assert!(session.recovered);
    assert_eq!(session.outcome, SessionOutcome::EndedEarly);
    assert_eq!(
        reopened.state(),
        &TimerState::Ready {
            next: PeriodKind::Focus
        }
    );
    assert_eq!(
        session
            .intervals
            .iter()
            .map(|span| span.elapsed_ms)
            .sum::<u64>(),
        3 * MINUTE
    );
    assert!(reopened.dispatch(Command::Poll).unwrap().is_none());
    reopened
        .dispatch(Command::StartFocus {
            session_id: "deliberate-new-focus".into(),
            duration_ms: None,
        })
        .unwrap();
    let TimerState::Running(active) = reopened.state() else {
        panic!("explicit new Focus should start")
    };
    assert_eq!(active.id, "deliberate-new-focus");
    assert_eq!(active.elapsed_ms, 0);
}
