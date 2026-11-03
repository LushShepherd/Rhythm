use rhythm_application::{ApplicationError, PersistedTimer, Rhythm, TimerRepository};
use rhythm_domain::{
    Checkpoint, Clock, ClockSample, Command, PeriodKind, SessionRecord, Settings, TimerState,
};
use std::cell::{Cell, RefCell};
use std::rc::Rc;

#[derive(Clone)]
struct ManualClock(Rc<RefCell<ClockSample>>);

impl ManualClock {
    fn new() -> Self {
        Self(Rc::new(RefCell::new(ClockSample {
            epoch: "first-process".into(),
            monotonic_ms: 0,
            utc_ms: 10_000,
        })))
    }

    fn advance(&self, milliseconds: u64) {
        let mut sample = self.0.borrow_mut();
        sample.monotonic_ms += milliseconds;
        sample.utc_ms += i64::try_from(milliseconds).unwrap();
    }
}

impl Clock for ManualClock {
    fn sample(&self) -> ClockSample {
        self.0.borrow().clone()
    }
}

#[derive(Debug, thiserror::Error)]
#[error("injected storage failure")]
struct SaveError;

#[derive(Default)]
struct Repository {
    saved: Option<PersistedTimer>,
    sessions: Vec<SessionRecord>,
    fail: Rc<Cell<bool>>,
    writes: usize,
}

impl TimerRepository for Repository {
    type Error = SaveError;

    fn load_timer(&self) -> Result<Option<PersistedTimer>, Self::Error> {
        Ok(self.saved.clone())
    }

    fn save_timer(
        &mut self,
        expected_revision: Option<u64>,
        checkpoint: &Checkpoint,
        terminal: Option<&SessionRecord>,
    ) -> Result<u64, Self::Error> {
        if self.fail.get() || self.saved.as_ref().map(|saved| saved.revision) != expected_revision {
            return Err(SaveError);
        }
        let revision = expected_revision.unwrap_or(0) + 1;
        self.saved = Some(PersistedTimer {
            revision,
            checkpoint: checkpoint.clone(),
        });
        self.sessions.extend(terminal.cloned());
        self.writes += 1;
        Ok(revision)
    }
}

fn start() -> Command {
    Command::StartFocus {
        session_id: "focus-a".into(),
        duration_ms: Some(1_000),
    }
}

#[test]
fn interrupted_period_is_committed_once_at_last_verified_elapsed_after_save_retry() {
    let clock = ManualClock::new();
    let repository = Repository::default();
    let fail = repository.fail.clone();
    let mut app = Rhythm::open(repository, clock.clone(), Settings::default()).unwrap();
    app.dispatch(start()).unwrap();
    clock.advance(250);
    app.dispatch(Command::Poll).unwrap();
    let before = app.checkpoint();
    clock.advance(10_000);
    fail.set(true);
    assert!(app.dispatch(Command::Interrupt).is_err());
    assert_eq!(app.checkpoint(), before);
    assert!(app.repository().sessions.is_empty());
    fail.set(false);
    let terminal = app.dispatch(Command::Interrupt).unwrap().unwrap();
    assert_eq!(terminal.elapsed_ms, 250);
    assert_eq!(terminal.outcome, rhythm_domain::SessionOutcome::EndedEarly);
    assert_eq!(terminal.ended_utc_ms, None);
    assert_eq!(
        app.state(),
        &TimerState::Ready {
            next: PeriodKind::Focus
        }
    );
    assert!(app.dispatch(Command::Interrupt).unwrap().is_none());
    assert_eq!(app.repository().sessions.len(), 1);
}

#[test]
fn failed_start_never_becomes_visible_or_durable() {
    let repository = Repository::default();
    let fail = repository.fail.clone();
    let mut app = Rhythm::open(repository, ManualClock::new(), Settings::default()).unwrap();
    let initial = app.checkpoint();
    fail.set(true);
    assert!(matches!(
        app.dispatch(start()),
        Err(ApplicationError::Repository(_))
    ));
    assert_eq!(app.checkpoint(), initial);
    assert_eq!(app.repository().saved.as_ref().unwrap().checkpoint, initial);
    assert_eq!(app.repository().writes, 1);
}

#[test]
fn failed_direct_switch_keeps_old_state_and_retries_as_one_commit() {
    let clock = ManualClock::new();
    let repository = Repository::default();
    let fail = repository.fail.clone();
    let mut app = Rhythm::open(repository, clock.clone(), Settings::default()).unwrap();
    app.dispatch(start()).unwrap();
    clock.advance(250);
    let before = app.checkpoint();
    let writes = app.repository().writes;
    let switch = Command::SwitchToBreak {
        session_id: "break".into(),
        duration_ms: None,
    };
    fail.set(true);
    assert!(matches!(
        app.dispatch(switch.clone()),
        Err(ApplicationError::Repository(_))
    ));
    assert_eq!(app.checkpoint(), before);
    assert_eq!(app.repository().saved.as_ref().unwrap().checkpoint, before);
    assert!(app.repository().sessions.is_empty());
    fail.set(false);
    let terminal = app.dispatch(switch.clone()).unwrap().unwrap();
    assert_eq!(terminal.elapsed_ms, 250);
    assert_eq!(app.repository().writes, writes + 1);
    assert_eq!(app.repository().sessions, vec![terminal]);
    assert!(
        matches!(app.state(), TimerState::Running(active) if active.kind == PeriodKind::ShortRest)
    );
    assert_eq!(
        app.repository().saved.as_ref().unwrap().checkpoint,
        app.checkpoint()
    );
    assert!(app.dispatch(switch).unwrap().is_none());
    assert_eq!(app.repository().writes, writes + 1);
}

#[test]
fn failed_completion_can_be_retried_without_duplicate_history() {
    let clock = ManualClock::new();
    let repository = Repository::default();
    let fail = repository.fail.clone();
    let mut app = Rhythm::open(repository, clock.clone(), Settings::default()).unwrap();
    app.dispatch(start()).unwrap();
    let running = app.checkpoint();
    clock.advance(8_000);
    fail.set(true);
    assert!(app.dispatch(Command::Poll).is_err());
    assert_eq!(app.checkpoint(), running);
    assert!(app.repository().sessions.is_empty());
    fail.set(false);
    let completed = app.dispatch(Command::Poll).unwrap().unwrap();
    assert_eq!(completed.elapsed_ms, 1_000);
    assert_eq!(completed.ended_utc_ms, Some(11_000));
    assert_eq!(completed.detected_utc_ms, 18_000);
    assert_eq!(
        app.state(),
        &TimerState::Ready {
            next: PeriodKind::ShortRest
        }
    );
    let committed_writes = app.repository().writes;
    for _ in 0..5 {
        assert!(app.dispatch(Command::Poll).unwrap().is_none());
    }
    assert_eq!(app.repository().writes, committed_writes);
    assert_eq!(app.repository().sessions.len(), 1);
}

#[test]
fn stale_application_cannot_publish_an_uncommitted_transition() {
    let mut app = Rhythm::open(
        Repository::default(),
        ManualClock::new(),
        Settings::default(),
    )
    .unwrap();
    let before = app.checkpoint();
    // Another connection has committed between this service's load and write.
    app.repository_mut().saved.as_mut().unwrap().revision += 1;
    assert!(matches!(
        app.dispatch(start()),
        Err(ApplicationError::Repository(_))
    ));
    assert_eq!(app.checkpoint(), before);
}

#[test]
fn stale_direct_switch_cannot_overwrite_a_newer_checkpoint_or_append_history() {
    let clock = ManualClock::new();
    let mut app = Rhythm::open(Repository::default(), clock.clone(), Settings::default()).unwrap();
    app.dispatch(start()).unwrap();
    let before = app.checkpoint();
    clock.advance(250);
    app.repository_mut().saved.as_mut().unwrap().revision += 1;
    assert!(matches!(
        app.dispatch(Command::SwitchToBreak {
            session_id: "break".into(),
            duration_ms: None,
        }),
        Err(ApplicationError::Repository(_))
    ));
    assert_eq!(app.checkpoint(), before);
    assert_eq!(app.repository().saved.as_ref().unwrap().checkpoint, before);
    assert!(app.repository().sessions.is_empty());
}

#[test]
fn restart_with_a_new_clock_epoch_persists_recovery_without_completion() {
    let clock = ManualClock::new();
    let mut original =
        Rhythm::open(Repository::default(), clock.clone(), Settings::default()).unwrap();
    original.dispatch(start()).unwrap();
    clock.advance(400);
    original.dispatch(Command::Poll).unwrap();
    let repository = Repository {
        saved: original.repository().saved.clone(),
        ..Repository::default()
    };
    let new_clock = ManualClock::new();
    new_clock.0.borrow_mut().epoch = "new-process".into();
    new_clock.0.borrow_mut().utc_ms = 1_000_000;
    let restored = Rhythm::open(repository, new_clock, Settings::default()).unwrap();
    match restored.state() {
        TimerState::RecoveryRequired(active) => assert_eq!(active.elapsed_ms, 400),
        state => panic!("expected recovery, got {state:?}"),
    }
    assert_eq!(
        restored.repository().saved.as_ref().unwrap().checkpoint,
        restored.checkpoint()
    );
    assert!(restored.repository().sessions.is_empty());
}

#[test]
fn failed_recovery_save_preserves_the_original_checkpoint() {
    let clock = ManualClock::new();
    let mut original =
        Rhythm::open(Repository::default(), clock.clone(), Settings::default()).unwrap();
    original.dispatch(start()).unwrap();
    let repository = Repository {
        saved: original.repository().saved.clone(),
        fail: Rc::new(Cell::new(true)),
        ..Repository::default()
    };
    clock.0.borrow_mut().epoch = "another-process".into();
    assert!(matches!(
        Rhythm::open(repository, clock, Settings::default()),
        Err(ApplicationError::Repository(_))
    ));
    assert!(matches!(
        original
            .repository()
            .saved
            .as_ref()
            .unwrap()
            .checkpoint
            .state,
        TimerState::Running(_)
    ));
}
