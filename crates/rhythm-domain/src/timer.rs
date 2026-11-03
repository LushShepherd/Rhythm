use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Current persisted timer schema. Unknown versions must not be rewritten.
pub const CHECKPOINT_VERSION: u32 = 1;
/// Guard against accidental units/overflow mistakes. Durations are milliseconds.
pub const MAX_PERIOD_MS: u64 = 24 * 60 * 60 * 1_000;
/// Small clock-reading differences are allowed; larger changes make UTC mapping uncertain.
pub const WALL_CLOCK_TOLERANCE_MS: i128 = 2_000;
const MAX_CYCLE_LENGTH: u32 = 1_000;
const MAX_IDENTIFIER_BYTES: usize = 128;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    pub focus_ms: u64,
    pub short_rest_ms: u64,
    pub long_rest_ms: u64,
    /// Legacy field name retained on disk and in the CLI. A cadence of four
    /// means three completed short rests before the next long rest.
    pub focus_before_long_rest: u32,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            focus_ms: 25 * 60 * 1_000,
            short_rest_ms: 5 * 60 * 1_000,
            long_rest_ms: 30 * 60 * 1_000,
            focus_before_long_rest: 4,
        }
    }
}

impl Settings {
    pub fn validate(&self) -> Result<(), TimerError> {
        for duration in [self.focus_ms, self.short_rest_ms, self.long_rest_ms] {
            validate_duration(duration)?;
        }
        if !(1..=MAX_CYCLE_LENGTH).contains(&self.focus_before_long_rest) {
            return Err(TimerError::InvalidSettings(
                "long-rest cadence must be between 1 and 1000 focus periods",
            ));
        }
        Ok(())
    }

    fn duration(&self, kind: PeriodKind) -> u64 {
        match kind {
            PeriodKind::Focus => self.focus_ms,
            PeriodKind::ShortRest => self.short_rest_ms,
            PeriodKind::LongRest => self.long_rest_ms,
        }
    }
}

/// A comparable elapsed-clock sample plus a UTC observation for journal dates.
///
/// `monotonic_ms` is not a serialized Rust `Instant`. An adapter may expose
/// process-relative or boot-relative ticks, but must change `epoch` whenever
/// those ticks cannot be compared with earlier samples. The engine does not
/// infer elapsed time from `utc_ms` or assume that an adapter counts sleep.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClockSample {
    pub epoch: String,
    pub monotonic_ms: u64,
    pub utc_ms: i64,
}

impl ClockSample {
    fn validate(&self) -> Result<(), TimerError> {
        validate_identifier(&self.epoch, "clock epoch")
    }
}

/// Supplies samples without tying the domain to an operating system or clock.
pub trait Clock {
    fn sample(&self) -> ClockSample;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PeriodKind {
    Focus,
    ShortRest,
    LongRest,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionOutcome {
    Completed,
    EndedEarly,
}

/// One measured active span. Pauses and time after a deadline are excluded.
///
/// If a clock correction prevents reliable calendar allocation, elapsed time
/// remains valid but `mapping_certain` is false and `ended_utc_ms` is `None`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActiveInterval {
    pub started_utc_ms: i64,
    pub ended_utc_ms: Option<i64>,
    pub elapsed_ms: u64,
    pub mapping_certain: bool,
}

/// A terminal record returned once, when a period completes or is ended early.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionRecord {
    pub id: String,
    pub kind: PeriodKind,
    pub planned_ms: u64,
    pub elapsed_ms: u64,
    pub started_utc_ms: i64,
    /// Actual deadline for completion, or the explicit end time when known.
    pub ended_utc_ms: Option<i64>,
    /// When the application observed completion; may be much later than the deadline.
    pub detected_utc_ms: i64,
    pub outcome: SessionOutcome,
    pub recovered: bool,
    pub intervals: Vec<ActiveInterval>,
}

impl SessionRecord {
    /// Check a decoded terminal record before storing or presenting it.
    pub fn validate(&self) -> Result<(), TimerError> {
        validate_identifier(&self.id, "session ID")?;
        validate_duration(self.planned_ms)?;
        if self.elapsed_ms > self.planned_ms
            || (self.outcome == SessionOutcome::Completed && self.elapsed_ms != self.planned_ms)
            || (self.outcome == SessionOutcome::EndedEarly && self.elapsed_ms == self.planned_ms)
        {
            return Err(TimerError::InvalidSession("outcome and duration disagree"));
        }
        let mut measured = 0_u64;
        for interval in &self.intervals {
            validate_interval(interval)
                .map_err(|_| TimerError::InvalidSession("invalid active interval"))?;
            measured = measured
                .checked_add(interval.elapsed_ms)
                .ok_or(TimerError::InvalidSession("interval total overflows"))?;
        }
        if measured != self.elapsed_ms {
            return Err(TimerError::InvalidSession(
                "active intervals do not match elapsed time",
            ));
        }
        if self.outcome == SessionOutcome::Completed
            && self.ended_utc_ms != self.intervals.last().and_then(|span| span.ended_utc_ms)
        {
            return Err(TimerError::InvalidSession(
                "completion does not match the final interval",
            ));
        }
        Ok(())
    }
}

/// Snapshot of the active span, kept separate from already closed intervals.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OpenInterval {
    pub started_utc_ms: i64,
    pub elapsed_ms: u64,
    pub mapping_certain: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActivePeriod {
    pub id: String,
    pub kind: PeriodKind,
    pub planned_ms: u64,
    /// Verified active elapsed time through `last_sample`, never estimated from UTC.
    pub elapsed_ms: u64,
    pub started_utc_ms: i64,
    pub settings_snapshot: Settings,
    pub last_sample: ClockSample,
    pub intervals: Vec<ActiveInterval>,
    pub open_interval: Option<OpenInterval>,
    pub recovered: bool,
}

impl ActivePeriod {
    pub fn remaining_ms(&self) -> u64 {
        self.planned_ms.saturating_sub(self.elapsed_ms)
    }

    fn new(
        id: String,
        kind: PeriodKind,
        duration: u64,
        settings: Settings,
        sample: ClockSample,
    ) -> Self {
        Self {
            id,
            kind,
            planned_ms: duration,
            elapsed_ms: 0,
            started_utc_ms: sample.utc_ms,
            settings_snapshot: settings,
            open_interval: Some(OpenInterval {
                started_utc_ms: sample.utc_ms,
                elapsed_ms: 0,
                mapping_certain: true,
            }),
            last_sample: sample,
            intervals: Vec::new(),
            recovered: false,
        }
    }

    fn advance(&mut self, sample: &ClockSample) -> Result<(), TimerError> {
        let delta = sample
            .monotonic_ms
            .checked_sub(self.last_sample.monotonic_ms)
            .ok_or(TimerError::ClockWentBackwards)?;
        let counted = delta.min(self.remaining_ms());
        let span = self
            .open_interval
            .as_mut()
            .ok_or(TimerError::InvalidCheckpoint("running interval is missing"))?;
        let wall_delta = i128::from(sample.utc_ms) - i128::from(self.last_sample.utc_ms);
        let span_wall = i128::from(sample.utc_ms) - i128::from(span.started_utc_ms);
        let span_monotonic = i128::from(span.elapsed_ms) + i128::from(delta);
        if (wall_delta - i128::from(delta)).abs() > WALL_CLOCK_TOLERANCE_MS
            || (span_wall - span_monotonic).abs() > WALL_CLOCK_TOLERANCE_MS
        {
            span.mapping_certain = false;
        }
        span.elapsed_ms += counted;
        self.elapsed_ms += counted;
        self.last_sample = sample.clone();
        Ok(())
    }

    fn effective_end(&self) -> Option<i64> {
        self.open_interval.as_ref().and_then(|span| {
            span.mapping_certain
                .then(|| mapped_end(span.started_utc_ms, span.elapsed_ms))
                .flatten()
        })
    }

    fn close_interval(&mut self) {
        if let Some(span) = self.open_interval.take() {
            // A zero-length span carries no measured time and need not clutter history.
            if span.elapsed_ms > 0 {
                let end = span
                    .mapping_certain
                    .then(|| mapped_end(span.started_utc_ms, span.elapsed_ms))
                    .flatten();
                self.intervals.push(ActiveInterval {
                    started_utc_ms: span.started_utc_ms,
                    ended_utc_ms: end,
                    elapsed_ms: span.elapsed_ms,
                    mapping_certain: end.is_some(),
                });
            }
        }
    }

    fn begin_interval(&mut self, sample: ClockSample) {
        self.open_interval = Some(OpenInterval {
            started_utc_ms: sample.utc_ms,
            elapsed_ms: 0,
            mapping_certain: true,
        });
        self.last_sample = sample;
    }
}

/// Ready states are invitations, not automatic transitions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", content = "data", rename_all = "snake_case")]
pub enum TimerState {
    Ready { next: PeriodKind },
    Running(ActivePeriod),
    Paused(ActivePeriod),
    RecoveryRequired(ActivePeriod),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Checkpoint {
    pub version: u32,
    pub settings: Settings,
    pub completed_focus: u32,
    /// Actual completed short rests; skipped or interrupted rests are excluded.
    /// Older checkpoints start with no inferred recovery credit.
    #[serde(default)]
    pub completed_short_rests: u32,
    pub long_rest_due: bool,
    pub state: TimerState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecoveryChoice {
    /// Resume only the verified remaining duration from a fresh clock anchor.
    Continue,
    /// End without crediting the unknown interval or claiming completion.
    End,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "command", content = "data", rename_all = "snake_case")]
pub enum Command {
    /// An explicit one-time extension; never changes saved preferences.
    ExtendFocus {
        additional_ms: u64,
    },
    /// Enter Focus directly, committing the previous period and the new one together.
    SwitchToFocus {
        session_id: String,
        duration_ms: Option<u64>,
    },
    /// Enter the appropriate short or long Break directly.
    SwitchToBreak {
        session_id: String,
        duration_ms: Option<u64>,
    },
    StartFocus {
        session_id: String,
        duration_ms: Option<u64>,
    },
    StartRest {
        session_id: String,
        duration_ms: Option<u64>,
    },
    Poll,
    Pause,
    Resume,
    EndEarly,
    /// End an interrupted desktop period at its last verified checkpoint.
    /// Never advance the clock or turn an overdue interruption into completion.
    Interrupt,
    NewRhythm,
    UpdateSettings(Settings),
    Recover(RecoveryChoice),
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum TimerError {
    #[error("invalid settings: {0}")]
    InvalidSettings(&'static str),
    #[error("duration must be between 1 millisecond and 24 hours")]
    InvalidDuration,
    #[error("{0} must contain 1 to 128 bytes of nonblank text")]
    InvalidIdentifier(&'static str),
    #[error("this action is not available in the current timer state")]
    InvalidTransition,
    #[error("the elapsed clock moved backwards within the same epoch")]
    ClockWentBackwards,
    #[error("unsupported timer checkpoint version {0}")]
    UnsupportedVersion(u32),
    #[error("invalid timer checkpoint: {0}")]
    InvalidCheckpoint(&'static str),
    #[error("invalid terminal session: {0}")]
    InvalidSession(&'static str),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TimerEngine {
    checkpoint: Checkpoint,
}

impl Default for TimerEngine {
    fn default() -> Self {
        // Defaults are defined constants and always satisfy validation.
        Self {
            checkpoint: Checkpoint {
                version: CHECKPOINT_VERSION,
                settings: Settings::default(),
                completed_focus: 0,
                completed_short_rests: 0,
                long_rest_due: false,
                state: TimerState::Ready {
                    next: PeriodKind::Focus,
                },
            },
        }
    }
}

impl TimerEngine {
    pub fn new(settings: Settings) -> Result<Self, TimerError> {
        settings.validate()?;
        Ok(Self {
            checkpoint: Checkpoint {
                settings,
                ..Self::default().checkpoint
            },
        })
    }

    /// Load serialized state, validating it before any transition is permitted.
    ///
    /// Call `apply(Command::Poll, current_sample)` to reconcile a running state.
    /// A different epoch yields `RecoveryRequired`; a wall-clock estimate never
    /// completes a restored timer.
    pub fn from_checkpoint(checkpoint: Checkpoint) -> Result<Self, TimerError> {
        validate_checkpoint(&checkpoint)?;
        Ok(Self { checkpoint })
    }

    pub fn checkpoint(&self) -> Checkpoint {
        self.checkpoint.clone()
    }

    pub fn state(&self) -> &TimerState {
        &self.checkpoint.state
    }

    pub fn settings(&self) -> &Settings {
        &self.checkpoint.settings
    }

    pub fn completed_focus(&self) -> u32 {
        self.checkpoint.completed_focus
    }

    pub fn completed_short_rests(&self) -> u32 {
        self.checkpoint.completed_short_rests
    }

    pub fn long_rest_due(&self) -> bool {
        self.checkpoint.long_rest_due
    }

    /// Apply an explicit action. An error leaves the entire engine unchanged.
    ///
    /// An incomparable running clock enters `RecoveryRequired` and returns no
    /// terminal record. The requested time-dependent action is not performed;
    /// the caller should render the resulting recovery state. Invalid start or
    /// resume actions are rejected before touching an existing period.
    pub fn apply(
        &mut self,
        command: Command,
        sample: ClockSample,
    ) -> Result<Option<SessionRecord>, TimerError> {
        sample.validate()?;
        let mut next = self.clone();
        let result = next.apply_inner(command, sample)?;
        validate_checkpoint(&next.checkpoint)?;
        if let Some(record) = &result {
            record.validate()?;
        }
        *self = next;
        Ok(result)
    }

    fn apply_inner(
        &mut self,
        command: Command,
        sample: ClockSample,
    ) -> Result<Option<SessionRecord>, TimerError> {
        match command {
            Command::Interrupt => return Ok(self.interrupt(sample.utc_ms)),
            Command::ExtendFocus { additional_ms } => {
                return self.extend_focus(additional_ms, sample);
            }
            Command::SwitchToFocus {
                session_id,
                duration_ms,
            } => return self.switch(true, session_id, duration_ms, sample),
            Command::SwitchToBreak {
                session_id,
                duration_ms,
            } => return self.switch(false, session_id, duration_ms, sample),
            Command::UpdateSettings(settings) => {
                settings.validate()?;
                self.checkpoint.settings = settings;
                return Ok(None);
            }
            Command::NewRhythm => {
                if !matches!(self.state(), TimerState::Ready { .. }) {
                    return Err(TimerError::InvalidTransition);
                }
                self.checkpoint.completed_focus = 0;
                self.checkpoint.completed_short_rests = 0;
                self.checkpoint.long_rest_due = false;
                self.checkpoint.state = TimerState::Ready {
                    next: PeriodKind::Focus,
                };
                return Ok(None);
            }
            Command::StartFocus {
                session_id,
                duration_ms,
            } => return self.start(PeriodKind::Focus, session_id, duration_ms, sample),
            Command::StartRest {
                session_id,
                duration_ms,
            } => {
                let kind = if self.checkpoint.long_rest_due {
                    PeriodKind::LongRest
                } else {
                    PeriodKind::ShortRest
                };
                return self.start(kind, session_id, duration_ms, sample);
            }
            Command::Recover(choice) => return self.recover(choice, sample),
            Command::Resume => {
                let TimerState::Paused(mut active) = self.checkpoint.state.clone() else {
                    return Err(TimerError::InvalidTransition);
                };
                if active.last_sample.epoch == sample.epoch
                    && sample.monotonic_ms < active.last_sample.monotonic_ms
                {
                    return Err(TimerError::ClockWentBackwards);
                }
                active.begin_interval(sample);
                self.checkpoint.state = TimerState::Running(active);
                return Ok(None);
            }
            Command::Poll | Command::Pause | Command::EndEarly => {}
        }

        match self.checkpoint.state.clone() {
            TimerState::Ready { .. } => {
                if command == Command::Poll {
                    Ok(None)
                } else {
                    Err(TimerError::InvalidTransition)
                }
            }
            TimerState::RecoveryRequired(_) => {
                if command == Command::Poll {
                    Ok(None)
                } else {
                    Err(TimerError::InvalidTransition)
                }
            }
            TimerState::Paused(active) => {
                if command == Command::Poll {
                    Ok(None)
                } else if command == Command::EndEarly {
                    Ok(Some(self.finish(
                        active,
                        SessionOutcome::EndedEarly,
                        Some(sample.utc_ms),
                        sample.utc_ms,
                    )))
                } else {
                    Err(TimerError::InvalidTransition)
                }
            }
            TimerState::Running(mut active) => {
                if active.last_sample.epoch != sample.epoch {
                    active.close_interval();
                    active.recovered = true;
                    self.checkpoint.state = TimerState::RecoveryRequired(active);
                    return Ok(None);
                }
                active.advance(&sample)?;
                if active.elapsed_ms == active.planned_ms {
                    let ended = active.effective_end();
                    return Ok(Some(self.finish(
                        active,
                        SessionOutcome::Completed,
                        ended,
                        sample.utc_ms,
                    )));
                }
                if command == Command::EndEarly {
                    let ended = active.effective_end();
                    Ok(Some(self.finish(
                        active,
                        SessionOutcome::EndedEarly,
                        ended,
                        sample.utc_ms,
                    )))
                } else {
                    if command == Command::Pause {
                        active.close_interval();
                        self.checkpoint.state = TimerState::Paused(active);
                    } else {
                        self.checkpoint.state = TimerState::Running(active);
                    }
                    Ok(None)
                }
            }
        }
    }

    fn extend_focus(
        &mut self,
        additional_ms: u64,
        sample: ClockSample,
    ) -> Result<Option<SessionRecord>, TimerError> {
        validate_duration(additional_ms)?;
        let TimerState::Running(mut active) = self.checkpoint.state.clone() else {
            return Err(TimerError::InvalidTransition);
        };
        if active.kind != PeriodKind::Focus || active.last_sample.epoch != sample.epoch {
            return Err(TimerError::InvalidTransition);
        }
        let extended = active
            .planned_ms
            .checked_add(additional_ms)
            .ok_or(TimerError::InvalidDuration)?;
        validate_duration(extended)?;
        active.advance(&sample)?;
        // A completed period must be committed as completed, never revived by
        // a delayed click. The next normal poll will publish its completion.
        if active.remaining_ms() == 0 {
            return Err(TimerError::InvalidTransition);
        }
        active.planned_ms = extended;
        self.checkpoint.state = TimerState::Running(active);
        Ok(None)
    }

    fn interrupt(&mut self, detected_utc_ms: i64) -> Option<SessionRecord> {
        let terminal = match self.checkpoint.state.clone() {
            TimerState::Running(mut active)
            | TimerState::Paused(mut active)
            | TimerState::RecoveryRequired(mut active) => {
                // A power callback may be delivered only after wake. The last
                // committed elapsed time is the safe boundary in every case.
                active.recovered = true;
                Some(self.finish(active, SessionOutcome::EndedEarly, None, detected_utc_ms))
            }
            TimerState::Ready { .. } => None,
        };
        self.checkpoint.state = TimerState::Ready {
            next: PeriodKind::Focus,
        };
        terminal
    }

    /// A state switch is one domain transition and therefore one durable transaction.
    /// Legacy paused/recovering periods contribute only their verified elapsed time.
    fn switch(
        &mut self,
        focus: bool,
        id: String,
        duration: Option<u64>,
        sample: ClockSample,
    ) -> Result<Option<SessionRecord>, TimerError> {
        validate_identifier(&id, "session ID")?;
        if let Some(duration) = duration {
            validate_duration(duration)?;
        }
        if matches!(self.state(), TimerState::Running(active)
            if (active.kind == PeriodKind::Focus) == focus)
        {
            // A repeated request to enter the state already in progress must not
            // reset its timer or create duplicate history.
            return Ok(None);
        }
        let terminal = match self.checkpoint.state.clone() {
            TimerState::Ready { .. } => None,
            TimerState::Running(mut active) => {
                let (outcome, ended) = if active.last_sample.epoch != sample.epoch {
                    active.recovered = true;
                    (SessionOutcome::EndedEarly, None)
                } else {
                    active.advance(&sample)?;
                    let outcome = if active.elapsed_ms == active.planned_ms {
                        SessionOutcome::Completed
                    } else {
                        SessionOutcome::EndedEarly
                    };
                    (outcome, active.effective_end())
                };
                Some(self.finish(active, outcome, ended, sample.utc_ms))
            }
            TimerState::Paused(active) => Some(self.finish(
                active,
                SessionOutcome::EndedEarly,
                Some(sample.utc_ms),
                sample.utc_ms,
            )),
            TimerState::RecoveryRequired(active) => {
                Some(self.finish(active, SessionOutcome::EndedEarly, None, sample.utc_ms))
            }
        };
        // Completing Focus above can make the long Break due, so choose its kind
        // after closing the old period.
        let kind = if focus {
            PeriodKind::Focus
        } else if self.checkpoint.long_rest_due {
            PeriodKind::LongRest
        } else {
            PeriodKind::ShortRest
        };
        self.start(kind, id, duration, sample)?;
        Ok(terminal)
    }

    fn start(
        &mut self,
        kind: PeriodKind,
        id: String,
        duration: Option<u64>,
        sample: ClockSample,
    ) -> Result<Option<SessionRecord>, TimerError> {
        if !matches!(self.state(), TimerState::Ready { .. }) {
            return Err(TimerError::InvalidTransition);
        }
        validate_identifier(&id, "session ID")?;
        let duration = duration.unwrap_or_else(|| self.settings().duration(kind));
        validate_duration(duration)?;
        self.checkpoint.state = TimerState::Running(ActivePeriod::new(
            id,
            kind,
            duration,
            self.checkpoint.settings.clone(),
            sample,
        ));
        Ok(None)
    }

    fn recover(
        &mut self,
        choice: RecoveryChoice,
        sample: ClockSample,
    ) -> Result<Option<SessionRecord>, TimerError> {
        let TimerState::RecoveryRequired(mut active) = self.checkpoint.state.clone() else {
            return Err(TimerError::InvalidTransition);
        };
        active.recovered = true;
        match choice {
            RecoveryChoice::Continue => {
                active.begin_interval(sample);
                self.checkpoint.state = TimerState::Running(active);
                Ok(None)
            }
            RecoveryChoice::End => Ok(Some(self.finish(
                active,
                SessionOutcome::EndedEarly,
                None,
                sample.utc_ms,
            ))),
        }
    }

    fn finish(
        &mut self,
        mut active: ActivePeriod,
        outcome: SessionOutcome,
        ended_utc_ms: Option<i64>,
        detected_utc_ms: i64,
    ) -> SessionRecord {
        active.close_interval();
        if outcome == SessionOutcome::Completed {
            match active.kind {
                PeriodKind::Focus => {
                    self.checkpoint.completed_focus =
                        self.checkpoint.completed_focus.saturating_add(1);
                    self.checkpoint.long_rest_due |= self.checkpoint.completed_short_rests
                        >= active
                            .settings_snapshot
                            .focus_before_long_rest
                            .saturating_sub(1);
                }
                PeriodKind::LongRest => {
                    self.checkpoint.completed_focus = 0;
                    self.checkpoint.completed_short_rests = 0;
                    self.checkpoint.long_rest_due = false;
                }
                PeriodKind::ShortRest => {
                    self.checkpoint.completed_short_rests =
                        self.checkpoint.completed_short_rests.saturating_add(1);
                    self.checkpoint.long_rest_due |= self.checkpoint.completed_short_rests
                        >= active
                            .settings_snapshot
                            .focus_before_long_rest
                            .saturating_sub(1);
                }
            }
        }
        let next = if active.kind == PeriodKind::Focus && outcome == SessionOutcome::Completed {
            if self.checkpoint.long_rest_due {
                PeriodKind::LongRest
            } else {
                PeriodKind::ShortRest
            }
        } else {
            PeriodKind::Focus
        };
        self.checkpoint.state = TimerState::Ready { next };
        SessionRecord {
            id: active.id,
            kind: active.kind,
            planned_ms: active.planned_ms,
            elapsed_ms: active.elapsed_ms,
            started_utc_ms: active.started_utc_ms,
            ended_utc_ms,
            detected_utc_ms,
            outcome,
            recovered: active.recovered,
            intervals: active.intervals,
        }
    }
}

fn validate_duration(duration: u64) -> Result<(), TimerError> {
    if !(1..=MAX_PERIOD_MS).contains(&duration) {
        Err(TimerError::InvalidDuration)
    } else {
        Ok(())
    }
}

fn validate_identifier(value: &str, label: &'static str) -> Result<(), TimerError> {
    if value.trim().is_empty() || value.len() > MAX_IDENTIFIER_BYTES {
        Err(TimerError::InvalidIdentifier(label))
    } else {
        Ok(())
    }
}

fn mapped_end(start: i64, elapsed: u64) -> Option<i64> {
    i64::try_from(elapsed)
        .ok()
        .and_then(|delta| start.checked_add(delta))
}

fn validate_interval(interval: &ActiveInterval) -> Result<(), TimerError> {
    if interval.elapsed_ms == 0
        || interval.mapping_certain != interval.ended_utc_ms.is_some()
        || (interval.mapping_certain
            && interval.ended_utc_ms != mapped_end(interval.started_utc_ms, interval.elapsed_ms))
    {
        Err(TimerError::InvalidCheckpoint("invalid closed interval"))
    } else {
        Ok(())
    }
}

fn validate_checkpoint(checkpoint: &Checkpoint) -> Result<(), TimerError> {
    if checkpoint.version != CHECKPOINT_VERSION {
        return Err(TimerError::UnsupportedVersion(checkpoint.version));
    }
    checkpoint.settings.validate()?;
    if checkpoint.long_rest_due
        && checkpoint.completed_focus == 0
        && checkpoint.completed_short_rests == 0
    {
        return Err(TimerError::InvalidCheckpoint(
            "long rest cannot be due before any completed focus or short rest",
        ));
    }
    let (active, must_be_open) = match &checkpoint.state {
        TimerState::Ready { next } => {
            if (*next == PeriodKind::LongRest && !checkpoint.long_rest_due)
                || (*next == PeriodKind::ShortRest && checkpoint.long_rest_due)
            {
                return Err(TimerError::InvalidCheckpoint(
                    "rest invitation disagrees with the cycle",
                ));
            }
            return Ok(());
        }
        TimerState::Running(active) => (active, true),
        TimerState::Paused(active) | TimerState::RecoveryRequired(active) => (active, false),
    };
    validate_identifier(&active.id, "session ID")?;
    validate_duration(active.planned_ms)?;
    active.settings_snapshot.validate()?;
    active.last_sample.validate()?;
    if active.elapsed_ms >= active.planned_ms {
        return Err(TimerError::InvalidCheckpoint(
            "an active period must have remaining time",
        ));
    }
    if active.open_interval.is_some() != must_be_open {
        return Err(TimerError::InvalidCheckpoint(
            "active interval disagrees with running state",
        ));
    }
    let mut measured = 0_u64;
    for interval in &active.intervals {
        measured = measured
            .checked_add(interval.elapsed_ms)
            .ok_or(TimerError::InvalidCheckpoint("interval total overflows"))?;
        validate_interval(interval)?;
    }
    if let Some(interval) = &active.open_interval {
        if interval.elapsed_ms > active.last_sample.monotonic_ms {
            return Err(TimerError::InvalidCheckpoint(
                "open interval exceeds the elapsed clock sample",
            ));
        }
        measured = measured
            .checked_add(interval.elapsed_ms)
            .ok_or(TimerError::InvalidCheckpoint("interval total overflows"))?;
    }
    if measured != active.elapsed_ms {
        return Err(TimerError::InvalidCheckpoint(
            "interval durations do not match verified elapsed time",
        ));
    }
    if active.kind == PeriodKind::LongRest && !checkpoint.long_rest_due {
        return Err(TimerError::InvalidCheckpoint(
            "long rest is active but no long rest is due",
        ));
    }
    Ok(())
}
