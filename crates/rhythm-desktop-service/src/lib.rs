//! Narrow desktop-facing operations over the durable Rust core.
//!
//! A single background worker owns this service. The webview receives serializable views,
//! never a connection, path, or SQL interface. Timer events are returned only after commit.
//! Views include the newest 100 sessions and reflections; drafts are never silently hidden.

mod routines;
pub use rhythm_storage::{
    DesktopProfile, NoteHistoryCursor, NoteHistoryPage, NoteHistoryQuery, NoteHistoryQueryCursor,
    NoteHistoryQueryPage, PendingDesktopProfile, SyncArchive, SyncImportReport, SyncObject,
};
pub use routines::{RoutineNext, RoutineView};

use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use rhythm_application::{ApplicationError, Rhythm};
use rhythm_domain::{
    Checkpoint, Clock, ClockSample, Command, RecoveryChoice, RoutineInput, SessionRecord, Settings,
    TimerState,
};
use rhythm_storage::{Draft, PeriodNote, PeriodNoteState, Reflection, ReflectionInput, Store};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

pub const HISTORY_PAGE_SIZE: u32 = 100;

#[derive(Debug, Error)]
pub enum ServiceError {
    #[error(transparent)]
    Application(#[from] ApplicationError<rhythm_storage::Error>),
    #[error(transparent)]
    Storage(#[from] rhythm_storage::Error),
    #[error("End the current period before restoring a backup.")]
    RestoreWhileActive,
    #[error("This invitation has expired or is no longer available.")]
    InvitationUnavailable,
    #[error("Finish the current period before accepting an invitation.")]
    InvitationWhileActive,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RestoreResult {
    pub safety_backup: String,
    #[serde(default)]
    pub pending_profile: Option<PendingDesktopProfile>,
}

/// The complete allowlist of operations exposed to the desktop frontend.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "op",
    content = "data",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum DesktopRequest {
    ImportPeriodNotes {
        notes: Vec<PeriodNote>,
    },
    ExtendFocus {
        additional_ms: u64,
    },
    SwitchToFocus {
        duration_ms: Option<u64>,
    },
    SwitchToBreak {
        duration_ms: Option<u64>,
    },
    StartFocus {
        duration_ms: Option<u64>,
    },
    StartRest {
        duration_ms: Option<u64>,
    },
    Pause,
    Resume,
    EndEarly,
    NewRhythm,
    Recover {
        choice: RecoveryChoice,
    },
    UpdateSettings {
        settings: Settings,
    },
    SavePeriodNote {
        id: String,
        version: u64,
        body: String,
    },
    CreateDraft {
        content: ReflectionInput,
    },
    SaveDraft {
        id: String,
        version: u64,
        body: String,
    },
    DiscardDraft {
        id: String,
        version: u64,
    },
    FinalizeDraft {
        id: String,
        version: u64,
    },
    EditReflection {
        id: String,
    },
    SaveRoutine {
        id: Option<String>,
        routine: RoutineInput,
    },
    DeleteRoutine {
        id: String,
    },
    DismissInvitation {
        routine_id: String,
        due_at_utc_ms: i64,
    },
    AcceptInvitation {
        routine_id: String,
        due_at_utc_ms: i64,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DesktopView {
    pub checkpoint: Checkpoint,
    pub revision: u64,
    pub sessions: Vec<SessionRecord>,
    pub drafts: Vec<Draft>,
    pub reflections: Vec<Reflection>,
    #[serde(flatten)]
    pub note_state: PeriodNoteState,
    #[serde(flatten)]
    pub routine_state: RoutineView,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DesktopResponse {
    pub view: DesktopView,
    /// The newly created or saved draft, including the next concurrency token.
    pub draft: Option<Draft>,
    pub period_note: Option<PeriodNote>,
    /// A newly committed terminal record. Never replayed by a subsequent snapshot or poll.
    pub terminal: Option<SessionRecord>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimerUpdate {
    pub checkpoint: Checkpoint,
    pub revision: u64,
    pub terminal: Option<SessionRecord>,
    pub notes_revision: u64,
    pub note_period_id: Option<String>,
}

// Sharing the adapter lets journal operations use the same injected UTC source without
// requiring a clock to implement Clone or creating a second process/boot epoch.
struct ServiceClock<C>(Arc<C>);

impl<C: Clock> Clock for ServiceClock<C> {
    fn sample(&self) -> ClockSample {
        self.0.sample()
    }
}

pub struct DesktopService<C: Clock> {
    application: Rhythm<Store, ServiceClock<C>>,
    clock: Arc<C>,
    database_path: PathBuf,
    routine_state: RoutineView,
    system_zone_override: Option<String>,
}

impl<C: Clock> DesktopService<C> {
    pub fn sync_device_id(&self) -> &str {
        self.application.repository().device_id()
    }

    /// Export only authored history; active timers, drafts, and preferences stay local.
    pub fn sync_objects(&self) -> Result<(String, Vec<SyncObject>), ServiceError> {
        let store = self.application.repository();
        Ok((store.device_id().to_string(), store.export_sync_objects()?))
    }

    pub fn import_sync_archive(
        &mut self,
        archive: &SyncArchive,
    ) -> Result<SyncImportReport, ServiceError> {
        Ok(self
            .application
            .repository_mut()
            .import_sync_archive(archive)?)
    }

    /// Open the local database and reconcile the stored timer against the injected clock.
    /// A changed process epoch enters recovery; opening never starts a new period.
    pub fn open(path: impl AsRef<Path>, clock: C) -> Result<Self, ServiceError> {
        let database_path = path.as_ref().to_path_buf();
        let clock = Arc::new(clock);
        let application = Rhythm::open(
            Store::open(path)?,
            ServiceClock(Arc::clone(&clock)),
            Settings::default(),
        )?;
        let mut service = Self {
            application,
            clock,
            database_path,
            routine_state: RoutineView::default(),
            system_zone_override: None,
        };
        service.poll_routines();
        Ok(service)
    }

    /// The shell flushes its editor before invoking these file operations. Native file
    /// pickers supply paths; they are deliberately absent from DesktopRequest.
    pub fn backup_to(&self, path: impl AsRef<Path>) -> Result<(), ServiceError> {
        self.application.repository().backup_to(path)?;
        Ok(())
    }

    pub fn backup_complete_to(
        &self,
        path: impl AsRef<Path>,
        profile: &DesktopProfile,
    ) -> Result<(), ServiceError> {
        self.application
            .repository()
            .backup_complete_to(path, profile)?;
        Ok(())
    }

    pub fn inspect_backup_profile(
        &self,
        path: impl AsRef<Path>,
    ) -> Result<Option<DesktopProfile>, ServiceError> {
        Ok(Store::inspect_backup_profile(path)?)
    }

    pub fn pending_desktop_profile(&self) -> Result<Option<PendingDesktopProfile>, ServiceError> {
        Ok(self.application.repository().pending_desktop_profile()?)
    }

    pub fn acknowledge_desktop_profile_restore(
        &mut self,
        token: &str,
    ) -> Result<bool, ServiceError> {
        Ok(self
            .application
            .repository_mut()
            .acknowledge_desktop_profile_restore(token)?)
    }

    /// The native shell supplies a previously previewed path and the frontend's
    /// validated current preferences. Both the safety snapshot and replacement
    /// include the full journal; pending preferences commit with the restored DB.
    pub fn restore_complete_from(
        &mut self,
        path: impl AsRef<Path>,
        current_profile: &DesktopProfile,
        expected_profile: &Option<DesktopProfile>,
    ) -> Result<RestoreResult, ServiceError> {
        if !matches!(self.application.state(), TimerState::Ready { .. }) {
            return Err(ServiceError::RestoreWhileActive);
        }
        current_profile.validate()?;
        let safety_path = self
            .database_path
            .with_file_name(format!("rhythm-before-restore-{}.sqlite3", Uuid::new_v4()));
        self.backup_complete_to(&safety_path, current_profile)?;
        let sample = self.clock.sample();
        let mut pending_profile = None;
        self.application.replace_from_backup(|store| {
            let (engine, revision, pending) =
                store.restore_complete_from(path, expected_profile, sample)?;
            pending_profile = pending;
            Ok((engine, revision))
        })?;
        let generation = self.routine_state.routines_revision.saturating_add(1);
        self.routine_state = RoutineView {
            routines_revision: generation,
            ..Default::default()
        };
        self.suppress_missed_desktop_invitations();
        // No fallible work after the replacement commits.
        Ok(RestoreResult {
            safety_backup: safety_path.to_string_lossy().into_owned(),
            pending_profile,
        })
    }

    pub fn export_markdown_to(&self, path: impl AsRef<Path>) -> Result<(), ServiceError> {
        self.application.repository().export_markdown_to(path)?;
        Ok(())
    }

    pub fn restore_from(&mut self, path: impl AsRef<Path>) -> Result<RestoreResult, ServiceError> {
        if !matches!(self.application.state(), TimerState::Ready { .. }) {
            return Err(ServiceError::RestoreWhileActive);
        }
        let safety_path = self
            .database_path
            .with_file_name(format!("rhythm-before-restore-{}.sqlite3", Uuid::new_v4()));
        self.backup_to(&safety_path)?;
        self.application
            .replace_from_backup(|store| store.restore_from(path))?;
        let generation = self.routine_state.routines_revision.saturating_add(1);
        self.routine_state = RoutineView {
            routines_revision: generation,
            ..Default::default()
        };
        self.poll_routines();
        // No fallible work after commit: a subsequent snapshot is a separate request.
        Ok(RestoreResult {
            safety_backup: safety_path.to_string_lossy().into_owned(),
            pending_profile: None,
        })
    }

    /// Read the committed view without advancing time or changing an editor's draft.
    pub fn snapshot(&self) -> Result<DesktopView, ServiceError> {
        let store = self.application.repository();
        Ok(DesktopView {
            checkpoint: self.application.checkpoint(),
            revision: self.application.revision(),
            sessions: store.list_sessions(HISTORY_PAGE_SIZE, 0)?,
            drafts: store.list_drafts()?,
            reflections: store.list_reflections(HISTORY_PAGE_SIZE, 0)?,
            note_state: store.period_notes()?,
            routine_state: self.routine_state.clone(),
        })
    }

    /// Narrow machine reads avoid loading journal text for timer/settings queries.
    pub fn current_timer(&self) -> TimerUpdate {
        self.timer_update(None)
    }
    pub fn routine_view(&self) -> RoutineView {
        self.routine_state.clone()
    }
    pub fn journal(&self) -> Result<Vec<Reflection>, ServiceError> {
        Ok(self
            .application
            .repository()
            .list_reflections(HISTORY_PAGE_SIZE, 0)?)
    }
    pub fn drafts(&self) -> Result<Vec<Draft>, ServiceError> {
        Ok(self.application.repository().list_drafts()?)
    }

    /// Read older writing only on demand; the timer's ordinary view stays small.
    pub fn note_history_page(
        &self,
        cursor: Option<&NoteHistoryCursor>,
        limit: u32,
        periods_only: bool,
    ) -> Result<NoteHistoryPage, ServiceError> {
        Ok(self
            .application
            .repository()
            .note_history_page(cursor, limit, periods_only)?)
    }

    pub fn query_note_history(
        &self,
        query: &NoteHistoryQuery,
        cursor: Option<&NoteHistoryQueryCursor>,
        limit: u32,
    ) -> Result<NoteHistoryQueryPage, ServiceError> {
        Ok(self
            .application
            .repository()
            .query_note_history(query, cursor, limit)?)
    }

    /// Reconcile a native heartbeat or lifecycle signal. Repeated late polls cannot create
    /// duplicate completion events, and neither a heartbeat nor a resume starts a new period.
    pub fn poll(&mut self) -> Result<TimerUpdate, ServiceError> {
        let terminal = self.application.dispatch(Command::Poll)?;
        Ok(self.timer_update(terminal))
    }

    /// Desktop sleep/shutdown/reopening ends any period at its last durable
    /// boundary. Notes archive in the same transaction; late editor saves still
    /// address that original note. The waiting Break has no deadline.
    pub fn interrupt_for_desktop(&mut self) -> Result<TimerUpdate, ServiceError> {
        let terminal = self.application.dispatch(Command::Interrupt)?;
        self.suppress_missed_desktop_invitations();
        Ok(self.timer_update(terminal))
    }

    /// Consume invitations missed while the desktop was unavailable without
    /// changing a period the person may already have deliberately started.
    pub fn suppress_missed_desktop_invitations(&mut self) {
        // Claim missed routine deliveries silently rather than replaying them
        // on waking. Future windows continue to work normally.
        self.poll_routines();
        if !self.routine_state.invitations.is_empty()
            || self.routine_state.selected_routine_window.is_some()
        {
            self.routine_state.invitations.clear();
            self.routine_state.selected_routine_window = None;
            self.bump_routines();
        }
    }

    /// Finish an explicit quit without reading unrelated journal content. Repeated calls
    /// are harmless: an already-ready timer cannot create another terminal record.
    /// The shell must stop its worker only after this operation succeeds.
    pub fn finish_for_quit(&mut self) -> Result<TimerUpdate, ServiceError> {
        let command = match self.application.state() {
            TimerState::Running(_) | TimerState::Paused(_) => Command::EndEarly,
            TimerState::RecoveryRequired(_) => Command::Recover(RecoveryChoice::End),
            TimerState::Ready { .. } => Command::Poll,
        };
        let terminal = self.application.dispatch(command)?;
        Ok(self.timer_update(terminal))
    }

    fn timer_update(&self, terminal: Option<SessionRecord>) -> TimerUpdate {
        let notes = self.application.repository().committed_period_notes();
        TimerUpdate {
            checkpoint: self.application.checkpoint(),
            revision: self.application.revision(),
            terminal,
            notes_revision: notes.notes_revision,
            note_period_id: notes.period_note.as_ref().map(|note| note.id.clone()),
        }
    }

    /// Execute one explicit UI intent. Session identifiers are generated in trusted Rust.
    /// Stale draft writes fail without replacing the durable text or its concurrency token.
    pub fn execute(&mut self, request: DesktopRequest) -> Result<DesktopResponse, ServiceError> {
        if matches!(
            request,
            DesktopRequest::AcceptInvitation { .. }
                | DesktopRequest::DismissInvitation { .. }
                | DesktopRequest::StartFocus { .. }
                | DesktopRequest::StartRest { .. }
                | DesktopRequest::SwitchToFocus { .. }
                | DesktopRequest::SwitchToBreak { .. }
        ) {
            self.poll_routines();
        }
        // All fallible rendering reads happen before any mutation. After a successful
        // commit, construct the response from its returned values rather than risk
        // reporting "save failed" because an unrelated post-commit SELECT failed.
        let mut view = self.snapshot()?;
        let mut draft = None;
        let mut period_note = None;
        let mut terminal = None;
        let mut accepted_invitation = None;
        let timer_command = match request {
            DesktopRequest::ImportPeriodNotes { notes } => {
                self.application
                    .repository_mut()
                    .import_period_notes(&notes)?;
                view.note_state = self
                    .application
                    .repository()
                    .committed_period_notes()
                    .clone();
                None
            }
            DesktopRequest::ExtendFocus { additional_ms } => {
                Some(Command::ExtendFocus { additional_ms })
            }
            DesktopRequest::SwitchToFocus { duration_ms } => Some(Command::SwitchToFocus {
                session_id: Uuid::new_v4().to_string(),
                duration_ms: duration_ms.or_else(|| {
                    self.routine_state
                        .selected_routine_window
                        .as_ref()
                        .map(|window| window.focus_ms)
                }),
            }),
            DesktopRequest::SwitchToBreak { duration_ms } => Some(Command::SwitchToBreak {
                session_id: Uuid::new_v4().to_string(),
                duration_ms,
            }),
            DesktopRequest::StartFocus { duration_ms } => Some(Command::StartFocus {
                session_id: Uuid::new_v4().to_string(),
                duration_ms: duration_ms.or_else(|| {
                    self.routine_state
                        .selected_routine_window
                        .as_ref()
                        .map(|window| window.focus_ms)
                }),
            }),
            DesktopRequest::StartRest { duration_ms } => Some(Command::StartRest {
                session_id: Uuid::new_v4().to_string(),
                duration_ms,
            }),
            DesktopRequest::Pause => Some(Command::Pause),
            DesktopRequest::Resume => Some(Command::Resume),
            DesktopRequest::EndEarly => Some(Command::EndEarly),
            DesktopRequest::NewRhythm => Some(Command::NewRhythm),
            DesktopRequest::Recover { choice } => Some(Command::Recover(choice)),
            DesktopRequest::UpdateSettings { settings } => Some(Command::UpdateSettings(settings)),
            DesktopRequest::SavePeriodNote { id, version, body } => {
                let now = self.clock.sample().utc_ms;
                period_note = Some(
                    self.application
                        .repository_mut()
                        .save_period_note(&id, version, &body, now)?,
                );
                view.note_state = self
                    .application
                    .repository()
                    .committed_period_notes()
                    .clone();
                None
            }
            DesktopRequest::CreateDraft { content } => {
                let now = self.clock.sample().utc_ms;
                draft = Some(
                    self.application
                        .repository_mut()
                        .create_draft(content, now)?,
                );
                None
            }
            DesktopRequest::SaveDraft { id, version, body } => {
                let now = self.clock.sample().utc_ms;
                draft = Some(
                    self.application
                        .repository_mut()
                        .save_draft(&id, version, &body, now)?,
                );
                None
            }
            DesktopRequest::DiscardDraft { id, version } => {
                self.application
                    .repository_mut()
                    .discard_draft(&id, version)?;
                view.drafts.retain(|draft| draft.id != id);
                None
            }
            DesktopRequest::FinalizeDraft { id, version } => {
                let pending = view
                    .drafts
                    .iter()
                    .find(|draft| draft.id == id)
                    .ok_or_else(|| rhythm_storage::Error::DraftNotFound(id.clone()))?;
                // An older record can lie outside the bounded history page. Fetch its
                // creation date before committing so edits retain storage's sort order.
                let existing = match &pending.record_id {
                    Some(record_id) => Some(
                        self.application
                            .repository()
                            .get_reflection(record_id)?
                            .ok_or_else(|| {
                                rhythm_storage::Error::ReflectionNotFound(record_id.clone())
                            })?,
                    ),
                    None => None,
                };
                let now = self.clock.sample().utc_ms;
                let revision = self
                    .application
                    .repository_mut()
                    .finalize_draft(&id, version, now)?;
                let created_at_utc_ms = existing.map_or(revision.created_at_utc_ms, |record| {
                    record.created_at_utc_ms
                });
                view.drafts.retain(|draft| draft.id != id);
                view.reflections
                    .retain(|record| record.id != revision.record_id);
                view.reflections.push(Reflection {
                    id: revision.record_id.clone(),
                    heads: vec![revision],
                    created_at_utc_ms,
                });
                view.reflections.sort_by(|left, right| {
                    right
                        .created_at_utc_ms
                        .cmp(&left.created_at_utc_ms)
                        .then_with(|| left.id.cmp(&right.id))
                });
                view.reflections.truncate(HISTORY_PAGE_SIZE as usize);
                None
            }
            DesktopRequest::EditReflection { id } => {
                let now = self.clock.sample().utc_ms;
                draft = Some(
                    self.application
                        .repository_mut()
                        .edit_reflection(&id, now)?,
                );
                None
            }
            DesktopRequest::SaveRoutine { id, routine } => {
                self.save_routine(id, routine)?;
                None
            }
            DesktopRequest::DeleteRoutine { id } => {
                self.application.repository_mut().delete_routine(&id)?;
                self.routine_state.routines.retain(|item| item.id != id);
                self.routine_state.routine_next.retain(|item| item.id != id);
                self.routine_state
                    .invitations
                    .retain(|item| item.routine_id != id);
                self.clear_routine_window(&id);
                self.bump_routines();
                None
            }
            DesktopRequest::DismissInvitation {
                routine_id,
                due_at_utc_ms,
            } => {
                if let Some(window) = self
                    .routine_state
                    .routine_windows
                    .iter()
                    .find(|item| {
                        item.routine_id == routine_id && item.due_at_utc_ms == due_at_utc_ms
                    })
                    .cloned()
                {
                    self.application
                        .repository_mut()
                        .dismiss_routine_window(&window)?;
                    self.routine_state.routine_windows.retain(|item| {
                        item.routine_id != routine_id || item.due_at_utc_ms != due_at_utc_ms
                    });
                    if self
                        .routine_state
                        .selected_routine_window
                        .as_ref()
                        .is_some_and(|item| {
                            item.routine_id == routine_id && item.due_at_utc_ms == due_at_utc_ms
                        })
                    {
                        self.routine_state.selected_routine_window = None;
                    }
                }
                self.routine_state.invitations.retain(|item| {
                    item.routine_id != routine_id || item.due_at_utc_ms != due_at_utc_ms
                });
                self.bump_routines();
                None
            }
            DesktopRequest::AcceptInvitation {
                routine_id,
                due_at_utc_ms,
            } => {
                if !matches!(self.application.state(), TimerState::Ready { .. }) {
                    return Err(ServiceError::InvitationWhileActive);
                }
                let now = self.clock.sample().utc_ms;
                let invitation = self
                    .routine_state
                    .invitations
                    .iter()
                    .chain(self.routine_state.routine_windows.iter())
                    .find(|item| {
                        item.routine_id == routine_id
                            && item.due_at_utc_ms == due_at_utc_ms
                            && now >= item.due_at_utc_ms
                            && now < item.expires_at_utc_ms
                    })
                    .ok_or(ServiceError::InvitationUnavailable)?
                    .clone();
                accepted_invitation = Some(invitation.clone());
                Some(Command::StartFocus {
                    session_id: Uuid::new_v4().to_string(),
                    duration_ms: Some(invitation.focus_ms),
                })
            }
        };
        if let Some(command) = timer_command {
            let starts_period = matches!(
                command,
                Command::StartFocus { .. }
                    | Command::StartRest { .. }
                    | Command::SwitchToFocus { .. }
                    | Command::SwitchToBreak { .. }
            );
            terminal = self.application.dispatch(command)?;
            view.note_state = self
                .application
                .repository()
                .committed_period_notes()
                .clone();
            if let Some(invitation) = accepted_invitation {
                let selected = invitation.window.is_some().then_some(invitation);
                if self.routine_state.selected_routine_window != selected {
                    self.routine_state.selected_routine_window = selected;
                    self.bump_routines();
                }
            }
            if starts_period && !self.routine_state.invitations.is_empty() {
                self.routine_state.invitations.clear();
                self.bump_routines();
            }
        }
        if let Some(saved) = &draft {
            view.drafts.retain(|draft| draft.id != saved.id);
            view.drafts.push(saved.clone());
            view.drafts.sort_by(|left, right| {
                right
                    .updated_at_utc_ms
                    .cmp(&left.updated_at_utc_ms)
                    .then_with(|| left.id.cmp(&right.id))
            });
        }
        if let Some(session) = &terminal {
            view.sessions.push(session.clone());
            view.sessions.sort_by(|left, right| {
                right
                    .started_utc_ms
                    .cmp(&left.started_utc_ms)
                    .then_with(|| left.id.cmp(&right.id))
            });
            view.sessions.truncate(HISTORY_PAGE_SIZE as usize);
        }
        view.checkpoint = self.application.checkpoint();
        view.revision = self.application.revision();
        view.routine_state = self.routine_state.clone();
        Ok(DesktopResponse {
            view,
            draft,
            period_note,
            terminal,
        })
    }
}
