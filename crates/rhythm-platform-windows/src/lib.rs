//! Windows elapsed-clock and power lifecycle adapters for the desktop shell.
//!
//! Windows timer credit uses awake time only. Suspend callbacks latch an
//! interruption even if the event queue is full. A paired native-clock check also
//! detects sleep when callbacks are late or unavailable. An interruption changes
//! the clock epoch before further timer credit can be added. The desktop worker
//! consumes the generation to end the period and return to an untimed Break.

use rhythm_domain::{Clock, ClockSample};
use std::sync::{OnceLock, mpsc::Receiver};
use std::time::{SystemTime, UNIX_EPOCH};
use thiserror::Error;
use uuid::Uuid;

#[cfg(not(windows))]
use std::time::Instant;

#[cfg(windows)]
#[allow(unsafe_code)]
mod native;

static PROCESS_EPOCH: OnceLock<String> = OnceLock::new();

#[cfg(test)]
pub(crate) static TEST_CLOCK_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// The elapsed-time guarantee available on the current target.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ClockCapability {
    /// Windows `QueryUnbiasedInterruptTimePrecise`: excludes sleep and hibernation.
    WindowsAwakeOnly,
    /// Portable development fallback; no device-sleep guarantee.
    PortableProcessOnly,
}

/// A process-scoped clock with Windows awake-only elapsed readings.
///
/// Clones and separately constructed instances share lifecycle generations.
/// The epoch changes after a suspend interruption; a fresh process has a new UUID.
/// The non-Windows implementation exists for portable tests and development; a
/// product must inspect [`Self::capability`] before promising sleep accounting.
#[derive(Clone, Debug)]
pub struct WindowsClock {
    epoch: String,
    #[cfg(not(windows))]
    origin: Instant,
}

impl WindowsClock {
    pub fn new() -> Self {
        #[cfg(not(windows))]
        static PROCESS_ORIGIN: OnceLock<Instant> = OnceLock::new();

        Self {
            epoch: PROCESS_EPOCH
                .get_or_init(|| Uuid::new_v4().to_string())
                .clone(),
            #[cfg(not(windows))]
            origin: *PROCESS_ORIGIN.get_or_init(Instant::now),
        }
    }

    pub const fn capability(&self) -> ClockCapability {
        if cfg!(windows) {
            ClockCapability::WindowsAwakeOnly
        } else {
            ClockCapability::PortableProcessOnly
        }
    }

    /// The latest process-wide interruption generation. This read is not consuming:
    /// every owner can compare it with its own last handled generation. Refreshing
    /// paired native clocks catches actual sleep even without native callbacks.
    pub fn interruption_generation(&self) -> u64 {
        #[cfg(windows)]
        {
            native::sample().1
        }
        #[cfg(not(windows))]
        {
            0
        }
    }

    /// Whether a native suspend has begun without a corresponding resume callback.
    /// The shell must not start another period while this flag is set.
    pub fn is_suspended(&self) -> bool {
        #[cfg(windows)]
        {
            native::is_suspended()
        }
        #[cfg(not(windows))]
        {
            false
        }
    }
}

impl Default for WindowsClock {
    fn default() -> Self {
        Self::new()
    }
}

impl Clock for WindowsClock {
    fn sample(&self) -> ClockSample {
        #[cfg(windows)]
        let (monotonic_ms, generation) = native::sample();
        #[cfg(not(windows))]
        let monotonic_ms = self.origin.elapsed().as_millis().min(u128::from(u64::MAX)) as u64;

        ClockSample {
            #[cfg(windows)]
            epoch: format!("{}:{generation}", self.epoch),
            #[cfg(not(windows))]
            epoch: self.epoch.clone(),
            monotonic_ms,
            utc_ms: utc_ms_at(SystemTime::now()),
        }
    }
}

/// A pair of reads bounds the inclusive-minus-awake bias even if the thread is
/// descheduled between API calls. The tolerance covers GetTickCount64's coarse
/// tick resolution, not arbitrary worker delays or UTC changes.
#[cfg(any(windows, test))]
#[derive(Debug, Default)]
struct SleepDetector {
    bias_upper_ms: Option<u64>,
    native_count: u64,
    native_sleep_pending: bool,
    sleep_detected: bool,
}

#[cfg(any(windows, test))]
impl SleepDetector {
    fn observe(
        &mut self,
        inclusive_ms: u64,
        awake_before_ms: u64,
        awake_after_ms: u64,
        native_count: u64,
        suspended: bool,
    ) -> bool {
        const TICK_TOLERANCE_MS: u64 = 50;
        if native_count != self.native_count {
            self.native_count = native_count;
            self.native_sleep_pending = true;
        }
        let lower = inclusive_ms.saturating_sub(awake_after_ms);
        let upper = inclusive_ms.saturating_sub(awake_before_ms);
        let gap = self
            .bias_upper_ms
            .is_some_and(|previous| lower > previous.saturating_add(TICK_TOLERANCE_MS));
        self.sleep_detected = gap;
        let missed_native_sleep = gap && !self.native_sleep_pending;
        self.bias_upper_ms = Some(if gap {
            upper
        } else {
            // A long scheduling delay widens a read's bounds. It must not replace
            // an earlier tighter bound and hide a later genuine sleep interval.
            self.bias_upper_ms
                .map_or(upper, |previous| previous.min(upper))
        });
        if gap || !suspended {
            self.native_sleep_pending = false;
        }
        missed_native_sleep
    }
}

fn utc_ms_at(time: SystemTime) -> i64 {
    match time.duration_since(UNIX_EPOCH) {
        Ok(delta) => delta.as_millis().min(i64::MAX as u128) as i64,
        Err(error) => -(error.duration().as_millis().min(i64::MAX as u128) as i64),
    }
}

/// A request to reconcile the timer on the application's worker thread.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PowerEvent {
    Suspending,
    Resumed,
}

#[derive(Debug, Error)]
pub enum PowerError {
    #[error("Native power notifications are available only on Windows")]
    UnsupportedPlatform,
    #[error("This process already has a power notification subscription")]
    AlreadySubscribed,
    #[error("The power notification subscription state is unavailable")]
    SubscriptionStateUnavailable,
    #[error("Windows could not register power notifications (error {0})")]
    RegistrationFailed(u32),
    #[error("Windows could not unregister power notifications (error {0})")]
    UnregistrationFailed(u32),
}

/// Keeps one process-wide native suspend/resume subscription alive.
///
/// The receiver has capacity eight. The OS callback only attempts a nonblocking
/// send; it never calls application code, touches storage, or waits for the timer
/// worker. The interruption generation is latched before enqueueing, so a full
/// queue cannot lose the interruption. Regular lifecycle checks remain required.
/// Drop unregisters; [`Self::close`] can report an unregister failure explicitly.
#[derive(Debug)]
pub struct PowerSubscription {
    #[cfg(windows)]
    registration: native::Registration,
}

impl PowerSubscription {
    pub fn subscribe() -> Result<(Self, Receiver<PowerEvent>), PowerError> {
        #[cfg(windows)]
        {
            let (registration, receiver) = native::Registration::subscribe()?;
            Ok((Self { registration }, receiver))
        }
        #[cfg(not(windows))]
        {
            Err(PowerError::UnsupportedPlatform)
        }
    }

    pub fn close(mut self) -> Result<(), PowerError> {
        #[cfg(windows)]
        {
            self.registration.close()
        }
        #[cfg(not(windows))]
        {
            let _ = &mut self;
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn clock_instances_share_a_process_epoch_and_never_reverse() {
        let _guard = TEST_CLOCK_LOCK.lock().unwrap();
        let clock = WindowsClock::new();
        let first = clock.sample();
        let second = WindowsClock::new().sample();
        assert_eq!(first.epoch, second.epoch);
        assert_eq!(first.epoch, clock.clone().sample().epoch);
        assert!(second.monotonic_ms >= first.monotonic_ms);
        assert!(Uuid::parse_str(first.epoch.split(':').next().unwrap()).is_ok());
        #[cfg(windows)]
        assert_eq!(clock.capability(), ClockCapability::WindowsAwakeOnly);
        #[cfg(not(windows))]
        assert_eq!(clock.capability(), ClockCapability::PortableProcessOnly);
    }

    #[test]
    fn paired_clocks_detect_sleep_without_native_callbacks_only_once() {
        let mut detector = SleepDetector::default();
        // Existing machine sleep history is a baseline, never a new interruption.
        assert!(!detector.observe(1_000_000, 100_000, 100_001, 0, false));
        assert!(!detector.observe(1_001_000, 101_000, 101_001, 0, false));
        assert!(detector.observe(1_061_100, 101_100, 101_101, 0, false));
        assert!(!detector.observe(1_062_100, 102_100, 102_101, 0, false));
    }

    #[test]
    fn awake_worker_delays_and_read_preemption_do_not_look_like_sleep() {
        let mut detector = SleepDetector::default();
        assert!(!detector.observe(10_000, 10_000, 10_001, 0, false));
        // Both clocks advance while an awake process is throttled or blocked.
        assert!(!detector.observe(610_000, 610_000, 610_001, 0, false));
        // Native reads straddle a long thread scheduling delay.
        assert!(!detector.observe(612_000, 610_002, 614_000, 0, false));
        assert!(!detector.observe(614_001, 614_001, 614_002, 0, false));
        // A real gap is still detectable after that wide observation.
        assert!(detector.observe(674_002, 614_002, 614_003, 0, false));
    }

    #[test]
    fn native_and_clock_evidence_for_one_sleep_do_not_double_interrupt() {
        let mut detector = SleepDetector::default();
        assert!(!detector.observe(10_000, 10_000, 10_001, 0, false));
        assert!(!detector.observe(10_100, 10_100, 10_101, 1, true));
        assert!(!detector.observe(70_200, 10_200, 10_201, 1, false));
        assert!(!detector.observe(70_300, 10_300, 10_301, 1, false));
        assert!(detector.observe(130_400, 10_400, 10_401, 1, false));
    }

    #[test]
    fn cancelled_suspend_does_not_hide_a_later_unreported_sleep() {
        let mut detector = SleepDetector::default();
        assert!(!detector.observe(10_000, 10_000, 10_001, 0, false));
        assert!(!detector.observe(10_100, 10_100, 10_101, 1, true));
        assert!(!detector.observe(10_200, 10_200, 10_201, 1, false));
        assert!(detector.observe(70_300, 10_300, 10_301, 1, false));
    }

    #[test]
    fn utc_observations_support_dates_before_the_unix_epoch() {
        assert_eq!(utc_ms_at(UNIX_EPOCH + Duration::from_millis(123)), 123);
        assert_eq!(utc_ms_at(UNIX_EPOCH - Duration::from_millis(123)), -123);
    }

    #[cfg(not(windows))]
    #[test]
    fn power_subscription_reports_unsupported_on_other_targets() {
        assert!(matches!(
            PowerSubscription::subscribe(),
            Err(PowerError::UnsupportedPlatform)
        ));
    }
}
