//! The native Windows session-ending boundary.
//!
//! Tao exits the process directly from WM_ENDSESSION. Install this subclass on
//! the main window's owning thread so the worker and WebView have a bounded
//! opportunity to commit interrupted state and pending writing before that exit.
//! No message handler cancels or indefinitely blocks Windows shutdown.

use std::{
    panic::{AssertUnwindSafe, catch_unwind},
    sync::{
        Arc, Mutex,
        mpsc::{Receiver, TryRecvError},
    },
    time::{Duration, Instant},
};
use tauri::WebviewWindow;
use windows::Win32::{
    Foundation::{HWND, LPARAM, LRESULT, WPARAM},
    UI::{
        Shell::{DefSubclassProc, GetWindowSubclass, RemoveWindowSubclass, SetWindowSubclass},
        WindowsAndMessaging::{
            DispatchMessageW, MSG, MWMO_INPUTAVAILABLE, MsgWaitForMultipleObjectsEx, PM_REMOVE,
            PeekMessageW, PostQuitMessage, QS_ALLINPUT, TranslateMessage, WM_ENDSESSION,
            WM_NCDESTROY, WM_QUERYENDSESSION, WM_QUIT,
        },
    },
};

const SUBCLASS_ID: usize = 0x5248_5954;
const SHUTDOWN_SAVE_BUDGET: Duration = Duration::from_secs(2);

#[derive(Default)]
struct Preparation {
    prepared: bool,
    ending: bool,
    cancelled: bool,
    receiver: Option<Receiver<()>>,
}

impl Preparation {
    fn needs_prepare(&mut self) -> bool {
        if self.prepared || self.ending {
            return false;
        }
        self.prepared = true;
        self.cancelled = false;
        true
    }

    /// None means another WM_ENDSESSION handler already owns the wait. Such a
    /// reentrant message must not reach Tao's immediate process-exit handler.
    fn begin_end(&mut self) -> Option<Option<Receiver<()>>> {
        if self.ending {
            return None;
        }
        self.ending = true;
        Some(self.receiver.take())
    }

    fn cancel(&mut self) {
        self.cancelled = true;
        self.prepared = false;
        self.receiver = None;
    }

    fn finish_end(&mut self) -> bool {
        self.ending = false;
        !self.cancelled
    }
}

struct Context {
    prepare: Box<dyn Fn() -> Receiver<()> + Send + Sync>,
    cancel: Box<dyn Fn() + Send + Sync>,
    state: Mutex<Preparation>,
}

impl Context {
    fn prepare_if_needed(&self) {
        let needed = self
            .state
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .needs_prepare();
        if needed {
            // Never hold the gate across application callbacks or a message pump.
            let receiver = (self.prepare)();
            self.state
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .receiver = Some(receiver);
        }
    }

    fn cancelled(&self) -> bool {
        self.state
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .cancelled
    }
}

/// Install once from Tauri's setup hook on the main window's owning thread.
///
/// `prepare` must return promptly. It queues the native interruption and asks
/// the frontend to flush writing, then returns a receiver acknowledged only
/// after the flush and its preceding database writes complete. A cancelled
/// shutdown does not undo the interruption or resume Focus.
pub(crate) fn install(
    window: &WebviewWindow,
    prepare: impl Fn() -> Receiver<()> + Send + Sync + 'static,
    cancel: impl Fn() + Send + Sync + 'static,
) -> Result<(), String> {
    let raw = window
        .hwnd()
        .map_err(|_| "Rhythm could not access its shutdown window.")?;
    let hwnd = HWND(raw.0);
    // SAFETY: setup runs on this live window's owning GUI thread. The callback
    // address and fixed identifier are private to this module.
    if unsafe { GetWindowSubclass(hwnd, Some(session_proc), SUBCLASS_ID, None) }.as_bool() {
        return Err("Rhythm's shutdown handler is already installed.".into());
    }
    let context = Arc::new(Context {
        prepare: Box::new(prepare),
        cancel: Box::new(cancel),
        state: Mutex::default(),
    });
    let reference = Arc::into_raw(context);
    // SAFETY: The subclass owns this strong reference until WM_NCDESTROY; every
    // callback obtains its own temporary Arc before pumping reentrant messages.
    if !unsafe { SetWindowSubclass(hwnd, Some(session_proc), SUBCLASS_ID, reference as usize) }
        .as_bool()
    {
        // SAFETY: Installation failed, so no callback owns this reference.
        drop(unsafe { Arc::from_raw(reference) });
        return Err("Windows could not install Rhythm's shutdown handler.".into());
    }
    Ok(())
}

unsafe extern "system" fn session_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    id: usize,
    reference: usize,
) -> LRESULT {
    if reference == 0 {
        // SAFETY: Forward this unchanged native message through the subclass chain.
        return unsafe { DefSubclassProc(hwnd, message, wparam, lparam) };
    }
    let pointer = reference as *const Context;
    // SAFETY: The installed subclass owns one Arc reference. Holding a temporary
    // reference prevents destruction during a nested WM_NCDESTROY while waiting.
    unsafe { Arc::increment_strong_count(pointer) };
    let context = unsafe { Arc::from_raw(pointer) };
    let handled = catch_unwind(AssertUnwindSafe(|| match message {
        WM_QUERYENDSESSION => {
            context.prepare_if_needed();
            // Do not block the shutdown inquiry or show a confirmation dialog.
            Some(LRESULT(1))
        }
        WM_ENDSESSION if wparam.0 == 0 => {
            context
                .state
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .cancel();
            (context.cancel)();
            None
        }
        WM_ENDSESSION => {
            context.prepare_if_needed();
            let wait = context
                .state
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .begin_end();
            let Some(receiver) = wait else {
                return Some(LRESULT(0));
            };
            if let Some(receiver) = receiver {
                wait_for_save(
                    &receiver,
                    SHUTDOWN_SAVE_BUDGET,
                    || context.cancelled(),
                    pump_messages,
                );
            }
            let should_exit = context
                .state
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .finish_end();
            if should_exit { None } else { Some(LRESULT(0)) }
        }
        WM_NCDESTROY => {
            // SAFETY: Remove exactly this callback on its owning window thread.
            // A failed removal deliberately retains the strong reference rather
            // than risk freeing context still reachable by Windows.
            if unsafe { RemoveWindowSubclass(hwnd, Some(session_proc), id) }.as_bool() {
                drop(unsafe { Arc::from_raw(pointer) });
            }
            None
        }
        _ => None,
    }));
    match handled {
        Ok(Some(result)) => result,
        // A failed best-effort save must not unwind through Windows or prevent
        // shutdown indefinitely. Durable notes remain available on next launch.
        _ => unsafe { DefSubclassProc(hwnd, message, wparam, lparam) },
    }
}

fn wait_for_save(
    receiver: &Receiver<()>,
    budget: Duration,
    cancelled: impl Fn() -> bool,
    mut pump: impl FnMut(Duration) -> bool,
) {
    let deadline = Instant::now() + budget;
    loop {
        match receiver.try_recv() {
            Ok(()) | Err(TryRecvError::Disconnected) => return,
            Err(TryRecvError::Empty) => {}
        }
        if cancelled() {
            return;
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() || !pump(remaining.min(Duration::from_millis(10))) {
            return;
        }
    }
}

fn pump_messages(wait: Duration) -> bool {
    let deadline = Instant::now() + wait;
    let mut message = MSG::default();
    // SAFETY: This runs on the existing GUI thread. A bounded nested pump keeps
    // WebView IPC and Tauri's queued main-thread callbacks moving while saving.
    unsafe {
        let _ = MsgWaitForMultipleObjectsEx(
            None,
            wait.as_millis() as u32,
            QS_ALLINPUT,
            MWMO_INPUTAVAILABLE,
        );
        while Instant::now() < deadline
            && PeekMessageW(&mut message, None, 0, 0, PM_REMOVE).as_bool()
        {
            if message.message == WM_QUIT {
                // Preserve the surrounding event loop's quit signal.
                PostQuitMessage(message.wParam.0 as i32);
                return false;
            }
            let _ = TranslateMessage(&message);
            let _ = DispatchMessageW(&message);
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{cell::Cell, sync::mpsc};

    #[test]
    fn repeated_and_reentrant_session_end_wait_only_once() {
        let mut gate = Preparation::default();
        assert!(gate.needs_prepare());
        assert!(!gate.needs_prepare());
        let (sender, receiver) = mpsc::channel();
        gate.receiver = Some(receiver);
        assert!(gate.begin_end().unwrap().is_some());
        assert!(gate.begin_end().is_none());
        assert!(!gate.needs_prepare());
        sender.send(()).unwrap_err();
        assert!(gate.finish_end());
    }

    #[test]
    fn cancelled_shutdown_does_not_reuse_an_old_acknowledgement() {
        let mut gate = Preparation::default();
        assert!(gate.needs_prepare());
        let (_sender, receiver) = mpsc::channel();
        gate.receiver = Some(receiver);
        gate.cancel();
        assert!(gate.receiver.is_none());
        assert!(gate.needs_prepare());
        assert!(!gate.cancelled);
        assert!(gate.begin_end().is_some());
        gate.cancel();
        assert!(!gate.finish_end());
    }

    #[test]
    fn ui_messages_can_deliver_a_save_ack_without_blocking_the_ui_thread() {
        let (sender, receiver) = mpsc::channel();
        let pumps = Cell::new(0);
        wait_for_save(
            &receiver,
            Duration::from_secs(1),
            || false,
            |_| {
                pumps.set(pumps.get() + 1);
                sender.send(()).unwrap();
                true
            },
        );
        assert_eq!(pumps.get(), 1);
    }

    #[test]
    fn a_missing_or_failed_save_never_waits_without_a_bound() {
        let (sender, receiver) = mpsc::channel();
        wait_for_save(
            &receiver,
            Duration::ZERO,
            || false,
            |_| panic!("expired wait must not pump"),
        );
        drop(sender);
        wait_for_save(
            &receiver,
            Duration::from_secs(1),
            || false,
            |_| panic!("closed channel must not pump"),
        );
        let (_sender, receiver) = mpsc::channel();
        wait_for_save(
            &receiver,
            Duration::from_secs(1),
            || true,
            |_| panic!("cancelled wait must not pump"),
        );
    }
}
