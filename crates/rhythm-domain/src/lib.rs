//! Rhythm's deterministic, GUI-independent timer domain.
//!
//! Time arrives as explicit [`ClockSample`] values. The engine never sleeps,
//! reads the system clock, starts a background task, or starts another period
//! after completion. Persist a new [`Checkpoint`] and any returned terminal
//! [`SessionRecord`] in one transaction before publishing the state to a UI.
//!
//! Clock epochs are trust boundaries: an adapter must change its epoch when
//! elapsed samples are no longer comparable. A restored running timer presented
//! with another epoch asks for recovery rather than estimating elapsed time from
//! the wall clock. Native suspend-inclusive clock adapters are a separate phase.

mod routines;
mod timer;

pub use routines::*;
pub use timer::*;
