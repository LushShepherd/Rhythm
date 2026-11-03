//! GUI-independent application services. State becomes visible only after a durable commit.
//!
//! A Tauri shell can call these services without owning timer or journal persistence rules.
//! The supplied [`ProcessClock`] is a development adapter: it deliberately does not promise
//! consistent sleep accounting or same-boot recovery across process restarts.

use rhythm_domain::{
    Checkpoint, Clock, ClockSample, Command, SessionRecord, Settings, TimerEngine, TimerError,
    TimerState,
};
use std::error::Error;
use std::time::{Instant, SystemTime, UNIX_EPOCH};
use thiserror::Error;
use uuid::Uuid;

/// A durable checkpoint and its optimistic concurrency token.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PersistedTimer {
    pub revision: u64,
    pub checkpoint: Checkpoint,
}

/// Storage port owned by the application layer.
///
/// Implementations must compare the expected revision and commit the checkpoint and any
/// terminal session in one transaction. `None` means insert only if no timer exists.
pub trait TimerRepository {
    type Error: Error;

    fn load_timer(&self) -> Result<Option<PersistedTimer>, Self::Error>;

    fn save_timer(
        &mut self,
        expected_revision: Option<u64>,
        checkpoint: &Checkpoint,
        terminal: Option<&SessionRecord>,
    ) -> Result<u64, Self::Error>;
}

#[derive(Debug, Error)]
pub enum ApplicationError<E: Error> {
    #[error(transparent)]
    Timer(#[from] TimerError),
    #[error("The change could not be saved: {0}")]
    Repository(#[source] E),
}

/// A single device's timer application service.
///
/// Each command is evaluated on a clone. A failed write leaves the in-memory engine unchanged,
/// so the caller cannot display a completed session that was never recorded. Another process
/// changing the checkpoint produces a storage conflict; reopen the service to reconcile it.
pub struct Rhythm<R: TimerRepository, C: Clock> {
    repository: R,
    clock: C,
    engine: TimerEngine,
    revision: u64,
}

impl<R: TimerRepository, C: Clock> Rhythm<R, C> {
    /// Open a stored timer or create one. Defaults apply only to a new database.
    /// Reopening reconciles the current clock without ever starting a new period.
    pub fn open(
        mut repository: R,
        clock: C,
        defaults: Settings,
    ) -> Result<Self, ApplicationError<R::Error>> {
        let saved = repository
            .load_timer()
            .map_err(ApplicationError::Repository)?;
        let (engine, revision) = match saved {
            Some(saved) => (
                TimerEngine::from_checkpoint(saved.checkpoint)?,
                saved.revision,
            ),
            None => {
                let engine = TimerEngine::new(defaults)?;
                let revision = repository
                    .save_timer(None, &engine.checkpoint(), None)
                    .map_err(ApplicationError::Repository)?;
                (engine, revision)
            }
        };
        let mut application = Self {
            repository,
            clock,
            engine,
            revision,
        };
        application.dispatch(Command::Poll)?;
        Ok(application)
    }

    /// Process an explicit user action or a platform poll. Returns a newly committed terminal
    /// session exactly once; callers may use that to present an invitation or cancel an alert.
    pub fn dispatch(
        &mut self,
        command: Command,
    ) -> Result<Option<SessionRecord>, ApplicationError<R::Error>> {
        let mut candidate = self.engine.clone();
        let terminal = candidate.apply(command, self.clock.sample())?;
        let checkpoint = candidate.checkpoint();
        if checkpoint == self.engine.checkpoint() && terminal.is_none() {
            return Ok(None);
        }
        let revision = self
            .repository
            .save_timer(Some(self.revision), &checkpoint, terminal.as_ref())
            .map_err(ApplicationError::Repository)?;
        self.engine = candidate;
        self.revision = revision;
        Ok(terminal)
    }

    pub fn state(&self) -> &TimerState {
        self.engine.state()
    }

    pub fn checkpoint(&self) -> Checkpoint {
        self.engine.checkpoint()
    }

    pub fn revision(&self) -> u64 {
        self.revision
    }

    pub fn repository(&self) -> &R {
        &self.repository
    }

    /// Access journal operations. Timer writes should go through [`Self::dispatch`] so the
    /// service's concurrency token stays in step with durable state.
    pub fn repository_mut(&mut self) -> &mut R {
        &mut self.repository
    }

    /// Replace durable state and adopt its prevalidated engine only after the repository
    /// commits. A failed replacement leaves the current in-memory state untouched.
    pub fn replace_from_backup(
        &mut self,
        restore: impl FnOnce(&mut R) -> Result<(TimerEngine, u64), R::Error>,
    ) -> Result<(), ApplicationError<R::Error>> {
        let (engine, revision) =
            restore(&mut self.repository).map_err(ApplicationError::Repository)?;
        self.engine = engine;
        self.revision = revision;
        Ok(())
    }
}

/// Monotonic process clock for the development harness and foreground-only adapters.
///
/// A random epoch prevents comparing unrelated `Instant`s after a restart. Production platform
/// adapters must supply verified suspend-inclusive samples before promising sleep continuity.
pub struct ProcessClock {
    epoch: String,
    origin: Instant,
}

impl ProcessClock {
    pub fn new() -> Self {
        Self {
            epoch: Uuid::new_v4().to_string(),
            origin: Instant::now(),
        }
    }
}

impl Default for ProcessClock {
    fn default() -> Self {
        Self::new()
    }
}

impl Clock for ProcessClock {
    fn sample(&self) -> ClockSample {
        let utc_ms = match SystemTime::now().duration_since(UNIX_EPOCH) {
            Ok(duration) => i64::try_from(duration.as_millis()).unwrap_or(i64::MAX),
            Err(error) => -i64::try_from(error.duration().as_millis()).unwrap_or(i64::MAX),
        };
        ClockSample {
            epoch: self.epoch.clone(),
            monotonic_ms: u64::try_from(self.origin.elapsed().as_millis()).unwrap_or(u64::MAX),
            utc_ms,
        }
    }
}
