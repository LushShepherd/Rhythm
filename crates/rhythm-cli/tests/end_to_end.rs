use rhythm_application::{ApplicationError, Rhythm};
use rhythm_domain::{
    Clock, ClockSample, Command, PeriodKind, RecoveryChoice, SessionOutcome, Settings, TimerState,
};
use rhythm_storage::{ReflectionInput, Store};
use std::cell::RefCell;
use std::rc::Rc;

#[derive(Clone)]
struct ManualClock(Rc<RefCell<ClockSample>>);

impl ManualClock {
    fn new(epoch: &str, utc_ms: i64) -> Self {
        Self(Rc::new(RefCell::new(ClockSample {
            epoch: epoch.into(),
            monotonic_ms: 0,
            utc_ms,
        })))
    }

    fn advance(&self, ms: u64) {
        let mut sample = self.0.borrow_mut();
        sample.monotonic_ms += ms;
        sample.utc_ms += i64::try_from(ms).unwrap();
    }
}

impl Clock for ManualClock {
    fn sample(&self) -> ClockSample {
        self.0.borrow().clone()
    }
}

fn focus(id: &str) -> Command {
    Command::StartFocus {
        session_id: id.into(),
        duration_ms: Some(1_000),
    }
}

#[test]
fn session_and_multiline_journal_survive_reopen_without_a_second_completion() {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("rhythm.sqlite3");
    let clock = ManualClock::new("boot-a", 10_000);
    let mut app = Rhythm::open(
        Store::open(&database).unwrap(),
        clock.clone(),
        Settings::default(),
    )
    .unwrap();
    app.dispatch(focus("session-a")).unwrap();
    let text = "  Dear future me,\n慢慢来。\n慢慢來。\n";
    let draft = app
        .repository_mut()
        .create_draft(
            ReflectionInput {
                session_id: Some("session-a".into()),
                ..ReflectionInput::thought(text)
            },
            10_100,
        )
        .unwrap();
    clock.advance(1_500);
    let completed = app.dispatch(Command::Poll).unwrap().unwrap();
    assert_eq!(completed.elapsed_ms, 1_000);
    assert_eq!(completed.ended_utc_ms, Some(11_000));
    assert_eq!(completed.detected_utc_ms, 11_500);
    let first = app
        .repository_mut()
        .finalize_draft(&draft.id, draft.version, 11_500)
        .unwrap();
    let edit = app
        .repository_mut()
        .edit_reflection(&first.record_id, 12_000)
        .unwrap();
    let edited_text = format!("{text}A little space helped.");
    let edit = app
        .repository_mut()
        .save_draft(&edit.id, edit.version, &edited_text, 12_010)
        .unwrap();
    let second = app
        .repository_mut()
        .finalize_draft(&edit.id, edit.version, 12_020)
        .unwrap();
    assert_eq!(second.parents, vec![first.id.clone()]);
    drop(app);

    let mut reopened = Rhythm::open(
        Store::open(&database).unwrap(),
        ManualClock::new("boot-b", 100_000),
        Settings::default(),
    )
    .unwrap();
    assert_eq!(
        reopened.state(),
        &TimerState::Ready {
            next: PeriodKind::ShortRest
        }
    );
    assert!(reopened.dispatch(Command::Poll).unwrap().is_none());
    assert_eq!(
        reopened.repository().list_sessions(100, 0).unwrap(),
        vec![completed]
    );
    let reflection = reopened
        .repository()
        .get_reflection(&first.record_id)
        .unwrap()
        .unwrap();
    assert_eq!(reflection.heads[0].content.body, edited_text);
    let history = reopened
        .repository()
        .revision_history(&first.record_id)
        .unwrap();
    assert_eq!(history.len(), 2);
    assert_eq!(history[0].content.body, text);
    assert!(reopened.repository().list_drafts().unwrap().is_empty());
}

#[test]
fn interrupted_running_period_keeps_verified_time_and_unsaved_draft() {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("rhythm.sqlite3");
    let clock = ManualClock::new("process-a", 10_000);
    let mut app = Rhythm::open(
        Store::open(&database).unwrap(),
        clock.clone(),
        Settings::default(),
    )
    .unwrap();
    app.dispatch(focus("recover-me")).unwrap();
    clock.advance(400);
    app.dispatch(Command::Poll).unwrap();
    let draft = app
        .repository_mut()
        .create_draft(ReflectionInput::thought("still writing… 留给自己"), 10_400)
        .unwrap();
    drop(app);

    let after_restart = ManualClock::new("process-b", 9_000_000);
    let mut app = Rhythm::open(
        Store::open(&database).unwrap(),
        after_restart.clone(),
        Settings::default(),
    )
    .unwrap();
    match app.state() {
        TimerState::RecoveryRequired(active) => assert_eq!(active.remaining_ms(), 600),
        state => panic!("expected explicit recovery, got {state:?}"),
    }
    assert!(app.repository().list_sessions(100, 0).unwrap().is_empty());
    assert_eq!(
        app.repository().get_draft(&draft.id).unwrap().unwrap(),
        draft
    );
    app.dispatch(Command::Recover(RecoveryChoice::Continue))
        .unwrap();
    after_restart.advance(600);
    let terminal = app.dispatch(Command::Poll).unwrap().unwrap();
    assert!(terminal.recovered);
    assert_eq!(terminal.outcome, SessionOutcome::Completed);
    assert_eq!(terminal.elapsed_ms, 1_000);
    assert_eq!(
        terminal
            .intervals
            .iter()
            .map(|span| span.elapsed_ms)
            .sum::<u64>(),
        1_000
    );
    assert_eq!(terminal.intervals.len(), 2);
    assert_eq!(app.checkpoint().completed_focus, 1);
    assert_eq!(
        app.repository().get_draft(&draft.id).unwrap().unwrap(),
        draft
    );
}

#[test]
fn two_open_services_cannot_overwrite_each_others_timer_state() {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("rhythm.sqlite3");
    let clock = ManualClock::new("shared-test-clock", 10_000);
    let mut first = Rhythm::open(
        Store::open(&database).unwrap(),
        clock.clone(),
        Settings::default(),
    )
    .unwrap();
    first.dispatch(focus("only-session")).unwrap();
    let mut second = Rhythm::open(
        Store::open(&database).unwrap(),
        clock.clone(),
        Settings::default(),
    )
    .unwrap();
    clock.advance(300);
    first.dispatch(Command::Poll).unwrap();
    let before = second.checkpoint();
    assert!(matches!(
        second.dispatch(Command::EndEarly),
        Err(ApplicationError::Repository(
            rhythm_storage::Error::TimerConflict
        ))
    ));
    assert_eq!(second.checkpoint(), before);
    assert!(first.repository().list_sessions(100, 0).unwrap().is_empty());
    clock.advance(700);
    first.dispatch(Command::Poll).unwrap().unwrap();
    assert_eq!(second.repository().list_sessions(100, 0).unwrap().len(), 1);
}
