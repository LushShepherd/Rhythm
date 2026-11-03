//! A delayed invitation is separate from Focus/Break session accounting.
use rhythm_desktop_service::{DesktopRequest, DesktopView};
use rhythm_domain::{ClockSample, PeriodKind, SessionOutcome, SessionRecord, TimerState};
use serde::{Deserialize, Serialize};
use std::{fs, path::PathBuf};

const DEFAULT_DELAY_MS: u64 = 9 * 60_000;
const MAX_DELAY_MS: u64 = 60 * 60_000;

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum BreakAction {
    Accept,
    Snooze,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ReminderKind {
    Focus,
    Break,
}

impl ReminderKind {
    pub(crate) fn from_period(kind: PeriodKind) -> Self {
        if kind == PeriodKind::Focus {
            Self::Focus
        } else {
            Self::Break
        }
    }
    fn matches(self, state: &TimerState) -> bool {
        matches!(
            (self, state),
            (
                Self::Focus,
                TimerState::Ready {
                    next: PeriodKind::ShortRest | PeriodKind::LongRest
                }
            ) | (
                Self::Break,
                TimerState::Ready {
                    next: PeriodKind::Focus
                }
            )
        )
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ReminderDescriptor {
    pub period_id: String,
    pub kind: ReminderKind,
    pub sequence: u32,
    pub due_at_utc_ms: i64,
    pub available: bool,
    pub postponements: u32,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct SavedReminder {
    schema: u8,
    reminder: Option<ReminderDescriptor>,
    delay_ms: u64,
    #[serde(default)]
    cleared_period_id: Option<String>,
}

#[derive(Clone, Default)]
pub(crate) struct BreakReminder {
    current: Option<ReminderDescriptor>,
    deadline_ms: u64,
    delay_ms: u64,
    path: Option<PathBuf>,
    last_period_id: Option<String>,
}

impl BreakReminder {
    /// Reopening is a desktop interruption, not a reason to replay an old
    /// invitation. Validate the file first, then durably retire its last owner.
    pub(crate) fn open_for_desktop(
        path: PathBuf,
        view: &DesktopView,
        now: &ClockSample,
    ) -> Result<Self, String> {
        let mut reminder = Self::open(path, view, now)?;
        reminder.clear();
        reminder.last_period_id = view.sessions.first().map(|session| session.id.clone());
        reminder.persist()?;
        Ok(reminder)
    }
    pub(crate) fn open(
        path: PathBuf,
        view: &DesktopView,
        now: &ClockSample,
    ) -> Result<Self, String> {
        let mut result = Self {
            path: Some(path.clone()),
            ..Self::default()
        };
        let saved = match fs::read(&path) {
            Ok(bytes) => {
                if bytes.len() > 4096 {
                    return Err("Saved reminder preferences are invalid.".into());
                }
                let saved: SavedReminder = serde_json::from_slice(&bytes)
                    .map_err(|_| "Saved reminder preferences could not be read. Existing data was left untouched.")?;
                if saved.schema != 1
                    || saved.delay_ms > MAX_DELAY_MS
                    || saved
                        .cleared_period_id
                        .as_ref()
                        .is_some_and(|id| id.is_empty() || id.len() > 128)
                    || saved.reminder.as_ref().is_some_and(|item| {
                        item.period_id.is_empty()
                            || item.period_id.len() > 128
                            || item.sequence == 0
                            || (!item.available && saved.delay_ms < 60_000)
                    })
                {
                    return Err(
                        "Saved reminder preferences are invalid. Existing data was left untouched."
                            .into(),
                    );
                }
                Some(saved)
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(_) => return Err("Saved reminder preferences could not be opened.".into()),
        };
        // Only a latest completed period and its still-ready checkpoint can own
        // a restored invitation; stale files cannot replace newer work.
        let latest = view.sessions.first();
        if let Some(saved) = saved {
            result.last_period_id = saved.cleared_period_id.clone();
            if let Some(item) = saved.reminder.filter(|item| {
                item.kind.matches(&view.checkpoint.state)
                    && latest.is_some_and(|last| {
                        last.id == item.period_id
                            && last.outcome == SessionOutcome::Completed
                            && ReminderKind::from_period(last.kind) == item.kind
                    })
            }) {
                let remaining = item.due_at_utc_ms.saturating_sub(now.utc_ms).max(0) as u64;
                result.deadline_ms = now
                    .monotonic_ms
                    .saturating_add(remaining.min(saved.delay_ms));
                result.delay_ms = saved.delay_ms;
                result.current = Some(item);
                result.advance(now);
            }
            // A crash between the timer commit and reminder-file write must not
            // hide a newer completed period. A tombstone prevents resurrecting
            // a choice that was explicitly cleared by a new rhythm or restore.
            if result.current.is_none()
                && let Some(last) = latest
                && result.last_period_id.as_deref() != Some(&last.id)
            {
                result.observe(&view.checkpoint.state, Some(last), now);
            }
        } else if let Some(last) = latest {
            result.observe(&view.checkpoint.state, Some(last), now);
        }
        Ok(result)
    }

    pub(crate) fn descriptor(&self) -> Option<ReminderDescriptor> {
        self.current.clone()
    }

    pub(crate) fn observe(
        &mut self,
        state: &TimerState,
        terminal: Option<&SessionRecord>,
        now: &ClockSample,
    ) -> bool {
        let before = self.descriptor();
        if let Some(terminal) = terminal {
            if self.last_period_id.as_deref() == Some(&terminal.id) {
                return false;
            }
            self.last_period_id = Some(terminal.id.clone());
            let kind = ReminderKind::from_period(terminal.kind);
            if terminal.outcome == SessionOutcome::Completed && kind.matches(state) {
                if self
                    .current
                    .as_ref()
                    .is_none_or(|item| item.period_id != terminal.id)
                {
                    self.current = Some(ReminderDescriptor {
                        period_id: terminal.id.clone(),
                        kind,
                        sequence: 1,
                        due_at_utc_ms: now.utc_ms,
                        available: true,
                        postponements: 0,
                    });
                    self.deadline_ms = now.monotonic_ms;
                    self.delay_ms = 0;
                }
            } else {
                self.clear();
            }
        } else if self
            .current
            .as_ref()
            .is_some_and(|item| !item.kind.matches(state))
        {
            self.clear();
        }
        before != self.current
    }

    pub(crate) fn clear(&mut self) -> bool {
        if let Some(item) = &self.current {
            self.last_period_id = Some(item.period_id.clone());
        }
        let changed = self.current.take().is_some();
        self.deadline_ms = 0;
        self.delay_ms = 0;
        changed
    }

    pub(crate) fn advance(&mut self, now: &ClockSample) -> bool {
        if let Some(item) = &mut self.current
            && !item.available
            && now.monotonic_ms >= self.deadline_ms
        {
            item.available = true;
            item.sequence = item.sequence.saturating_add(1);
            return true;
        }
        false
    }

    pub(crate) fn is_current(&self, state: &TimerState, period_id: &str, sequence: u32) -> bool {
        self.current.as_ref().is_some_and(|item| {
            item.period_id == period_id
                && item.sequence == sequence
                && item.available
                && item.kind.matches(state)
        })
    }

    pub(crate) fn accept_request(
        &self,
        state: &TimerState,
        period_id: &str,
        sequence: u32,
        duration_ms: Option<u64>,
    ) -> Option<DesktopRequest> {
        if !self.is_current(state, period_id, sequence) {
            return None;
        }
        Some(match self.current.as_ref()?.kind {
            ReminderKind::Focus => DesktopRequest::SwitchToBreak { duration_ms },
            ReminderKind::Break => DesktopRequest::SwitchToFocus { duration_ms },
        })
    }

    /// Save before acknowledging. Failed persistence leaves the current choice
    /// actionable, without creating or completing any timer/session record.
    pub(crate) fn postpone(
        &mut self,
        state: &TimerState,
        period_id: &str,
        sequence: u32,
        duration_ms: Option<u64>,
        now: &ClockSample,
    ) -> Result<bool, String> {
        if !self.is_current(state, period_id, sequence) {
            return Ok(false);
        }
        let duration_ms = duration_ms.unwrap_or(DEFAULT_DELAY_MS);
        if !(60_000..=MAX_DELAY_MS).contains(&duration_ms) {
            return Err("Reminder delay must be between 1 and 60 minutes.".into());
        }
        let mut next = self.clone();
        let item = next.current.as_mut().expect("checked current invitation");
        item.available = false;
        item.sequence = item
            .sequence
            .checked_add(1)
            .ok_or("This reminder has too many responses.")?;
        item.postponements = item.postponements.saturating_add(1);
        item.due_at_utc_ms = now.utc_ms.saturating_add(duration_ms as i64);
        next.deadline_ms = now.monotonic_ms.saturating_add(duration_ms);
        next.delay_ms = duration_ms;
        next.persist()?;
        *self = next;
        Ok(true)
    }

    pub(crate) fn persist(&self) -> Result<(), String> {
        let Some(path) = &self.path else {
            return Ok(());
        };
        let saved = SavedReminder {
            schema: 1,
            reminder: self.descriptor(),
            delay_ms: self.delay_ms,
            cleared_period_id: self.last_period_id.clone(),
        };
        let bytes = serde_json::to_vec(&saved).map_err(|_| "The reminder could not be saved.")?;
        let temp = path.with_extension("json.tmp");
        fs::write(&temp, bytes)
            .and_then(|_| fs::rename(&temp, path))
            .map_err(|_| "The reminder could not be saved on this device. Please try again.".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn file() -> PathBuf {
        std::env::temp_dir().join(format!("rhythm-reminder-{}.json", uuid::Uuid::new_v4()))
    }
    fn view(id: &str, kind: PeriodKind) -> DesktopView {
        DesktopView {
            checkpoint: rhythm_domain::Checkpoint {
                version: 1,
                settings: Default::default(),
                completed_focus: 1,
                completed_short_rests: 1,
                long_rest_due: false,
                state: ready(if kind == PeriodKind::Focus {
                    PeriodKind::ShortRest
                } else {
                    PeriodKind::Focus
                }),
            },
            revision: 1,
            sessions: vec![completed(id, kind)],
            drafts: vec![],
            reflections: vec![],
            note_state: Default::default(),
            routine_state: Default::default(),
        }
    }
    fn now(ms: u64) -> ClockSample {
        ClockSample {
            epoch: "test".into(),
            monotonic_ms: ms,
            utc_ms: 1_000_000 + ms as i64,
        }
    }
    fn ready(next: PeriodKind) -> TimerState {
        TimerState::Ready { next }
    }
    fn completed(id: &str, kind: PeriodKind) -> SessionRecord {
        SessionRecord {
            id: id.into(),
            kind,
            planned_ms: 1_000,
            elapsed_ms: 1_000,
            started_utc_ms: 0,
            ended_utc_ms: Some(1_000),
            detected_utc_ms: 1_000,
            outcome: SessionOutcome::Completed,
            recovered: false,
            intervals: vec![],
        }
    }

    #[test]
    fn both_completion_directions_offer_the_opposite_state() {
        for (kind, next) in [
            (PeriodKind::Focus, PeriodKind::ShortRest),
            (PeriodKind::ShortRest, PeriodKind::Focus),
            (PeriodKind::LongRest, PeriodKind::Focus),
        ] {
            let mut reminder = BreakReminder::default();
            let state = ready(next);
            reminder.observe(&state, Some(&completed("period", kind)), &now(0));
            reminder.observe(&state, None, &now(1));
            let request = reminder
                .accept_request(&state, "period", 1, Some(180_000))
                .unwrap();
            assert_eq!(
                request,
                if kind == PeriodKind::Focus {
                    DesktopRequest::SwitchToBreak {
                        duration_ms: Some(180_000),
                    }
                } else {
                    DesktopRequest::SwitchToFocus {
                        duration_ms: Some(180_000),
                    }
                }
            );
        }
    }

    #[test]
    fn delay_is_nine_minutes_and_does_not_create_another_period() {
        let state = ready(PeriodKind::Focus);
        let mut reminder = BreakReminder::default();
        reminder.observe(
            &state,
            Some(&completed("rest", PeriodKind::ShortRest)),
            &now(0),
        );
        assert!(
            reminder
                .postpone(&state, "rest", 1, None, &now(100))
                .unwrap()
        );
        let pending = reminder.descriptor().unwrap();
        assert_eq!(pending.due_at_utc_ms, now(540_100).utc_ms);
        assert_eq!(pending.sequence, 2);
        assert_eq!(pending.postponements, 1);
        assert!(!pending.available);
        assert!(!reminder.advance(&now(540_099)));
        assert!(reminder.advance(&now(540_100)));
        assert_eq!(reminder.descriptor().unwrap().sequence, 3);
        assert!(reminder.accept_request(&state, "rest", 1, None).is_none());
        assert!(reminder.accept_request(&state, "rest", 3, None).is_some());
    }

    #[test]
    fn manual_delay_bounds_and_clock_changes_do_not_accept_old_actions() {
        let state = ready(PeriodKind::ShortRest);
        let mut reminder = BreakReminder::default();
        reminder.observe(
            &state,
            Some(&completed("focus", PeriodKind::Focus)),
            &now(0),
        );
        assert!(
            reminder
                .postpone(&state, "focus", 1, Some(1), &now(0))
                .is_err()
        );
        assert!(
            reminder
                .postpone(&state, "focus", 1, Some(MAX_DELAY_MS + 1), &now(0))
                .is_err()
        );
        assert!(
            reminder
                .postpone(&state, "focus", 1, Some(60_000), &now(0))
                .unwrap()
        );
        let mut moved_wall_clock = now(1_000);
        moved_wall_clock.utc_ms += 99_000_000;
        assert!(!reminder.advance(&moved_wall_clock));
        assert!(
            !reminder
                .postpone(&state, "focus", 1, None, &now(2_000))
                .unwrap()
        );
        assert!(reminder.advance(&now(60_000)));
        reminder.clear();
        assert!(reminder.accept_request(&state, "focus", 3, None).is_none());
    }

    #[test]
    fn early_exit_and_superseded_completion_cannot_answer_an_old_toast() {
        let state = ready(PeriodKind::ShortRest);
        let mut reminder = BreakReminder::default();
        reminder.observe(&state, Some(&completed("old", PeriodKind::Focus)), &now(0));
        reminder.observe(&state, Some(&completed("new", PeriodKind::Focus)), &now(1));
        assert!(reminder.accept_request(&state, "old", 1, None).is_none());
        let mut ended = completed("early", PeriodKind::Focus);
        ended.outcome = SessionOutcome::EndedEarly;
        reminder.observe(&state, Some(&ended), &now(2));
        assert!(reminder.descriptor().is_none());
    }

    #[test]
    fn postponement_survives_restart_without_changing_session_or_cadence() {
        let path = file();
        let view = view("rest", PeriodKind::ShortRest);
        let before = view.clone();
        let mut reminder = BreakReminder::open(path.clone(), &view, &now(0)).unwrap();
        reminder
            .postpone(&view.checkpoint.state, "rest", 1, None, &now(100))
            .unwrap();
        let restarted = ClockSample {
            epoch: "new".into(),
            monotonic_ms: 10,
            utc_ms: now(120_100).utc_ms,
        };
        let mut loaded = BreakReminder::open(path.clone(), &view, &restarted).unwrap();
        assert_eq!(loaded.descriptor(), reminder.descriptor());
        assert!(!loaded.advance(&ClockSample {
            monotonic_ms: 420_009,
            ..restarted.clone()
        }));
        assert!(loaded.advance(&ClockSample {
            monotonic_ms: 420_010,
            ..restarted
        }));
        assert_eq!(view, before);
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn desktop_reopening_discards_due_and_postponed_reminders_in_both_directions() {
        for kind in [PeriodKind::Focus, PeriodKind::ShortRest] {
            for postponed in [false, true] {
                let path = file();
                let view = view("old", kind);
                let mut reminder = BreakReminder::open(path.clone(), &view, &now(0)).unwrap();
                if postponed {
                    reminder
                        .postpone(&view.checkpoint.state, "old", 1, None, &now(1))
                        .unwrap();
                } else {
                    reminder.persist().unwrap();
                }
                let mut reopened =
                    BreakReminder::open_for_desktop(path.clone(), &view, &now(9_000_000)).unwrap();
                assert!(reopened.descriptor().is_none());
                assert!(!reopened.advance(&now(99_000_000)));
                assert!(
                    reopened
                        .accept_request(&view.checkpoint.state, "old", 1, None)
                        .is_none()
                );
                assert!(!reopened.observe(
                    &view.checkpoint.state,
                    view.sessions.first(),
                    &now(99_000_001)
                ));
                assert!(
                    BreakReminder::open(path.clone(), &view, &now(99_000_002))
                        .unwrap()
                        .descriptor()
                        .is_none()
                );
                fs::remove_file(path).unwrap();
            }
        }
    }

    #[test]
    fn failed_delay_save_keeps_the_current_choice_and_corrupt_file_is_preserved() {
        let path = file();
        let view = view("focus", PeriodKind::Focus);
        let mut reminder = BreakReminder::default();
        reminder.observe(&view.checkpoint.state, view.sessions.first(), &now(0));
        // A nonexistent parent makes writing fail without touching any user data.
        reminder.path = Some(path.join("missing.json"));
        let before = reminder.descriptor();
        assert!(
            reminder
                .postpone(&view.checkpoint.state, "focus", 1, None, &now(1))
                .is_err()
        );
        assert_eq!(reminder.descriptor(), before);
        fs::write(&path, b"unknown saved format").unwrap();
        assert!(BreakReminder::open(path.clone(), &view, &now(1)).is_err());
        assert_eq!(fs::read(&path).unwrap(), b"unknown saved format");
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn cleared_choices_stay_cleared_but_a_newer_committed_completion_is_recovered() {
        let path = file();
        let previous = view("previous", PeriodKind::ShortRest);
        let mut reminder = BreakReminder::open(path.clone(), &previous, &now(0)).unwrap();
        reminder.clear();
        reminder.persist().unwrap();
        assert!(
            BreakReminder::open(path.clone(), &previous, &now(1))
                .unwrap()
                .descriptor()
                .is_none()
        );
        let current = view("current", PeriodKind::Focus);
        let loaded = BreakReminder::open(path.clone(), &current, &now(2)).unwrap();
        assert_eq!(loaded.descriptor().unwrap().period_id, "current");
        assert!(
            loaded
                .accept_request(&current.checkpoint.state, "previous", 1, None)
                .is_none()
        );
        fs::remove_file(path).unwrap();
    }
}
