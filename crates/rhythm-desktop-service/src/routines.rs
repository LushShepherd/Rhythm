use rhythm_domain::{Clock, Invitation, Routine, RoutineInput};
use serde::{Deserialize, Serialize};

use crate::{DesktopService, ServiceError};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoutineNext {
    pub id: String,
    pub next_utc_ms: Option<i64>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoutineView {
    pub routines: Vec<Routine>,
    pub invitations: Vec<Invitation>,
    /// Active daily windows survive notification delivery and app restarts.
    pub routine_windows: Vec<Invitation>,
    /// An explicit choice supplies the next focus preset while this window lasts.
    /// This selection is local to the running process; no timer starts on reopen.
    pub selected_routine_window: Option<Invitation>,
    pub routine_next: Vec<RoutineNext>,
    pub routine_error: Option<String>,
    pub routines_revision: u64,
}

impl<C: Clock> DesktopService<C> {
    pub fn ready_for_invitation(&self) -> bool {
        matches!(
            self.application.state(),
            rhythm_domain::TimerState::Ready { .. }
        )
    }
    /// Override device-zone discovery for deterministic tests or an embedding host.
    pub fn set_system_timezone(&mut self, zone: String) {
        self.system_zone_override = Some(zone);
        self.poll_routines();
    }

    fn system_timezone(&self, needs_system: bool) -> Result<String, rhythm_storage::Error> {
        if !needs_system {
            return Ok("UTC".into());
        }
        if let Some(zone) = &self.system_zone_override {
            return Ok(zone.clone());
        }
        iana_time_zone::get_timezone().map_err(|_| {
            rhythm_storage::Error::Routine(rhythm_domain::RoutineError::TimeZone(
                "The device time zone is unavailable. Choose a named time zone for your routines."
                    .into(),
            ))
        })
    }

    pub(crate) fn bump_routines(&mut self) {
        self.routine_state.routines_revision =
            self.routine_state.routines_revision.saturating_add(1);
    }

    /// Scheduling failure is visible but cannot block the timer or journal.
    /// Delivery is claimed durably before publication. Restart never replays a claimed
    /// notification. Active daily windows are independently reconstructed, so reopening
    /// can offer the remaining window without showing another native notification.
    pub fn poll_routines(&mut self) -> RoutineView {
        let before = self.routine_state.clone();
        let now = self.clock.sample().utc_ms;
        self.routine_state
            .invitations
            .retain(|item| now >= item.due_at_utc_ms && now < item.expires_at_utc_ms);
        match self.refresh_routines(now) {
            Ok(()) => self.routine_state.routine_error = None,
            Err(error) => {
                self.routine_state.routine_next.clear();
                self.routine_state.invitations.clear();
                self.routine_state.routine_windows.clear();
                self.routine_state.selected_routine_window = None;
                self.routine_state.routine_error = Some(error.to_string());
            }
        }
        if self.routine_state != before {
            self.bump_routines();
        }
        self.routine_state.clone()
    }

    fn refresh_routines(&mut self, now: i64) -> Result<(), rhythm_storage::Error> {
        let routines = self.application.repository().list_routines()?;
        // Keep definitions editable even if device-zone resolution fails, so the
        // person can switch a routine to a named zone or disable it.
        self.routine_state.routines = routines.clone();
        let zone = self.system_timezone(
            routines
                .iter()
                .any(|item| item.input.enabled && item.input.timezone == "system"),
        )?;
        let next = routines
            .iter()
            .map(|item| {
                Ok(RoutineNext {
                    id: item.id.clone(),
                    next_utc_ms: self
                        .application
                        .repository()
                        .next_routine_invitation(item, now, &zone)?
                        .map(|due| due.due_at_utc_ms),
                })
            })
            .collect::<Result<Vec<_>, rhythm_storage::Error>>()?;
        let mut retained = Vec::new();
        for invitation in &self.routine_state.invitations {
            if let Some(routine) = routines
                .iter()
                .find(|item| item.id == invitation.routine_id)
                && routine.due_invitation(now, &zone)?.as_ref() == Some(invitation)
            {
                retained.push(invitation.clone());
            }
        }
        let due = self
            .application
            .repository_mut()
            .take_due_invitations(now, &zone)?;
        let windows = self
            .application
            .repository()
            .active_routine_windows(now, &zone)?;
        if self
            .routine_state
            .selected_routine_window
            .as_ref()
            .is_some_and(|selected| !windows.contains(selected))
        {
            self.routine_state.selected_routine_window = None;
        }
        self.routine_state.routine_windows = windows;
        self.routine_state.invitations = retained;
        self.routine_state.invitations.extend(due);
        self.routine_state.invitations.sort_by(|a, b| {
            a.due_at_utc_ms
                .cmp(&b.due_at_utc_ms)
                .then(a.routine_id.cmp(&b.routine_id))
        });
        self.routine_state.routines = routines;
        self.routine_state.routine_next = next;
        Ok(())
    }

    pub(crate) fn save_routine(
        &mut self,
        id: Option<String>,
        input: RoutineInput,
    ) -> Result<(), ServiceError> {
        let now = self.clock.sample().utc_ms;
        // Resolve all fallible scheduling reads before committing the new definition.
        let zone = self.system_timezone(input.enabled && input.timezone == "system")?;
        let candidate = Routine {
            id: id.clone().unwrap_or_else(|| "preview".into()),
            input: input.clone(),
        };
        let next_utc_ms = self
            .application
            .repository()
            .next_routine_invitation(&candidate, now, &zone)?
            .map(|item| item.due_at_utc_ms);
        let saved = self.application.repository_mut().save_routine(id, input)?;
        self.routine_state
            .routines
            .retain(|item| item.id != saved.id);
        self.routine_state
            .routine_next
            .retain(|item| item.id != saved.id);
        self.routine_state
            .invitations
            .retain(|item| item.routine_id != saved.id);
        self.clear_routine_window(&saved.id);
        self.routine_state.routine_next.push(RoutineNext {
            id: saved.id.clone(),
            next_utc_ms,
        });
        self.routine_state.routines.push(saved);
        self.routine_state.routines.sort_by(|a, b| a.id.cmp(&b.id));
        self.routine_state
            .routine_next
            .sort_by(|a, b| a.id.cmp(&b.id));
        self.routine_state.routine_error = None;
        self.bump_routines();
        // Refresh scheduling after the successful save without turning a separate
        // scheduling error into a false "save failed" response.
        self.poll_routines();
        Ok(())
    }

    pub(crate) fn clear_routine_window(&mut self, routine_id: &str) {
        self.routine_state
            .routine_windows
            .retain(|item| item.routine_id != routine_id);
        if self
            .routine_state
            .selected_routine_window
            .as_ref()
            .is_some_and(|item| item.routine_id == routine_id)
        {
            self.routine_state.selected_routine_window = None;
        }
    }
}
