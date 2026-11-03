//! The complete unsafe boundary. No application state crosses the FFI boundary.

use crate::{PowerError, PowerEvent, SleepDetector};
use std::ffi::c_void;
use std::ptr;
use std::sync::{
    Mutex,
    atomic::{AtomicU64, Ordering},
    mpsc::{Receiver, SyncSender, sync_channel},
};
use windows_sys::Win32::{
    Foundation::ERROR_SUCCESS,
    System::{
        Power::{
            DEVICE_NOTIFY_SUBSCRIBE_PARAMETERS, HPOWERNOTIFY,
            PowerRegisterSuspendResumeNotification, PowerUnregisterSuspendResumeNotification,
        },
        SystemInformation::GetTickCount64,
        WindowsProgramming::QueryUnbiasedInterruptTimePrecise,
    },
    UI::WindowsAndMessaging::{
        DEVICE_NOTIFY_CALLBACK, PBT_APMRESUMEAUTOMATIC, PBT_APMRESUMESUSPEND, PBT_APMSUSPEND,
    },
};

// This context lives until process exit. The callback never dereferences the
// opaque OS context or touches a heap allocation owned by a registration guard.
// This remains memory-safe even if Windows has an in-flight callback at close.
static SINK: Mutex<Option<SyncSender<PowerEvent>>> = Mutex::new(None);
// Bit zero is suspended; the upper bits count distinct suspend callbacks. One
// atomic value lets clock sampling distinguish a completed sleep from a newer
// suspend arriving concurrently. These process-lifetime statics own no app data.
static POWER_STATE: AtomicU64 = AtomicU64::new(0);
static INTERRUPTION_GENERATION: AtomicU64 = AtomicU64::new(0);
static SLEEP_DETECTOR: Mutex<SleepDetector> = Mutex::new(SleepDetector {
    bias_upper_ms: None,
    native_count: 0,
    native_sleep_pending: false,
    sleep_detected: false,
});

pub(super) fn sample() -> (u64, u64) {
    // Serialize the three reads too: samples from multiple webview/worker threads
    // must not reach the detector out of order. A callback never takes this lock.
    let mut detector = SLEEP_DETECTOR
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let mut awake_before = 0_u64;
    let mut awake_after = 0_u64;
    // SAFETY: Both output pointers address live aligned u64 storage. The APIs do
    // not retain them, require no initialization, and are supported on Windows 10+.
    // GetTickCount64 takes no parameters. These APIs only read elapsed counters.
    let inclusive = unsafe {
        QueryUnbiasedInterruptTimePrecise(&mut awake_before);
        let inclusive = GetTickCount64();
        QueryUnbiasedInterruptTimePrecise(&mut awake_after);
        inclusive
    };
    let state = POWER_STATE.load(Ordering::SeqCst);
    if detector.observe(
        inclusive,
        awake_before / 10_000,
        awake_after / 10_000,
        state >> 1,
        state & 1 != 0,
    ) {
        INTERRUPTION_GENERATION.fetch_add(1, Ordering::SeqCst);
    }
    if detector.sleep_detected {
        // A real sleep gap proves that this thread is awake again even if the
        // resume callback never arrived. Do not clear a newer concurrent suspend.
        let _ = POWER_STATE.compare_exchange(state, state & !1, Ordering::SeqCst, Ordering::SeqCst);
    }
    (
        // Use the first awake read for credit. If sleep lands between the
        // inclusive read and the final awake read, the gap becomes visible on
        // the next sample, but this sample still predates that interruption.
        awake_before / 10_000,
        INTERRUPTION_GENERATION.load(Ordering::SeqCst),
    )
}

pub(super) fn is_suspended() -> bool {
    POWER_STATE.load(Ordering::SeqCst) & 1 != 0
}

#[derive(Debug)]
pub(super) struct Registration {
    handle: Option<HPOWERNOTIFY>,
}

impl Registration {
    pub(super) fn subscribe() -> Result<(Self, Receiver<PowerEvent>), PowerError> {
        let mut sink = SINK
            .lock()
            .map_err(|_| PowerError::SubscriptionStateUnavailable)?;
        if sink.is_some() {
            return Err(PowerError::AlreadySubscribed);
        }
        let (sender, receiver) = sync_channel(8);
        let parameters = DEVICE_NOTIFY_SUBSCRIBE_PARAMETERS {
            Callback: Some(power_callback),
            Context: ptr::null_mut(),
        };
        let mut handle = ptr::null_mut();
        // SAFETY: Windows reads the correctly typed input structure during the
        // registration call. Its callback has the required system ABI and lives
        // for the process lifetime. The context is null and never dereferenced.
        // The out pointer refers to live writable storage for the returned handle.
        let result = unsafe {
            PowerRegisterSuspendResumeNotification(
                DEVICE_NOTIFY_CALLBACK,
                (&parameters as *const DEVICE_NOTIFY_SUBSCRIBE_PARAMETERS)
                    .cast_mut()
                    .cast(),
                &mut handle,
            )
        };
        if result != ERROR_SUCCESS {
            return Err(PowerError::RegistrationFailed(result));
        }
        *sink = Some(sender);
        Ok((
            Self {
                handle: Some(handle as HPOWERNOTIFY),
            },
            receiver,
        ))
    }

    pub(super) fn close(&mut self) -> Result<(), PowerError> {
        let Some(handle) = self.handle.take() else {
            return Ok(());
        };
        // Serialize registration and unregistration. The callback uses try_lock,
        // so it cannot block this call or deadlock with an OS unregister wait.
        let mut sink = SINK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        // SAFETY: This handle came from one successful native registration and
        // is consumed at most once. The global callback state remains live even
        // if Windows reports an error or has a callback already in flight.
        let result = unsafe { PowerUnregisterSuspendResumeNotification(handle) };
        sink.take();
        if result == ERROR_SUCCESS {
            Ok(())
        } else {
            Err(PowerError::UnregistrationFailed(result))
        }
    }
}

impl Drop for Registration {
    fn drop(&mut self) {
        let _ = self.close();
    }
}

unsafe extern "system" fn power_callback(
    _context: *const c_void,
    event_type: u32,
    _setting: *const c_void,
) -> u32 {
    let event = match event_type {
        PBT_APMSUSPEND => PowerEvent::Suspending,
        PBT_APMRESUMEAUTOMATIC | PBT_APMRESUMESUSPEND => PowerEvent::Resumed,
        _ => return ERROR_SUCCESS,
    };
    match event {
        PowerEvent::Suspending => {
            if POWER_STATE
                .try_update(Ordering::SeqCst, Ordering::SeqCst, |state| {
                    (state & 1 == 0).then_some(state.wrapping_add(3))
                })
                .is_ok()
            {
                INTERRUPTION_GENERATION.fetch_add(1, Ordering::SeqCst);
            }
        }
        PowerEvent::Resumed => {
            POWER_STATE.fetch_and(!1, Ordering::SeqCst);
        }
    }
    // No dereference, allocation, blocking lock, user callback, or panic path.
    // The atomic generation above preserves an interruption even when the queue
    // or its try_lock drops a signal. The worker checks it before timer actions.
    if let Ok(sink) = SINK.try_lock()
        && let Some(sender) = sink.as_ref()
    {
        let _ = sender.try_send(event);
    }
    ERROR_SUCCESS
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc::TryRecvError;

    #[test]
    fn native_subscription_is_bounded_and_unregisters_without_suspending() {
        let _guard = crate::TEST_CLOCK_LOCK.lock().unwrap();
        let (mut registration, receiver) = Registration::subscribe().unwrap();
        assert!(matches!(
            Registration::subscribe(),
            Err(PowerError::AlreadySubscribed)
        ));
        // SAFETY: Directly invoke our callback with the documented event codes.
        // Both pointer arguments are unused. No machine power state is changed.
        unsafe {
            power_callback(ptr::null(), PBT_APMSUSPEND, ptr::null());
            power_callback(ptr::null(), PBT_APMRESUMEAUTOMATIC, ptr::null());
            power_callback(ptr::null(), PBT_APMRESUMESUSPEND, ptr::null());
        }
        assert_eq!(receiver.try_recv(), Ok(PowerEvent::Suspending));
        assert_eq!(receiver.try_recv(), Ok(PowerEvent::Resumed));
        let generation_before_full_queue = INTERRUPTION_GENERATION.load(Ordering::SeqCst);
        assert_eq!(receiver.try_recv(), Ok(PowerEvent::Resumed));
        for _ in 0..32 {
            // SAFETY: Same callback-only simulation as above.
            unsafe {
                power_callback(ptr::null(), PBT_APMRESUMEAUTOMATIC, ptr::null());
            }
        }
        assert_eq!(receiver.try_iter().count(), 8);
        // Refill the queue; a dropped suspend hint still changes the clock epoch.
        for _ in 0..8 {
            // SAFETY: Simulates our callback only; never changes OS power state.
            unsafe {
                power_callback(ptr::null(), PBT_APMRESUMEAUTOMATIC, ptr::null());
            }
        }
        // SAFETY: Both pointers are unused by our callback.
        unsafe {
            power_callback(ptr::null(), PBT_APMSUSPEND, ptr::null());
            power_callback(ptr::null(), PBT_APMSUSPEND, ptr::null());
        }
        assert!(is_suspended());
        assert_eq!(
            INTERRUPTION_GENERATION.load(Ordering::SeqCst),
            generation_before_full_queue + 1
        );
        assert_eq!(receiver.try_iter().count(), 8);
        // SAFETY: Same callback-only simulation, restores the test state.
        unsafe {
            power_callback(ptr::null(), PBT_APMRESUMEAUTOMATIC, ptr::null());
        }
        assert!(!is_suspended());
        receiver.try_iter().for_each(drop);
        registration.close().unwrap();
        assert_eq!(receiver.try_recv(), Err(TryRecvError::Disconnected));
        registration.close().unwrap();
    }
}
