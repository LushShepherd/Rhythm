use proptest::prelude::*;
use rhythm_domain::{
    CHECKPOINT_VERSION, ClockSample, Command, MAX_PERIOD_MS, PeriodKind, RecoveryChoice,
    SessionOutcome, Settings, TimerEngine, TimerError, TimerState,
};

fn at(ms: u64) -> ClockSample {
    ClockSample {
        epoch: "test-clock".into(),
        monotonic_ms: ms,
        utc_ms: i64::try_from(ms).unwrap(),
    }
}

fn small() -> TimerEngine {
    TimerEngine::new(Settings {
        focus_ms: 100,
        short_rest_ms: 20,
        long_rest_ms: 60,
        focus_before_long_rest: 3,
    })
    .unwrap()
}

fn focus(id: &str) -> Command {
    Command::StartFocus {
        session_id: id.into(),
        duration_ms: None,
    }
}

fn rest(id: &str) -> Command {
    Command::StartRest {
        session_id: id.into(),
        duration_ms: None,
    }
}

fn active(engine: &TimerEngine) -> &rhythm_domain::ActivePeriod {
    match engine.state() {
        TimerState::Running(active)
        | TimerState::Paused(active)
        | TimerState::RecoveryRequired(active) => active,
        TimerState::Ready { .. } => panic!("expected an active or recovering period"),
    }
}

#[test]
fn defaults_match_the_product_and_start_ready() {
    let engine = TimerEngine::default();
    assert_eq!(engine.settings().focus_ms, 1_500_000);
    assert_eq!(engine.settings().short_rest_ms, 300_000);
    assert_eq!(engine.settings().long_rest_ms, 1_800_000);
    assert_eq!(engine.settings().focus_before_long_rest, 4);
    assert_eq!(
        engine.state(),
        &TimerState::Ready {
            next: PeriodKind::Focus
        }
    );
}

#[test]
fn default_cycle_has_three_actual_short_breaks_before_a_long_break() {
    let mut engine = TimerEngine::default();
    let mut time = 0;
    for index in 0..4 {
        engine
            .apply(focus(&format!("focus-{index}")), at(time))
            .unwrap();
        time += 1_500_000;
        engine.apply(Command::Poll, at(time)).unwrap();
        engine
            .apply(rest(&format!("rest-{index}")), at(time))
            .unwrap();
        assert_eq!(
            active(&engine).kind,
            if index < 3 {
                PeriodKind::ShortRest
            } else {
                PeriodKind::LongRest
            }
        );
        time += if index < 3 { 300_000 } else { 1_800_000 };
        engine.apply(Command::Poll, at(time)).unwrap();
    }
    assert_eq!(engine.completed_short_rests(), 0);
    assert!(!engine.long_rest_due());
}

#[test]
fn skipped_and_unfinished_short_rests_do_not_advance_recovery_cadence() {
    let mut engine = small();
    for index in 0..5 {
        engine
            .apply(focus(&format!("focus-{index}")), at(index * 110))
            .unwrap();
        engine.apply(Command::Poll, at(index * 110 + 100)).unwrap();
        engine
            .apply(rest(&format!("short-{index}")), at(index * 110 + 100))
            .unwrap();
        engine
            .apply(Command::EndEarly, at(index * 110 + 105))
            .unwrap();
    }
    assert_eq!(engine.completed_focus(), 5);
    assert_eq!(engine.completed_short_rests(), 0);
    assert!(!engine.long_rest_due());
}

#[test]
fn legacy_checkpoint_load_does_not_invent_completed_short_rests() {
    let engine = small();
    let mut json = serde_json::to_value(engine.checkpoint()).unwrap();
    json.as_object_mut()
        .unwrap()
        .remove("completed_short_rests");
    let restored = TimerEngine::from_checkpoint(serde_json::from_value(json).unwrap()).unwrap();
    assert_eq!(restored.completed_short_rests(), 0);
}

#[test]
fn extension_preserves_the_focus_period_and_counts_all_verified_time() {
    let mut engine = small();
    engine.apply(focus("focus"), at(0)).unwrap();
    engine
        .apply(Command::ExtendFocus { additional_ms: 50 }, at(80))
        .unwrap();
    assert_eq!(active(&engine).id, "focus");
    assert_eq!(active(&engine).elapsed_ms, 80);
    assert_eq!(active(&engine).planned_ms, 150);
    assert_eq!(engine.settings().focus_ms, 100);
    assert!(engine.apply(Command::Poll, at(100)).unwrap().is_none());
    let completed = engine.apply(Command::Poll, at(150)).unwrap().unwrap();
    assert_eq!(completed.elapsed_ms, 150);
    assert_eq!(completed.ended_utc_ms, Some(150));
}

#[test]
fn extension_rejects_invalid_states_values_and_elapsed_deadlines_atomically() {
    let mut engine = small();
    assert!(
        engine
            .apply(Command::ExtendFocus { additional_ms: 50 }, at(0))
            .is_err()
    );
    engine.apply(focus("focus"), at(0)).unwrap();
    for (additional_ms, at_ms) in [(0, 10), (MAX_PERIOD_MS, 10), (u64::MAX, 10), (50, 100)] {
        let before = engine.checkpoint();
        assert!(
            engine
                .apply(Command::ExtendFocus { additional_ms }, at(at_ms))
                .is_err()
        );
        assert_eq!(engine.checkpoint(), before);
    }
    engine.apply(Command::Poll, at(100)).unwrap();
    engine.apply(rest("break"), at(100)).unwrap();
    let before = engine.checkpoint();
    assert!(
        engine
            .apply(Command::ExtendFocus { additional_ms: 50 }, at(110))
            .is_err()
    );
    assert_eq!(engine.checkpoint(), before);
}

#[test]
fn direct_switches_preserve_elapsed_time_without_an_intermediate_ready_state() {
    let mut engine = small();
    engine
        .apply(
            Command::SwitchToFocus {
                session_id: "focus".into(),
                duration_ms: None,
            },
            at(0),
        )
        .unwrap();
    let record = engine
        .apply(
            Command::SwitchToBreak {
                session_id: "break".into(),
                duration_ms: None,
            },
            at(35),
        )
        .unwrap()
        .unwrap();
    assert_eq!(record.kind, PeriodKind::Focus);
    assert_eq!(record.elapsed_ms, 35);
    assert_eq!(record.outcome, SessionOutcome::EndedEarly);
    assert_eq!(engine.completed_focus(), 0);
    assert_eq!(active(&engine).kind, PeriodKind::ShortRest);
    assert_eq!(active(&engine).planned_ms, 20);
    let record = engine
        .apply(
            Command::SwitchToFocus {
                session_id: "focus-again".into(),
                duration_ms: None,
            },
            at(42),
        )
        .unwrap()
        .unwrap();
    assert_eq!(record.kind, PeriodKind::ShortRest);
    assert_eq!(record.elapsed_ms, 7);
    assert_eq!(active(&engine).kind, PeriodKind::Focus);
    assert_eq!(active(&engine).started_utc_ms, 42);
}

#[test]
fn switching_at_the_third_focus_deadline_selects_long_break() {
    let mut engine = small();
    for index in 0..3 {
        let now = index * 120;
        engine
            .apply(
                Command::SwitchToFocus {
                    session_id: format!("focus-{index}"),
                    duration_ms: None,
                },
                at(now),
            )
            .unwrap();
        let record = engine
            .apply(
                Command::SwitchToBreak {
                    session_id: format!("break-{index}"),
                    duration_ms: None,
                },
                at(now + 100),
            )
            .unwrap()
            .unwrap();
        assert_eq!(record.outcome, SessionOutcome::Completed);
    }
    assert_eq!(engine.completed_focus(), 3);
    assert_eq!(active(&engine).kind, PeriodKind::LongRest);
    assert_eq!(active(&engine).planned_ms, 60);
}

#[test]
fn repeated_same_state_switches_and_invalid_switches_cannot_reset_the_timer() {
    let mut engine = small();
    engine.apply(focus("focus"), at(0)).unwrap();
    let before = engine.checkpoint();
    assert!(
        engine
            .apply(
                Command::SwitchToFocus {
                    session_id: "duplicate".into(),
                    duration_ms: Some(500),
                },
                at(25)
            )
            .unwrap()
            .is_none()
    );
    assert_eq!(engine.checkpoint(), before);
    assert_eq!(
        engine.apply(
            Command::SwitchToBreak {
                session_id: "break".into(),
                duration_ms: Some(0),
            },
            at(30)
        ),
        Err(TimerError::InvalidDuration)
    );
    assert_eq!(engine.checkpoint(), before);
}

#[test]
fn switching_legacy_paused_and_recovering_periods_counts_only_verified_time() {
    for mode in ["paused", "recovery", "changed-epoch"] {
        let mut engine = small();
        engine.apply(focus("focus"), at(0)).unwrap();
        engine.apply(Command::Poll, at(30)).unwrap();
        let mut sample = at(5_000);
        if mode == "paused" {
            engine.apply(Command::Pause, at(30)).unwrap();
        } else {
            sample.epoch = "another-process".into();
            if mode == "recovery" {
                engine.apply(Command::Poll, sample.clone()).unwrap();
            }
        }
        let record = engine
            .apply(
                Command::SwitchToBreak {
                    session_id: "break".into(),
                    duration_ms: None,
                },
                sample,
            )
            .unwrap()
            .unwrap();
        assert_eq!(record.elapsed_ms, 30, "{mode}");
        assert_eq!(record.outcome, SessionOutcome::EndedEarly, "{mode}");
        assert_eq!(record.recovered, mode != "paused", "{mode}");
        assert_eq!(active(&engine).kind, PeriodKind::ShortRest);
        assert_eq!(active(&engine).elapsed_ms, 0);
    }
}

#[test]
fn boundaries_emit_completion_once_and_never_auto_start() {
    let mut engine = small();
    assert!(engine.apply(focus("first"), at(0)).unwrap().is_none());
    assert!(engine.apply(Command::Poll, at(99)).unwrap().is_none());
    assert_eq!(active(&engine).remaining_ms(), 1);
    let completed = engine.apply(Command::Poll, at(100)).unwrap().unwrap();
    assert_eq!(completed.elapsed_ms, 100);
    assert_eq!(completed.outcome, SessionOutcome::Completed);
    assert_eq!(engine.completed_focus(), 1);
    assert_eq!(
        engine.state(),
        &TimerState::Ready {
            next: PeriodKind::ShortRest
        }
    );
    let checkpoint = engine.checkpoint();
    for ms in [100, 1_000, 100_000] {
        assert!(engine.apply(Command::Poll, at(ms)).unwrap().is_none());
        assert_eq!(engine.checkpoint(), checkpoint);
    }
}

#[test]
fn delayed_completion_uses_deadline_and_caps_active_time() {
    let mut engine = small();
    engine.apply(focus("first"), at(50)).unwrap();
    let completed = engine.apply(Command::Poll, at(10_000)).unwrap().unwrap();
    assert_eq!(completed.elapsed_ms, 100);
    assert_eq!(completed.ended_utc_ms, Some(150));
    assert_eq!(completed.detected_utc_ms, 10_000);
    assert_eq!(completed.intervals[0].elapsed_ms, 100);
    assert_eq!(completed.intervals[0].ended_utc_ms, Some(150));
    assert_eq!(engine.completed_focus(), 1);
}

#[test]
fn pauses_are_excluded_and_resume_requires_explicit_action() {
    let mut engine = small();
    engine.apply(focus("first"), at(0)).unwrap();
    engine.apply(Command::Pause, at(30)).unwrap();
    engine.apply(Command::Poll, at(10_000)).unwrap();
    assert!(matches!(engine.state(), TimerState::Paused(_)));
    assert_eq!(active(&engine).elapsed_ms, 30);
    engine.apply(Command::Resume, at(20_000)).unwrap();
    assert!(engine.apply(Command::Poll, at(20_069)).unwrap().is_none());
    let completed = engine.apply(Command::Poll, at(20_070)).unwrap().unwrap();
    assert_eq!(completed.elapsed_ms, 100);
    assert_eq!(completed.ended_utc_ms, Some(20_070));
    assert_eq!(completed.intervals.len(), 2);
    assert_eq!(completed.intervals[0].elapsed_ms, 30);
    assert_eq!(completed.intervals[1].elapsed_ms, 70);
}

#[test]
fn pause_or_end_after_deadline_finishes_normally() {
    for command in [Command::Pause, Command::EndEarly] {
        let mut engine = small();
        engine.apply(focus("first"), at(0)).unwrap();
        let record = engine.apply(command, at(101)).unwrap().unwrap();
        assert_eq!(record.outcome, SessionOutcome::Completed);
        assert_eq!(record.ended_utc_ms, Some(100));
        assert_eq!(engine.completed_focus(), 1);
    }
}

#[test]
fn early_ending_does_not_increment_completed_focus() {
    let mut engine = small();
    engine.apply(focus("first"), at(0)).unwrap();
    let record = engine.apply(Command::EndEarly, at(25)).unwrap().unwrap();
    assert_eq!(record.elapsed_ms, 25);
    assert_eq!(record.outcome, SessionOutcome::EndedEarly);
    assert_eq!(engine.completed_focus(), 0);
    assert!(matches!(engine.state(), TimerState::Ready { .. }));
}

#[test]
fn ending_a_paused_period_does_not_count_the_pause() {
    let mut engine = small();
    engine.apply(focus("first"), at(0)).unwrap();
    engine.apply(Command::Pause, at(25)).unwrap();
    let record = engine
        .apply(Command::EndEarly, at(10_000))
        .unwrap()
        .unwrap();
    assert_eq!(record.elapsed_ms, 25);
    assert_eq!(record.intervals.len(), 1);
    assert_eq!(record.outcome, SessionOutcome::EndedEarly);
}

#[test]
fn third_completed_focus_requires_a_long_rest_and_completion_resets_cycle() {
    let mut engine = small();
    let mut time = 0;
    for i in 1..=3 {
        engine
            .apply(focus(&format!("focus-{i}")), at(time))
            .unwrap();
        time += 100;
        engine.apply(Command::Poll, at(time)).unwrap();
        assert_eq!(engine.completed_focus(), i);
        assert_eq!(engine.long_rest_due(), i == 3);
        let expected = if i == 3 {
            PeriodKind::LongRest
        } else {
            PeriodKind::ShortRest
        };
        assert_eq!(engine.state(), &TimerState::Ready { next: expected });
        engine.apply(rest(&format!("rest-{i}")), at(time)).unwrap();
        assert_eq!(active(&engine).kind, expected);
        time += if i == 3 { 60 } else { 20 };
        engine.apply(Command::Poll, at(time)).unwrap();
        assert_eq!(
            engine.state(),
            &TimerState::Ready {
                next: PeriodKind::Focus
            }
        );
    }
    assert_eq!(engine.completed_focus(), 0);
    assert!(!engine.long_rest_due());
}

#[test]
fn skipped_or_interrupted_long_rest_remains_due() {
    let mut engine = small();
    for i in 0..3 {
        engine
            .apply(focus(&format!("focus-{i}")), at(i * 120))
            .unwrap();
        engine.apply(Command::Poll, at(i * 120 + 100)).unwrap();
        if i < 2 {
            engine
                .apply(rest(&format!("short-{i}")), at(i * 120 + 100))
                .unwrap();
            engine.apply(Command::Poll, at(i * 120 + 120)).unwrap();
        }
    }
    assert!(engine.long_rest_due());
    engine.apply(rest("interrupted-rest"), at(340)).unwrap();
    let interrupted = engine.apply(Command::EndEarly, at(350)).unwrap().unwrap();
    assert_eq!(interrupted.kind, PeriodKind::LongRest);
    assert_eq!(interrupted.outcome, SessionOutcome::EndedEarly);
    assert!(engine.long_rest_due());
    assert_eq!(engine.completed_focus(), 3);
    engine.apply(focus("fourth"), at(350)).unwrap();
    engine.apply(Command::Poll, at(450)).unwrap();
    assert_eq!(
        engine.state(),
        &TimerState::Ready {
            next: PeriodKind::LongRest
        }
    );
}

#[test]
fn deliberately_new_rhythm_clears_cycle_only_when_ready() {
    let mut engine = small();
    engine.apply(focus("first"), at(0)).unwrap();
    let before = engine.checkpoint();
    assert_eq!(
        engine.apply(Command::NewRhythm, at(1)),
        Err(TimerError::InvalidTransition)
    );
    assert_eq!(engine.checkpoint(), before);
    engine.apply(Command::Poll, at(100)).unwrap();
    engine.apply(Command::NewRhythm, at(100)).unwrap();
    assert_eq!(engine.completed_focus(), 0);
    assert!(!engine.long_rest_due());
}

#[test]
fn one_time_duration_changes_only_one_period() {
    let mut engine = small();
    engine
        .apply(
            Command::StartFocus {
                session_id: "custom".into(),
                duration_ms: Some(7),
            },
            at(0),
        )
        .unwrap();
    let record = engine.apply(Command::Poll, at(7)).unwrap().unwrap();
    assert_eq!(record.planned_ms, 7);
    assert_eq!(engine.completed_focus(), 1);
    engine.apply(focus("ordinary"), at(7)).unwrap();
    assert_eq!(active(&engine).planned_ms, 100);
}

#[test]
fn setting_edits_do_not_change_a_running_period_or_its_cycle_policy() {
    let mut engine = small();
    engine.apply(focus("first"), at(0)).unwrap();
    let mut settings = engine.settings().clone();
    settings.focus_ms = 1;
    settings.focus_before_long_rest = 1;
    engine
        .apply(Command::UpdateSettings(settings), at(0))
        .unwrap();
    assert_eq!(active(&engine).planned_ms, 100);
    assert_eq!(active(&engine).settings_snapshot.focus_before_long_rest, 3);
    engine.apply(Command::Poll, at(100)).unwrap();
    assert!(!engine.long_rest_due());
    engine.apply(focus("second"), at(100)).unwrap();
    engine.apply(Command::Poll, at(101)).unwrap();
    assert!(engine.long_rest_due());
}

#[test]
fn rest_kind_is_preserved_when_settings_change_mid_rest() {
    let mut engine = small();
    for i in 0..3 {
        engine
            .apply(focus(&format!("focus-{i}")), at(i * 120))
            .unwrap();
        engine.apply(Command::Poll, at(i * 120 + 100)).unwrap();
        if i < 2 {
            engine
                .apply(rest(&format!("short-{i}")), at(i * 120 + 100))
                .unwrap();
            engine.apply(Command::Poll, at(i * 120 + 120)).unwrap();
        }
    }
    engine.apply(rest("long"), at(340)).unwrap();
    let mut settings = engine.settings().clone();
    settings.focus_before_long_rest = 100;
    settings.long_rest_ms = 1;
    engine
        .apply(Command::UpdateSettings(settings), at(340))
        .unwrap();
    assert_eq!(active(&engine).planned_ms, 60);
    engine.apply(Command::Poll, at(400)).unwrap();
    assert_eq!(engine.completed_focus(), 0);
}

#[test]
fn wall_clock_forward_or_backward_never_changes_elapsed_time() {
    for utc in [-10_000, 10_000] {
        let mut engine = small();
        engine.apply(focus("first"), at(0)).unwrap();
        let mut sample = at(50);
        sample.utc_ms = utc;
        assert!(engine.apply(Command::Poll, sample).unwrap().is_none());
        assert_eq!(active(&engine).elapsed_ms, 50);
        let mut sample = at(100);
        sample.utc_ms = utc + 50;
        let record = engine.apply(Command::Poll, sample).unwrap().unwrap();
        assert_eq!(record.elapsed_ms, 100);
        assert_eq!(record.ended_utc_ms, None);
        assert!(!record.intervals[0].mapping_certain);
    }
}

#[test]
fn cumulative_wall_drift_marks_allocation_uncertain() {
    let mut engine = small();
    engine
        .apply(
            Command::StartFocus {
                session_id: "long-enough".into(),
                duration_ms: Some(10_000),
            },
            at(0),
        )
        .unwrap();
    for tick in 1..=3 {
        let mut sample = at(tick * 1_000);
        sample.utc_ms += i64::try_from(tick * 1_000).unwrap();
        engine.apply(Command::Poll, sample).unwrap();
    }
    assert!(
        !active(&engine)
            .open_interval
            .as_ref()
            .unwrap()
            .mapping_certain
    );
}

#[test]
fn monotonic_rollback_returns_error_without_losing_state() {
    let mut engine = small();
    engine.apply(focus("first"), at(20)).unwrap();
    engine.apply(Command::Poll, at(40)).unwrap();
    let before = engine.checkpoint();
    assert_eq!(
        engine.apply(Command::Pause, at(39)),
        Err(TimerError::ClockWentBackwards)
    );
    assert_eq!(engine.checkpoint(), before);
}

#[test]
fn checkpoint_roundtrip_same_epoch_preserves_elapsed_and_pending_confirmation() {
    let mut engine = small();
    engine.apply(focus("first"), at(0)).unwrap();
    engine.apply(Command::Poll, at(40)).unwrap();
    let bytes = serde_json::to_vec(&engine.checkpoint()).unwrap();
    let checkpoint = serde_json::from_slice(&bytes).unwrap();
    let mut restored = TimerEngine::from_checkpoint(checkpoint).unwrap();
    assert_eq!(restored.checkpoint(), engine.checkpoint());
    let record = restored.apply(Command::Poll, at(150)).unwrap().unwrap();
    assert_eq!(record.elapsed_ms, 100);
    assert_eq!(record.ended_utc_ms, Some(100));
    assert_eq!(restored.completed_focus(), 1);
    assert!(restored.apply(Command::Poll, at(200)).unwrap().is_none());
}

#[test]
fn changed_epoch_requires_recovery_and_does_not_credit_unknown_time() {
    let mut engine = small();
    engine.apply(focus("first"), at(0)).unwrap();
    engine.apply(Command::Poll, at(30)).unwrap();
    let mut rebooted = at(1_000_000);
    rebooted.epoch = "another-boot".into();
    assert!(
        engine
            .apply(Command::Poll, rebooted.clone())
            .unwrap()
            .is_none()
    );
    assert!(matches!(engine.state(), TimerState::RecoveryRequired(_)));
    assert_eq!(active(&engine).elapsed_ms, 30);
    assert_eq!(engine.completed_focus(), 0);
    assert!(
        engine
            .apply(Command::Poll, rebooted.clone())
            .unwrap()
            .is_none()
    );
    let record = engine
        .apply(Command::Recover(RecoveryChoice::End), rebooted)
        .unwrap()
        .unwrap();
    assert_eq!(record.elapsed_ms, 30);
    assert_eq!(record.ended_utc_ms, None);
    assert!(record.recovered);
    assert_eq!(record.outcome, SessionOutcome::EndedEarly);
}

#[test]
fn recovery_continues_only_the_verified_remaining_duration() {
    let mut engine = small();
    engine.apply(focus("first"), at(0)).unwrap();
    engine.apply(Command::Poll, at(30)).unwrap();
    let fresh = |ms| ClockSample {
        epoch: "new-process".into(),
        monotonic_ms: ms,
        utc_ms: 10_000 + i64::try_from(ms).unwrap(),
    };
    engine.apply(Command::Poll, fresh(0)).unwrap();
    engine
        .apply(Command::Recover(RecoveryChoice::Continue), fresh(0))
        .unwrap();
    assert!(engine.apply(Command::Poll, fresh(69)).unwrap().is_none());
    let record = engine.apply(Command::Poll, fresh(70)).unwrap().unwrap();
    assert_eq!(record.elapsed_ms, 100);
    assert_eq!(record.intervals.len(), 2);
    assert_eq!(record.intervals[0].elapsed_ms, 30);
    assert_eq!(record.intervals[1].elapsed_ms, 70);
    assert!(record.recovered);
    assert_eq!(record.ended_utc_ms, Some(10_070));
}

#[test]
fn paused_checkpoint_can_resume_under_new_epoch_without_recovery_guess() {
    let mut engine = small();
    engine.apply(focus("first"), at(0)).unwrap();
    engine.apply(Command::Pause, at(30)).unwrap();
    let mut fresh = at(5);
    fresh.epoch = "new-process".into();
    engine.apply(Command::Resume, fresh).unwrap();
    assert_eq!(active(&engine).elapsed_ms, 30);
    assert!(matches!(engine.state(), TimerState::Running(_)));
}

#[test]
fn invalid_actions_and_values_are_atomic() {
    let mut engine = small();
    for action in [Command::Pause, Command::Resume, Command::EndEarly] {
        let before = engine.checkpoint();
        assert_eq!(
            engine.apply(action, at(0)),
            Err(TimerError::InvalidTransition)
        );
        assert_eq!(engine.checkpoint(), before);
    }
    for duration in [0, MAX_PERIOD_MS + 1, u64::MAX] {
        let before = engine.checkpoint();
        assert_eq!(
            engine.apply(
                Command::StartFocus {
                    session_id: "invalid".into(),
                    duration_ms: Some(duration),
                },
                at(0)
            ),
            Err(TimerError::InvalidDuration)
        );
        assert_eq!(engine.checkpoint(), before);
    }
    assert!(engine.apply(focus("   "), at(0)).is_err());
    engine.apply(focus("valid"), at(0)).unwrap();
    let before = engine.checkpoint();
    assert_eq!(
        engine.apply(focus("second"), at(1)),
        Err(TimerError::InvalidTransition)
    );
    assert_eq!(engine.checkpoint(), before);
}

#[test]
fn malformed_or_newer_checkpoints_cannot_load() {
    let mut engine = small();
    let mut newer = engine.checkpoint();
    newer.version = CHECKPOINT_VERSION + 1;
    assert_eq!(
        TimerEngine::from_checkpoint(newer),
        Err(TimerError::UnsupportedVersion(CHECKPOINT_VERSION + 1))
    );
    let mut invalid = engine.checkpoint();
    invalid.settings.focus_ms = 0;
    assert!(TimerEngine::from_checkpoint(invalid).is_err());
    engine.apply(focus("first"), at(0)).unwrap();
    engine.apply(Command::Poll, at(40)).unwrap();
    let mut invalid = engine.checkpoint();
    let TimerState::Running(ref mut active) = invalid.state else {
        unreachable!()
    };
    active.elapsed_ms = 39;
    assert!(TimerEngine::from_checkpoint(invalid).is_err());
    let mut invalid = engine.checkpoint();
    let TimerState::Running(ref mut active) = invalid.state else {
        unreachable!()
    };
    active.open_interval = None;
    assert!(TimerEngine::from_checkpoint(invalid).is_err());
}

#[test]
fn timestamp_overflow_loses_only_date_mapping_not_measured_time() {
    let mut engine = small();
    let mut start = at(0);
    start.utc_ms = i64::MAX - 10;
    engine.apply(focus("first"), start).unwrap();
    let mut end = at(100);
    end.utc_ms = i64::MAX;
    let record = engine.apply(Command::Poll, end).unwrap().unwrap();
    assert_eq!(record.elapsed_ms, 100);
    assert_eq!(record.ended_utc_ms, None);
    assert!(!record.intervals[0].mapping_certain);
}

#[test]
fn terminal_validation_rejects_inflated_or_misattributed_history() {
    let mut engine = small();
    engine.apply(focus("first"), at(0)).unwrap();
    let record = engine.apply(Command::Poll, at(100)).unwrap().unwrap();
    assert!(record.validate().is_ok());
    let mut inflated = record.clone();
    inflated.elapsed_ms = 101;
    assert!(inflated.validate().is_err());
    let mut missing_interval = record.clone();
    missing_interval.intervals.clear();
    assert!(missing_interval.validate().is_err());
    let mut wake_dated = record;
    wake_dated.ended_utc_ms = Some(10_000);
    assert!(wake_dated.validate().is_err());
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(128))]

    #[test]
    fn polling_frequency_does_not_change_terminal_duration_or_deadline(
        planned in 1_u64..10_000,
        gaps in prop::collection::vec(0_u64..500, 0..50),
        late in 0_u64..10_000,
    ) {
        let mut engine = small();
        engine.apply(Command::StartFocus {
            session_id: "property".into(), duration_ms: Some(planned)
        }, at(0)).unwrap();
        let mut elapsed = 0;
        for gap in gaps {
            elapsed = (elapsed + gap).min(planned.saturating_sub(1));
            prop_assert!(engine.apply(Command::Poll, at(elapsed)).unwrap().is_none());
        }
        let record = engine.apply(Command::Poll, at(planned + late)).unwrap().unwrap();
        prop_assert_eq!(record.elapsed_ms, planned);
        prop_assert_eq!(record.ended_utc_ms, Some(i64::try_from(planned).unwrap()));
        prop_assert_eq!(record.intervals.iter().map(|span| span.elapsed_ms).sum::<u64>(), planned);
        prop_assert_eq!(engine.completed_focus(), 1);
        prop_assert!(engine.apply(Command::Poll, at(planned + late + 1)).unwrap().is_none());
    }

    #[test]
    fn pause_length_never_changes_measured_duration(
        before in 1_u64..100,
        after in 1_u64..100,
        pause in 0_u64..1_000_000,
    ) {
        let duration = before + after;
        let mut engine = small();
        engine.apply(Command::StartFocus {
            session_id: "paused-property".into(), duration_ms: Some(duration)
        }, at(0)).unwrap();
        engine.apply(Command::Pause, at(before)).unwrap();
        engine.apply(Command::Resume, at(before + pause)).unwrap();
        let record = engine.apply(Command::Poll, at(duration + pause)).unwrap().unwrap();
        prop_assert_eq!(record.elapsed_ms, duration);
        prop_assert_eq!(record.ended_utc_ms, Some(i64::try_from(duration + pause).unwrap()));
        prop_assert_eq!(record.intervals.len(), 2);
    }

    #[test]
    fn arbitrary_commands_preserve_state_invariants(
        steps in prop::collection::vec((0_u8..10, 0_u64..300), 0..100),
    ) {
        let mut engine = small();
        let mut elapsed = 0;
        for (index, (action, delta)) in steps.into_iter().enumerate() {
            elapsed += delta;
            let command = match action {
                0 => focus(&format!("focus-{index}")),
                1 => rest(&format!("rest-{index}")),
                2 => Command::Pause,
                3 => Command::Resume,
                4 => Command::EndEarly,
                5 => Command::NewRhythm,
                6 => Command::Recover(RecoveryChoice::Continue),
                8 => Command::SwitchToFocus { session_id: format!("switch-focus-{index}"), duration_ms: None },
                9 => Command::SwitchToBreak { session_id: format!("switch-break-{index}"), duration_ms: None },
                _ => Command::Poll,
            };
            let before = engine.checkpoint();
            match engine.apply(command, at(elapsed)) {
                Err(_) => prop_assert_eq!(engine.checkpoint(), before),
                Ok(Some(record)) => {
                    prop_assert!(record.elapsed_ms <= record.planned_ms);
                    prop_assert_eq!(record.intervals.iter().map(|span| span.elapsed_ms).sum::<u64>(), record.elapsed_ms);
                    if action >= 8 {
                        prop_assert!(matches!(engine.state(), TimerState::Running(_)), "a switch enters the next state atomically");
                    } else {
                        prop_assert!(matches!(engine.state(), TimerState::Ready { .. }), "completion must leave the timer ready");
                    }
                }
                Ok(None) => {}
            }
            prop_assert!(TimerEngine::from_checkpoint(engine.checkpoint()).is_ok());
        }
    }
}
