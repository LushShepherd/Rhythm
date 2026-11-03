//! Coordinates a UI draft flush with a durable worker quit and the final CLI reply.
//! No worker lock is held while the WebView saves through that same worker.
use std::sync::{Condvar, Mutex, MutexGuard};
use std::time::Duration;

#[derive(Default)]
enum Phase {
    #[default]
    Idle,
    UiQuit,
    Preparing {
        token: String,
        result: Option<Result<(), String>>,
    },
    Finishing,
    Closing {
        request_id: String,
    },
    Exiting,
}

#[derive(Default)]
pub(crate) struct QuitCoordinator {
    phase: Mutex<Phase>,
    prepared: Condvar,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum PrepareError {
    Refused(String),
    Timeout,
}

impl QuitCoordinator {
    pub(crate) fn is_idle(&self) -> bool {
        matches!(*self.lock(), Phase::Idle)
    }

    fn lock(&self) -> MutexGuard<'_, Phase> {
        self.phase.lock().unwrap_or_else(|error| error.into_inner())
    }

    pub(crate) fn begin_cli(&self, token: String) -> Result<(), String> {
        let mut phase = self.lock();
        if !matches!(*phase, Phase::Idle) {
            return Err("Rhythm is already preparing to quit.".into());
        }
        *phase = Phase::Preparing {
            token,
            result: None,
        };
        Ok(())
    }

    pub(crate) fn prepared(&self, token: &str, result: Result<(), String>) -> Result<(), String> {
        let mut phase = self.lock();
        let Phase::Preparing {
            token: pending,
            result: saved,
        } = &mut *phase
        else {
            return Err("This quit request is no longer waiting for a save.".into());
        };
        if pending != token || saved.is_some() {
            return Err("This quit request is no longer waiting for a save.".into());
        }
        *saved = Some(result);
        self.prepared.notify_one();
        Ok(())
    }

    /// The deadline and readiness receipt are resolved under one lock. A late UI
    /// receipt cannot revive a cancelled request and silently close the app later.
    pub(crate) fn wait_prepared(&self, token: &str, timeout: Duration) -> Result<(), PrepareError> {
        let phase = self.lock();
        let (mut phase, _) = self.prepared.wait_timeout_while(phase, timeout, |phase| {
            matches!(phase, Phase::Preparing { token: pending, result: None } if pending == token)
        }).unwrap_or_else(|error| error.into_inner());
        match &mut *phase {
            Phase::Preparing {
                token: pending,
                result,
            } if pending == token => {
                let result = result.take();
                *phase = if result == Some(Ok(())) {
                    Phase::Finishing
                } else {
                    Phase::Idle
                };
                match result {
                    Some(Ok(())) => Ok(()),
                    Some(Err(message)) => Err(PrepareError::Refused(message)),
                    None => Err(PrepareError::Timeout),
                }
            }
            _ => Err(PrepareError::Refused(
                "This quit request is no longer active.".into(),
            )),
        }
    }

    pub(crate) fn cancel_preparing(&self, token: &str) {
        let mut phase = self.lock();
        if matches!(&*phase, Phase::Preparing { token: pending, .. } if pending == token) {
            *phase = Phase::Idle;
            self.prepared.notify_one();
        }
    }

    pub(crate) fn finish_failed(&self) {
        let mut phase = self.lock();
        if matches!(*phase, Phase::Finishing) {
            *phase = Phase::Idle;
        }
    }

    /// Called only after the worker confirms its final durable save and stops.
    pub(crate) fn committed(&self, request_id: String) {
        *self.lock() = Phase::Closing { request_id };
    }

    /// Exit only once, after transport has attempted delivery and waited for its
    /// bounded acknowledgement. Failure to receive the reply does not undo a quit.
    pub(crate) fn take_exit(&self, request_id: &str) -> bool {
        let mut phase = self.lock();
        if matches!(&*phase, Phase::Closing { request_id: pending } if pending == request_id) {
            *phase = Phase::Exiting;
            return true;
        }
        false
    }

    pub(crate) fn begin_ui(&self) -> Result<(), String> {
        let mut phase = self.lock();
        if !matches!(*phase, Phase::Idle) {
            return Err(
                "A command-line quit is already in progress. Please wait for it to finish.".into(),
            );
        }
        *phase = Phase::UiQuit;
        Ok(())
    }

    pub(crate) fn ui_failed(&self) {
        let mut phase = self.lock();
        if matches!(*phase, Phase::UiQuit) {
            *phase = Phase::Idle;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stale_or_duplicate_readiness_cannot_finish_another_request() {
        let quit = QuitCoordinator::default();
        quit.begin_cli("current".into()).unwrap();
        assert!(quit.prepared("old", Ok(())).is_err());
        assert!(quit.begin_ui().is_err());
        assert!(quit.begin_cli("other".into()).is_err());
        quit.prepared("current", Ok(())).unwrap();
        assert!(quit.prepared("current", Ok(())).is_err());
        quit.wait_prepared("current", Duration::ZERO).unwrap();
        assert!(quit.begin_ui().is_err());
        quit.committed("request-id".into());
        assert!(!quit.take_exit("different"));
        assert!(quit.take_exit("request-id"));
        assert!(!quit.take_exit("request-id"));
    }

    #[test]
    fn timeout_invalidates_late_readiness_and_allows_a_new_quit() {
        let quit = QuitCoordinator::default();
        quit.begin_cli("expired".into()).unwrap();
        assert_eq!(
            quit.wait_prepared("expired", Duration::ZERO),
            Err(PrepareError::Timeout)
        );
        assert!(quit.prepared("expired", Ok(())).is_err());
        quit.begin_cli("new".into()).unwrap();
        assert!(quit.wait_prepared("expired", Duration::ZERO).is_err());
        quit.prepared("new", Ok(())).unwrap();
        quit.wait_prepared("new", Duration::ZERO).unwrap();
    }

    #[test]
    fn save_rejection_and_worker_failure_never_enable_exit() {
        let quit = QuitCoordinator::default();
        quit.begin_cli("failed-save".into()).unwrap();
        quit.prepared("failed-save", Err("Could not save".into()))
            .unwrap();
        assert_eq!(
            quit.wait_prepared("failed-save", Duration::ZERO),
            Err(PrepareError::Refused("Could not save".into()))
        );
        assert!(!quit.take_exit("failed-save"));
        quit.begin_cli("worker-failure".into()).unwrap();
        quit.prepared("worker-failure", Ok(())).unwrap();
        quit.wait_prepared("worker-failure", Duration::ZERO)
            .unwrap();
        quit.finish_failed();
        assert!(!quit.take_exit("worker-failure"));
        quit.begin_ui().unwrap();
    }

    #[test]
    fn ui_and_cli_quit_cannot_race_the_worker_stop() {
        let quit = QuitCoordinator::default();
        quit.begin_ui().unwrap();
        assert!(quit.begin_cli("cli".into()).is_err());
        quit.ui_failed();
        quit.begin_cli("cli".into()).unwrap();
        quit.cancel_preparing("wrong");
        assert!(quit.begin_ui().is_err());
        quit.cancel_preparing("cli");
        quit.begin_ui().unwrap();
    }
}
