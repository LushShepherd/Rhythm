//! Thin desktop adapter. The worker alone owns SQLite and advances the Rust timer.
use crate::automatic_sync::{self, Cancellation};
use crate::break_reminder::{BreakAction, BreakReminder, ReminderDescriptor, ReminderKind};
use crate::cli_quit::{PrepareError, QuitCoordinator};
use crate::files::{self, Operation};
use crate::notification::{self, NotificationDiagnostics, NotificationReceipt};
use rhythm_control::{ControlRequest, ControlResponse};
use rhythm_desktop_service::{
    DesktopProfile, DesktopRequest, DesktopResponse, DesktopService, DesktopView,
    NoteHistoryCursor, NoteHistoryPage, NoteHistoryQuery, NoteHistoryQueryCursor,
    NoteHistoryQueryPage, PendingDesktopProfile, RestoreResult, RoutineView, SyncArchive,
    SyncImportReport, SyncObject, TimerUpdate,
};
use rhythm_domain::{Clock, PeriodKind, SessionRecord, TimerState};
use rhythm_platform_windows::{ClockCapability, PowerSubscription, WindowsClock};
use serde::Serialize;
use std::{
    collections::HashSet,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};
use tauri::{AppHandle, Emitter, Manager, State, WebviewWindow};

type Reply<T> = mpsc::Sender<Result<T, String>>;

enum Work {
    Control(ControlRequest, u64, Reply<ControlResponse>),
    Snapshot(Reply<DesktopView>),
    NoteHistory(Option<NoteHistoryCursor>, u32, bool, Reply<NoteHistoryPage>),
    QueryNoteHistory(
        NoteHistoryQuery,
        Option<NoteHistoryQueryCursor>,
        u32,
        Reply<NoteHistoryQueryPage>,
    ),
    SyncSnapshot(Reply<(String, Vec<SyncObject>)>),
    SyncImport(String, SyncArchive, Cancellation, Reply<SyncImportReport>),
    Execute(DesktopRequest, u64, Reply<DesktopResponse>),
    CurrentReminder(Reply<Option<ReminderDescriptor>>),
    RespondBreak(ReminderChoice, Reply<Option<ReminderResponse>>),
    Backup(PathBuf, DesktopProfile, Reply<()>),
    Export(PathBuf, Reply<()>),
    InspectBackup(PathBuf, Reply<Option<DesktopProfile>>),
    Restore(
        PathBuf,
        DesktopProfile,
        Box<Option<DesktopProfile>>,
        Reply<RestoreResult>,
    ),
    PendingProfile(Reply<Option<PendingDesktopProfile>>),
    AcknowledgeProfile(String, Reply<bool>),
    Quit(Reply<()>),
    Interrupt,
    LifecycleSaved(mpsc::Sender<()>),
    Wake,
}

#[derive(Clone, Serialize)]
struct ReminderResponse {
    #[serde(flatten)]
    response: DesktopResponse,
    reminder: Option<ReminderDescriptor>,
}

struct ReminderChoice {
    period_id: String,
    sequence: u32,
    action: BreakAction,
    duration_ms: Option<u64>,
}

#[derive(Clone, Serialize)]
struct PlatformStatus {
    sleep_inclusive: bool,
    power_events: bool,
    notification_note: String,
}

struct Desktop {
    sender: mpsc::Sender<Work>,
    status: PlatformStatus,
    language: Mutex<String>,
    ui_ready: AtomicBool,
    quit: Arc<QuitCoordinator>,
    shutdown_pending: Arc<AtomicBool>,
    lifecycle_save: Mutex<Option<(String, mpsc::Sender<()>)>>,
    selected_restore: Mutex<Option<SelectedRestore>>,
    sync: automatic_sync::Manager,
    _control: rhythm_control::ServerGuard,
}

struct SyncJournal(mpsc::Sender<Work>);

impl automatic_sync::Journal for SyncJournal {
    fn snapshot(&mut self) -> Result<crate::sync_engine::LocalSnapshot, String> {
        let (reply, receive) = mpsc::channel();
        self.0
            .send(Work::SyncSnapshot(reply))
            .map_err(|_| "The journal worker stopped.")?;
        let (device_id, objects) = receive
            .recv()
            .map_err(|_| "The journal worker stopped.")??;
        Ok(crate::sync_engine::LocalSnapshot { device_id, objects })
    }

    fn import(
        &mut self,
        device_id: &str,
        archive: &SyncArchive,
        cancellation: &Cancellation,
    ) -> Result<SyncImportReport, String> {
        let (reply, receive) = mpsc::channel();
        self.0
            .send(Work::SyncImport(
                device_id.into(),
                archive.clone(),
                cancellation.clone(),
                reply,
            ))
            .map_err(|_| "The journal worker stopped.")?;
        receive.recv().map_err(|_| "The journal worker stopped.")?
    }
}

#[tauri::command]
fn sync_status(state: State<'_, Desktop>) -> automatic_sync::Status {
    state.sync.status()
}

#[tauri::command]
async fn connect_sync(
    state: State<'_, Desktop>,
    passphrase: String,
) -> Result<automatic_sync::Status, String> {
    let sync = state.sync.clone();
    tauri::async_runtime::spawn_blocking(move || sync.connect(passphrase))
        .await
        .map_err(|_| "Google Drive connection stopped.")?
}

#[tauri::command]
async fn retry_sync(state: State<'_, Desktop>) -> Result<automatic_sync::Status, String> {
    let sync = state.sync.clone();
    tauri::async_runtime::spawn_blocking(move || sync.retry())
        .await
        .map_err(|_| "Sync retry stopped.")?
}

#[tauri::command]
async fn disconnect_sync(state: State<'_, Desktop>) -> Result<automatic_sync::Status, String> {
    let sync = state.sync.clone();
    tauri::async_runtime::spawn_blocking(move || sync.disconnect())
        .await
        .map_err(|_| "Disconnect did not finish.")?
}

struct SelectedRestore {
    token: String,
    path: PathBuf,
    profile: Option<DesktopProfile>,
}

#[derive(Serialize)]
struct RestorePreview {
    token: String,
    file_name: String,
    profile: Option<DesktopProfile>,
}

fn cancel_restore_selection(selected: &mut Option<SelectedRestore>, token: &str) -> bool {
    if selected
        .as_ref()
        .is_some_and(|selected| selected.token == token)
    {
        selected.take();
        true
    } else {
        false
    }
}

// Commands wait on a blocking pool, never the native window/event-loop thread.
async fn request<T: Send + 'static>(
    sender: mpsc::Sender<Work>,
    make_work: impl FnOnce(Reply<T>) -> Work + Send + 'static,
) -> Result<T, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let (reply, receiver) = mpsc::channel();
        sender
            .send(make_work(reply))
            .map_err(|_| "The timer worker has stopped. Reopen Rhythm.".to_owned())?;
        // Do not time out a write: a late successful commit would look like failure
        // and an immediate user retry could duplicate their action.
        receiver.recv().map_err(|_| {
            "The timer worker stopped before confirming the save. Reopen Rhythm.".to_owned()
        })?
    })
    .await
    .map_err(|_| "The desktop request could not finish.".to_owned())?
}

#[tauri::command]
async fn snapshot(state: State<'_, Desktop>) -> Result<DesktopView, String> {
    request(state.sender.clone(), Work::Snapshot).await
}

#[tauri::command]
async fn note_history(
    state: State<'_, Desktop>,
    cursor: Option<NoteHistoryCursor>,
    limit: u32,
    periods_only: bool,
) -> Result<NoteHistoryPage, String> {
    request(state.sender.clone(), move |reply| {
        Work::NoteHistory(cursor, limit, periods_only, reply)
    })
    .await
}

#[tauri::command]
async fn note_history_query(
    state: State<'_, Desktop>,
    query: NoteHistoryQuery,
    cursor: Option<NoteHistoryQueryCursor>,
    limit: u32,
) -> Result<NoteHistoryQueryPage, String> {
    request(state.sender.clone(), move |reply| {
        Work::QueryNoteHistory(query, cursor, limit, reply)
    })
    .await
}

#[tauri::command]
async fn execute(
    state: State<'_, Desktop>,
    request: DesktopRequest,
) -> Result<DesktopResponse, String> {
    let generation = WindowsClock::new().interruption_generation();
    self::request(state.sender.clone(), move |reply| {
        Work::Execute(request, generation, reply)
    })
    .await
}

#[tauri::command]
fn lifecycle_saved(state: State<'_, Desktop>, token: String, error: Option<String>) {
    let mut pending = state
        .lifecycle_save
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    if pending
        .as_ref()
        .is_none_or(|(current, _)| current != &token)
    {
        return;
    }
    if error.is_none()
        && let Some((_, reply)) = pending.take()
    {
        // The same worker has already committed the awaited editor writes.
        let _ = state.sender.send(Work::LifecycleSaved(reply));
    }
}

fn prepare_shutdown(app: &AppHandle) -> mpsc::Receiver<()> {
    let (ack, receiver) = mpsc::channel();
    if let Some(state) = app.try_state::<Desktop>() {
        state.shutdown_pending.store(true, Ordering::Release);
        let token = uuid::Uuid::new_v4().to_string();
        let _ = state.sender.send(Work::Interrupt);
        if state.ui_ready.load(Ordering::Acquire) {
            *state
                .lifecycle_save
                .lock()
                .unwrap_or_else(|e| e.into_inner()) = Some((token.clone(), ack));
            let _ = app.emit(
                "rhythm:lifecycle_prepare",
                serde_json::json!({"token":token}),
            );
        } else {
            let _ = state.sender.send(Work::LifecycleSaved(ack));
        }
    }
    receiver
}

fn cancel_shutdown(app: &AppHandle) {
    if let Some(state) = app.try_state::<Desktop>() {
        state.shutdown_pending.store(false, Ordering::Release);
        state
            .lifecycle_save
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take();
        // Re-publish the committed Break state so an unfinished UI preparation
        // cannot prevent a later deliberate Focus after shutdown was cancelled.
        let _ = state.sender.send(Work::Interrupt);
    }
}

/// The UI flushes its note before calling this. The worker checks the completed
/// period again at the write boundary, so stale/double-clicked toasts are quiet.
#[tauri::command]
async fn respond_break_reminder(
    app: AppHandle,
    state: State<'_, Desktop>,
    period_id: String,
    sequence: u32,
    action: BreakAction,
    duration_ms: Option<u64>,
    reveal: Option<bool>,
) -> Result<Option<ReminderResponse>, String> {
    let result = self::request(state.sender.clone(), move |reply| {
        Work::RespondBreak(
            ReminderChoice {
                period_id,
                sequence,
                action,
                duration_ms,
            },
            reply,
        )
    })
    .await?;
    if result.is_some() && action == BreakAction::Accept && reveal.unwrap_or(false) {
        reveal_main(&app);
    }
    Ok(result)
}

#[tauri::command]
async fn current_reminder(state: State<'_, Desktop>) -> Result<Option<ReminderDescriptor>, String> {
    request(state.sender.clone(), Work::CurrentReminder).await
}

fn reveal_main(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
    }
}

async fn choose_file(app: &AppHandle, operation: Operation) -> Result<Option<PathBuf>, String> {
    let window = app
        .get_webview_window("main")
        .ok_or("The main window is unavailable.")?;
    let owner = window
        .hwnd()
        .map_err(|_| "The main window is unavailable.")?
        .0 as usize;
    let language = language(app);
    tauri::async_runtime::spawn_blocking(move || files::choose(operation, owner, &language))
        .await
        .map_err(|_| "The file dialog could not finish.".to_owned())?
}

#[tauri::command]
async fn backup_data(
    app: AppHandle,
    state: State<'_, Desktop>,
    profile: DesktopProfile,
) -> Result<Option<String>, String> {
    let Some(path) = choose_file(&app, Operation::Backup).await? else {
        return Ok(None);
    };
    let display = path.display().to_string();
    request(state.sender.clone(), move |reply| {
        Work::Backup(path, profile, reply)
    })
    .await?;
    Ok(Some(display))
}

#[tauri::command]
async fn export_markdown(
    app: AppHandle,
    state: State<'_, Desktop>,
) -> Result<Option<String>, String> {
    let Some(path) = choose_file(&app, Operation::Export).await? else {
        return Ok(None);
    };
    let display = path.display().to_string();
    request(state.sender.clone(), move |reply| Work::Export(path, reply)).await?;
    Ok(Some(display))
}

/// The caller supplies only an encrypted envelope, never a destination path.
/// The native dialog owns destination selection and returns null on cancellation.
#[tauri::command]
async fn save_encrypted_notes(app: AppHandle, contents: String) -> Result<Option<String>, String> {
    crate::encrypted_file::validate(&contents)?;
    let Some(path) = choose_file(&app, Operation::EncryptedNotes).await? else {
        return Ok(None);
    };
    let display = path.display().to_string();
    tauri::async_runtime::spawn_blocking(move || crate::encrypted_file::save_new(&path, &contents))
        .await
        .map_err(|_| "The encrypted notes file could not be saved.".to_owned())??;
    Ok(Some(display))
}

#[tauri::command]
async fn choose_restore_data(
    app: AppHandle,
    state: State<'_, Desktop>,
) -> Result<Option<RestorePreview>, String> {
    state
        .selected_restore
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .take();
    let Some(path) = choose_file(&app, Operation::Restore).await? else {
        return Ok(None);
    };
    let inspect_path = path.clone();
    let profile = request(state.sender.clone(), move |reply| {
        Work::InspectBackup(inspect_path, reply)
    })
    .await?;
    let token = uuid::Uuid::new_v4().to_string();
    let file_name = path
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned();
    *state
        .selected_restore
        .lock()
        .unwrap_or_else(|e| e.into_inner()) = Some(SelectedRestore {
        token: token.clone(),
        path,
        profile: profile.clone(),
    });
    Ok(Some(RestorePreview {
        token,
        file_name,
        profile,
    }))
}

#[tauri::command]
fn cancel_restore_data(state: State<'_, Desktop>, token: String) -> bool {
    let mut selected = state
        .selected_restore
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    cancel_restore_selection(&mut selected, &token)
}

#[tauri::command]
async fn commit_restore_data(
    state: State<'_, Desktop>,
    token: String,
    current_profile: DesktopProfile,
) -> Result<RestoreResult, String> {
    let selected = {
        let mut pending = state
            .selected_restore
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if pending
            .as_ref()
            .is_none_or(|selected| selected.token != token)
        {
            return Err("Choose the backup again before restoring.".into());
        }
        pending.take().expect("checked selected restore")
    };
    // Stop/fence network imports before replacing the journal. Restoring never
    // re-enrolls cloud credentials or starts importing history without reconnect.
    let sync = state.sync.clone();
    tauri::async_runtime::spawn_blocking(move || sync.disconnect())
        .await
        .map_err(|_| "Sync could not stop before restoring the backup.")??;
    state.ui_ready.store(false, Ordering::Release);
    let result = request(state.sender.clone(), move |reply| {
        Work::Restore(
            selected.path,
            current_profile,
            Box::new(selected.profile),
            reply,
        )
    })
    .await;
    if result.is_err() {
        state.ui_ready.store(true, Ordering::Release);
    }
    result
}

#[tauri::command]
async fn pending_profile_restore(
    state: State<'_, Desktop>,
) -> Result<Option<PendingDesktopProfile>, String> {
    request(state.sender.clone(), Work::PendingProfile).await
}

#[tauri::command]
async fn acknowledge_profile_restore(
    state: State<'_, Desktop>,
    token: String,
) -> Result<bool, String> {
    request(state.sender.clone(), move |reply| {
        Work::AcknowledgeProfile(token, reply)
    })
    .await
}

#[tauri::command]
fn platform_status(state: State<'_, Desktop>) -> PlatformStatus {
    state.status.clone()
}

#[tauri::command]
fn ui_ready(state: State<'_, Desktop>) {
    state.ui_ready.store(true, Ordering::Release);
}

#[tauri::command]
fn set_fullscreen(window: WebviewWindow, enabled: bool) -> Result<(), String> {
    window
        .set_fullscreen(enabled)
        .map_err(|_| "The window could not change fullscreen mode.".to_owned())
}

#[tauri::command]
fn set_notification_language(state: State<'_, Desktop>, language: String) -> Result<(), String> {
    if !matches!(language.as_str(), "en" | "zh-Hans" | "zh-Hant") {
        return Err("Unsupported language.".to_owned());
    }
    *state
        .language
        .lock()
        .map_err(|_| "The language preference is unavailable.".to_owned())? = language;
    Ok(())
}

fn language(app: &AppHandle) -> String {
    app.try_state::<Desktop>()
        .and_then(|state| state.language.lock().ok().map(|language| language.clone()))
        .unwrap_or_else(|| "en".to_owned())
}

fn notification_copy(language: &str, kind: Option<PeriodKind>) -> (&'static str, &'static str) {
    match (language, kind) {
        ("zh-Hans", Some(PeriodKind::Focus)) => (
            "专注告一段落",
            "给自己一点休息。准备好后，回到 Rhythm 开始。",
        ),
        ("zh-Hant", Some(PeriodKind::Focus)) => (
            "專注告一段落",
            "給自己一點休息。準備好後，回到 Rhythm 開始。",
        ),
        ("zh-Hans", Some(_)) => ("休息结束了", "慢慢来。准备好后，再开始下一段专注。"),
        ("zh-Hant", Some(_)) => ("休息結束了", "慢慢來。準備好後，再開始下一段專注。"),
        ("zh-Hans", None) => ("Rhythm", "这是一条测试提醒。你的文字始终留在应用内。"),
        ("zh-Hant", None) => ("Rhythm", "這是一則測試提醒。你的文字始終留在應用程式內。"),
        (_, Some(PeriodKind::Focus)) => (
            "A little room to rest",
            "Your focus period is complete. Return to Rhythm when you are ready.",
        ),
        (_, Some(_)) => (
            "Ready when you are",
            "Your rest is complete. Begin another focus period when you choose.",
        ),
        (_, None) => (
            "Rhythm",
            "A gentle test reminder. Your journal words stay inside the app.",
        ),
    }
}

fn notify(app: &AppHandle, kind: Option<PeriodKind>) -> Result<NotificationReceipt, String> {
    let (title, body) = notification_copy(&language(app), kind);
    let app_handle = app.clone();
    notification::show(&app.config().identifier, title, body, move |error| {
        let _ = app_handle.emit("rhythm:error", error);
    })
}

fn break_action_labels(
    language: &str,
    kind: ReminderKind,
) -> notification::BreakActionLabels<'static> {
    let (accept, snooze) = match (language, kind) {
        ("zh-Hans", ReminderKind::Focus) => ("休息一下", "继续专注"),
        ("zh-Hant", ReminderKind::Focus) => ("休息一下", "繼續專注"),
        (_, ReminderKind::Focus) => ("Take a break", "Stay focused"),
        ("zh-Hans", ReminderKind::Break) => ("专注", "继续休息"),
        ("zh-Hant", ReminderKind::Break) => ("專注", "繼續休息"),
        (_, ReminderKind::Break) => ("Focus", "Keep resting"),
    };
    notification::BreakActionLabels { accept, snooze }
}

#[derive(Clone, Serialize)]
struct BreakActionEvent {
    period_id: String,
    kind: ReminderKind,
    sequence: u32,
    action: BreakAction,
}

fn notify_period_completed(
    app: &AppHandle,
    reminder: &ReminderDescriptor,
) -> Result<NotificationReceipt, String> {
    let language = language(app);
    let period_kind = if reminder.kind == ReminderKind::Focus {
        PeriodKind::Focus
    } else {
        PeriodKind::ShortRest
    };
    let (title, body) = notification_copy(&language, Some(period_kind));
    let action_app = app.clone();
    let failure_app = app.clone();
    let reminder = reminder.clone();
    notification::show_break(
        &app.config().identifier,
        title,
        body,
        break_action_labels(&language, reminder.kind),
        move |activation| {
            let action = match activation {
                notification::ToastActivation::Open => {
                    reveal_main(&action_app);
                    return;
                }
                notification::ToastActivation::Accept => BreakAction::Accept,
                notification::ToastActivation::Snooze => BreakAction::Snooze,
            };
            let _ = action_app.emit(
                "rhythm:break_action",
                BreakActionEvent {
                    period_id: reminder.period_id.clone(),
                    kind: reminder.kind,
                    sequence: reminder.sequence,
                    action,
                },
            );
        },
        move |message| {
            let _ = failure_app.emit("rhythm:error", message);
        },
    )
}

#[tauri::command]
async fn test_notification(app: AppHandle) -> Result<NotificationReceipt, String> {
    tauri::async_runtime::spawn_blocking(move || notify(&app, None))
        .await
        .map_err(|_| "The notification request could not finish.".to_owned())?
}

#[tauri::command]
async fn notification_status(app: AppHandle) -> Result<NotificationDiagnostics, String> {
    tauri::async_runtime::spawn_blocking(move || {
        notification::diagnostics(&app.config().identifier)
    })
    .await
    .map_err(|_| "Windows notification status is unavailable.".to_owned())?
}

#[tauri::command]
async fn quit(app: AppHandle, state: State<'_, Desktop>) -> Result<(), String> {
    state.quit.begin_ui()?;
    if let Err(error) = request(state.sender.clone(), Work::Quit).await {
        state.quit.ui_failed();
        return Err(error);
    }
    app.exit(0);
    Ok(())
}

#[tauri::command]
fn prepare_cli_quit_result(
    state: State<'_, Desktop>,
    token: String,
    error: Option<String>,
) -> Result<(), String> {
    state.quit.prepared(
        &token,
        error.map_or(Ok(()), |message| Err(message.chars().take(500).collect())),
    )
}

fn cancel_cli_quit(app: &AppHandle, token: &str, error: &str) {
    let _ = app.emit(
        "rhythm:cli_quit_cancelled",
        serde_json::json!({"token":token,"error":error}),
    );
}

/// Runs on the control thread, not the timer worker. The UI can therefore flush
/// its draft through Work::Execute while this function waits for readiness.
fn control_quit(
    app: &AppHandle,
    sender: &mpsc::Sender<Work>,
    quit: &QuitCoordinator,
    id: String,
) -> ControlResponse {
    let ready = app
        .try_state::<Desktop>()
        .is_some_and(|state| state.ui_ready.load(Ordering::Acquire));
    if !ready || app.get_webview_window("main").is_none() {
        return ControlResponse::failure(
            id,
            "ui_unavailable",
            "Rhythm's editor is not ready to save. Keep the app open and try again after it finishes opening.",
        );
    }
    let token = uuid::Uuid::new_v4().to_string();
    if let Err(error) = quit.begin_cli(token.clone()) {
        return ControlResponse::failure(id, "quit_refused", error);
    }
    if app
        .emit(
            "rhythm:cli_quit_requested",
            serde_json::json!({"token":token}),
        )
        .is_err()
    {
        quit.cancel_preparing(&token);
        let error = "Rhythm could not ask the editor to save. The app has been left open.";
        cancel_cli_quit(app, &token, error);
        return ControlResponse::failure(id, "ui_unavailable", error);
    }
    if let Err(error) = quit.wait_prepared(&token, Duration::from_secs(10)) {
        let (code, message) = match error {
            PrepareError::Refused(message) => ("quit_refused", message),
            PrepareError::Timeout => ("quit_timeout", "The editor did not confirm its save in time. Rhythm has been left open; check the editor before trying again.".into()),
        };
        cancel_cli_quit(app, &token, &message);
        return ControlResponse::failure(id, code, message);
    }
    let (reply, response) = mpsc::channel();
    let result = if sender.send(Work::Quit(reply)).is_err() {
        Err("The timer worker is unavailable. Rhythm could not confirm a safe quit.".into())
    } else {
        // A durable write must not be cancelled just because its reply is late.
        response.recv().unwrap_or_else(|_| {
            Err("The timer worker stopped before confirming the final save.".into())
        })
    };
    if let Err(error) = result {
        quit.finish_failed();
        cancel_cli_quit(app, &token, &error);
        return ControlResponse::failure(id, "quit_refused", error);
    }
    quit.committed(id.clone());
    ControlResponse::success(id, serde_json::json!({"closing":true}))
}

fn publish_terminal(
    app: &AppHandle,
    terminal: Option<&SessionRecord>,
    state: &TimerState,
    reminder: &mut BreakReminder,
) {
    let now = WindowsClock::new().sample();
    let changed = reminder.observe(state, terminal, &now) | reminder.advance(&now);
    if changed {
        publish_reminder(app, reminder, true);
    }
}

fn publish_reminder(app: &AppHandle, reminder: &BreakReminder, notify_when_due: bool) {
    if let Err(message) = reminder.persist() {
        let _ = app.emit("rhythm:error", message);
    }
    let descriptor = reminder.descriptor();
    let _ = app.emit("rhythm:reminder", &descriptor);
    if notify_when_due
        && !foreground_can_present(app)
        && let Some(current) = descriptor.filter(|item| item.available)
        && let Err(message) = notify_period_completed(app, &current)
    {
        let _ = app.emit("rhythm:error", message);
    }
}

/// A ready, visible foreground UI presents its own state transition. Sending an
/// OS banner as well would interrupt the same person twice. Background delivery
/// never shows, focuses, unminimizes, or changes fullscreen on the window.
fn foreground_can_present(app: &AppHandle) -> bool {
    let ready = app
        .try_state::<Desktop>()
        .is_some_and(|state| state.ui_ready.load(Ordering::Acquire));
    app.get_webview_window("main").is_some_and(|window| {
        presentation_is_foreground(
            ready,
            window.is_visible().unwrap_or(false),
            window.is_focused().unwrap_or(false),
            window.is_minimized().unwrap_or(true),
        )
    })
}

fn presentation_is_foreground(ready: bool, visible: bool, focused: bool, minimized: bool) -> bool {
    ready && visible && focused && !minimized
}

fn poll(
    service: &mut DesktopService<WindowsClock>,
    app: &AppHandle,
    last_error: &mut Option<String>,
    reminder: &mut BreakReminder,
) {
    match service.poll() {
        Ok(update) => {
            *last_error = None;
            // The clock's epoch can change between the worker's lifecycle check
            // and this sample. Do not publish a recovery choice or deadline.
            if matches!(update.checkpoint.state, TimerState::RecoveryRequired(_)) {
                if let Err(error) = interrupt_worker(service, app, reminder) {
                    *last_error = Some(error.clone());
                    let _ = app.emit("rhythm:error", error);
                }
                return;
            }
            publish_terminal(
                app,
                update.terminal.as_ref(),
                &update.checkpoint.state,
                reminder,
            );
            let _ = app.emit("rhythm:timer", update);
        }
        Err(error) => {
            let message = error.to_string();
            // Keep retrying the transaction, but don't flood an editor with identical errors.
            if last_error.as_ref() != Some(&message) {
                let _ = app.emit("rhythm:error", &message);
                *last_error = Some(message);
            }
        }
    }
}

fn timer_change(request: &DesktopRequest) -> bool {
    matches!(
        request,
        DesktopRequest::ExtendFocus { .. }
            | DesktopRequest::SwitchToFocus { .. }
            | DesktopRequest::SwitchToBreak { .. }
            | DesktopRequest::StartFocus { .. }
            | DesktopRequest::StartRest { .. }
            | DesktopRequest::Pause
            | DesktopRequest::Resume
            | DesktopRequest::EndEarly
            | DesktopRequest::NewRhythm
            | DesktopRequest::Recover { .. }
            | DesktopRequest::AcceptInvitation { .. }
    )
}

fn interrupt_worker(
    service: &mut DesktopService<WindowsClock>,
    app: &AppHandle,
    reminder: &mut BreakReminder,
) -> Result<(), String> {
    reminder.clear(); // Invalidate old actions before publishing any new state.
    let update = service
        .interrupt_for_desktop()
        .map_err(|error| error.to_string())?;
    // The database is authoritative even if deleting a displayed banner fails.
    reminder.persist()?;
    if let Err(message) = notification::clear_history(&app.config().identifier) {
        let _ = app.emit("rhythm:error", message);
    }
    let _ = app.emit("rhythm:reminder", Option::<ReminderDescriptor>::None);
    let _ = app.emit("rhythm:lifecycle_interrupted", &update);
    let _ = app.emit("rhythm:timer", update);
    Ok(())
}

fn start_worker(app: AppHandle, path: PathBuf) -> Result<Desktop, Box<dyn std::error::Error>> {
    let control_app = app.clone();
    let completion_app = app.clone();
    let sync_status_app = app.clone();
    let sync_history_app = app.clone();
    let quit = Arc::new(QuitCoordinator::default());
    let worker_quit = quit.clone();
    let shutdown_pending = Arc::new(AtomicBool::new(false));
    let worker_shutdown = shutdown_pending.clone();
    let (sender, receiver) = mpsc::channel();
    let control_directory = path
        .parent()
        .ok_or("Missing application data directory")?
        .to_path_buf();
    let (ready, startup) = mpsc::channel::<Result<PlatformStatus, String>>();
    thread::Builder::new().name("rhythm-core".into()).spawn(move || {
        let clock = WindowsClock::new();
        let sleep_inclusive = clock.capability() != ClockCapability::WindowsAwakeOnly;
        let reminder_path = path.with_file_name("reminder.json");
        let mut service = match DesktopService::open(path, clock.clone()) {
            Ok(service) => service,
            Err(error) => { let _ = ready.send(Err(error.to_string())); return; }
        };
        // A new process must never continue or complete an old desktop period.
        if let Err(error) = service.interrupt_for_desktop() {
            let _ = ready.send(Err(error.to_string())); return;
        }
        let mut profile_restore_pending = match service.pending_desktop_profile() {
            Ok(pending) => pending.is_some(),
            Err(error) => { let _ = ready.send(Err(error.to_string())); return; }
        };
        let mut break_reminder = match service.snapshot().map_err(|error| error.to_string())
            .and_then(|view| BreakReminder::open_for_desktop(reminder_path, &view, &clock.sample())) {
            Ok(reminder) => reminder,
            Err(error) => { let _ = ready.send(Err(error)); return; }
        };
        let _ = notification::clear_history(&app.config().identifier);
        // Keep the subscription guard alive until this worker has stopped.
        let power = PowerSubscription::subscribe().ok();
        let status = PlatformStatus {
            sleep_inclusive,
            power_events: power.is_some(),
            notification_note: "Windows settings and Do not disturb may hide banners. The test reports native submission errors; acceptance does not confirm a visible banner.".into(),
        };
        if ready.send(Ok(status)).is_err() { return; }
        let mut last_error = None;
        let mut last_routines = None;
        let mut announced_invitations = HashSet::new();
        let mut next_poll = Instant::now() + Duration::from_secs(1);
        let mut generation = clock.interruption_generation();
        let mut interruption_pending = false;
        let mut awaiting_resume = false;
        let mut lifecycle_acks = Vec::new();
        loop {
            // A bounded wait makes native power hints and UI requests independent of
            // WebView timer throttling. The native awake clock measures elapsed time.
            let result = receiver.recv_timeout(next_poll.saturating_duration_since(Instant::now()));
            let mut should_poll = false;
            // Native callbacks latch a generation before enqueueing hints. A late
            // duplicate resume must never end a newly deliberate Focus.
            if let Some((_, events)) = &power { for _ in events.try_iter() {} }
            let observed_generation = clock.interruption_generation();
            if observed_generation != generation { awaiting_resume = true; }
            if observed_generation != generation || matches!(&result, Ok(Work::Interrupt)) {
                interruption_pending = true;
                let _ = app.emit("rhythm:lifecycle_prepare", serde_json::json!({}));
            }
            if interruption_pending {
                match interrupt_worker(&mut service, &app, &mut break_reminder) {
                    Ok(()) => { interruption_pending = false; generation = observed_generation; }
                    Err(message) => { last_error = Some(message.clone()); let _ = app.emit("rhythm:error", message); }
                }
            }
            if awaiting_resume && !interruption_pending && !clock.is_suspended() {
                service.suppress_missed_desktop_invitations();
                awaiting_resume = false;
            }
            let mut blocked = profile_restore_pending || interruption_pending || clock.is_suspended() || worker_shutdown.load(Ordering::Acquire);
            match result {
                Ok(Work::Control(request, issued_generation, reply)) => {
                    let changes_timer = request.operation == "execute" && request.request.as_ref()
                        .and_then(|value| serde_json::from_value::<DesktopRequest>(value.clone()).ok())
                        .is_some_and(|request| timer_change(&request));
                    let result = if profile_restore_pending && request.operation == "execute" {
                        ControlResponse::failure(request.request_id, "restore_pending", "Reopen Rhythm to finish restoring your preferences before making changes.")
                    } else if changes_timer && (blocked || issued_generation != generation) {
                        ControlResponse::failure(request.request_id, "interrupted", "Focus was interrupted. Start Focus again when you are ready.")
                    } else { handle_control(&mut service, &app, request, &mut break_reminder) };
                    let _ = reply.send(Ok(result));
                }
                Ok(Work::Snapshot(reply)) => {
                    let _ = reply.send(service.snapshot().map_err(|error| error.to_string()));
                }
                Ok(Work::NoteHistory(cursor, limit, periods_only, reply)) => {
                    let _ = reply.send(service.note_history_page(cursor.as_ref(), limit, periods_only).map_err(|error| error.to_string()));
                }
                Ok(Work::QueryNoteHistory(query, cursor, limit, reply)) => {
                    let _ = reply.send(service.query_note_history(&query, cursor.as_ref(), limit).map_err(|error| error.to_string()));
                }
                Ok(Work::SyncSnapshot(reply)) => {
                    let result = if profile_restore_pending { Err("Finish restoring the journal before syncing.".into()) }
                        else { service.sync_objects().map_err(|error| error.to_string()) };
                    let _ = reply.send(result);
                }
                Ok(Work::SyncImport(device_id, archive, cancellation, reply)) => {
                    let result = if cancellation.cancelled() || profile_restore_pending || service.sync_device_id() != device_id {
                        Err("Sync was stopped or the journal was restored. Local writing is safe.".into())
                    } else { service.import_sync_archive(&archive).map_err(|error| error.to_string()) };
                    let _ = reply.send(result);
                }
                Ok(Work::CurrentReminder(reply)) => {
                    if !blocked && break_reminder.advance(&clock.sample()) {
                        publish_reminder(&app, &break_reminder, true);
                    }
                    let _ = reply.send(Ok(break_reminder.descriptor()));
                }
                Ok(Work::Execute(request, issued_generation, reply)) => {
                    if profile_restore_pending {
                        let _ = reply.send(Err("Reopen Rhythm to finish restoring your preferences before making changes.".into()));
                        continue;
                    }
                    if timer_change(&request) && (blocked || issued_generation != generation) {
                        let _ = reply.send(Err("Focus was interrupted. Start Focus again when you are ready.".into()));
                        continue;
                    }
                    let clears_reminder = matches!(request, DesktopRequest::NewRhythm);
                    let result = service.execute(request).map_err(|error| error.to_string());
                    if let Ok(response) = &result {
                        if clears_reminder && break_reminder.clear() { publish_reminder(&app, &break_reminder, false); }
                        publish_terminal(&app, response.terminal.as_ref(), &response.view.checkpoint.state, &mut break_reminder);
                        let _ = app.emit("rhythm:timer", TimerUpdate {
                            checkpoint: response.view.checkpoint.clone(),
                            revision: response.view.revision,
                            terminal: response.terminal.clone(),
                            notes_revision: response.view.note_state.notes_revision,
                            note_period_id: response.view.note_state.period_note.as_ref().map(|note| note.id.clone()),
                        });
                    }
                    let _ = reply.send(result);
                }
                Ok(Work::RespondBreak(choice, reply)) => {
                    let result = if blocked { Ok(None) } else { respond_to_reminder(&mut service, &app, &mut break_reminder,
                        &worker_quit, choice) };
                    let _ = reply.send(result);
                }
                Ok(Work::Quit(reply)) => {
                    let result = service.finish_for_quit().map(|_| ()).map_err(|error| error.to_string());
                    let done = result.is_ok();
                    let _ = reply.send(result);
                    if done { break; }
                }
                Ok(Work::Backup(path, profile, reply)) => {
                    let result = if profile_restore_pending { Err("Finish restoring preferences before making another backup.".into()) }
                        else { service.backup_complete_to(&path, &profile).map_err(|error| error.to_string()) };
                    let _ = reply.send(result);
                }
                Ok(Work::Export(path, reply)) => {
                    let _ = reply.send(service.export_markdown_to(&path).map_err(|error| error.to_string()));
                }
                Ok(Work::InspectBackup(path, reply)) => {
                    let _ = reply.send(service.inspect_backup_profile(&path).map_err(|error| error.to_string()));
                }
                Ok(Work::PendingProfile(reply)) => {
                    let _ = reply.send(service.pending_desktop_profile().map_err(|error| error.to_string()));
                }
                Ok(Work::AcknowledgeProfile(token, reply)) => {
                    let result = service.acknowledge_desktop_profile_restore(&token).map_err(|error| error.to_string());
                    if matches!(result, Ok(true)) {
                        service.suppress_missed_desktop_invitations();
                        profile_restore_pending = false;
                    }
                    let _ = reply.send(result);
                }
                Ok(Work::Restore(path, current_profile, expected_profile, reply)) => {
                    if blocked { let _ = reply.send(Err("Wait until the desktop interruption has finished.".into())); continue; }
                    let result = service.restore_complete_from(&path, &current_profile, &expected_profile).map_err(|error| error.to_string());
                    if let Ok(restored) = &result {
                        profile_restore_pending = restored.pending_profile.is_some();
                        last_error = None; break_reminder.clear();
                        publish_reminder(&app, &break_reminder, false);
                        let _ = notification::clear_history(&app.config().identifier);
                    }
                    let _ = reply.send(result);
                }
                Ok(Work::Interrupt) => {}
                Ok(Work::LifecycleSaved(ack)) => {
                    // Keep the acknowledgement through a transient checkpoint
                    // failure; shutdown can finish once the retry commits.
                    lifecycle_acks.push(ack);
                }
                Ok(Work::Wake) | Err(mpsc::RecvTimeoutError::Timeout) => should_poll = true,
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
            // A suspend can arrive while a queued request is being committed.
            // Reconcile that boundary before any notification/routine poll.
            let after_generation = clock.interruption_generation();
            if after_generation != generation
                || matches!(service.current_timer().checkpoint.state, TimerState::RecoveryRequired(_))
            {
                if after_generation != generation { awaiting_resume = true; }
                interruption_pending = true;
                let _ = app.emit("rhythm:lifecycle_prepare", serde_json::json!({}));
                match interrupt_worker(&mut service, &app, &mut break_reminder) {
                    Ok(()) => { interruption_pending = false; generation = after_generation; }
                    Err(message) => { last_error = Some(message.clone()); let _ = app.emit("rhythm:error", message); }
                }
                blocked = profile_restore_pending || interruption_pending || clock.is_suspended() || worker_shutdown.load(Ordering::Acquire);
            }
            if awaiting_resume && !interruption_pending && !clock.is_suspended() {
                service.suppress_missed_desktop_invitations();
                awaiting_resume = false;
            }
            if !interruption_pending {
                for ack in lifecycle_acks.drain(..) { let _ = ack.send(()); }
            }
            blocked |= profile_restore_pending;
            if !blocked && (should_poll || Instant::now() >= next_poll) {
                poll(&mut service, &app, &mut last_error, &mut break_reminder);
            }
            if !blocked { publish_routines(&mut service, &app, &mut last_routines, &mut announced_invitations); }
            if Instant::now() >= next_poll { next_poll = Instant::now() + Duration::from_secs(1); }
        }
    })?;
    let status = startup.recv()?.map_err(std::io::Error::other)?;
    let control_sender = sender.clone();
    let control_quit_state = quit.clone();
    let completion_quit = quit.clone();
    let control = rhythm_control::start_server_with_completion(
        &control_directory,
        move |request| {
            if request.operation == "quit" {
                return control_quit(
                    &control_app,
                    &control_sender,
                    &control_quit_state,
                    request.request_id,
                );
            }
            let id = request.request_id.clone();
            let (reply, response) = mpsc::channel();
            if control_sender
                .send(Work::Control(
                    request,
                    WindowsClock::new().interruption_generation(),
                    reply,
                ))
                .is_err()
            {
                return ControlResponse::failure(
                    id,
                    "app_unavailable",
                    "The desktop worker has stopped. Reopen Rhythm.",
                );
            }
            match response.recv() {
                Ok(Ok(response)) => response,
                _ => ControlResponse::failure(
                    id,
                    "outcome_unknown",
                    "The worker stopped before confirming the action. Inspect state before retrying.",
                ),
            }
        },
        move |response| {
            if response.ok && completion_quit.take_exit(&response.request_id) {
                completion_app.exit(0);
            }
        },
    )?;
    let sync = automatic_sync::Manager::start(
        control_directory,
        SyncJournal(sender.clone()),
        move |status| {
            let _ = sync_status_app.emit("rhythm:sync", status);
        },
        move || {
            let _ = sync_history_app.emit("rhythm:sync_history", ());
        },
    )
    .map_err(std::io::Error::other)?;
    Ok(Desktop {
        sender,
        status,
        language: Mutex::new("en".into()),
        ui_ready: AtomicBool::new(false),
        quit,
        shutdown_pending,
        lifecycle_save: Mutex::new(None),
        selected_restore: Mutex::new(None),
        sync,
        _control: control,
    })
}

fn respond_to_reminder(
    service: &mut DesktopService<WindowsClock>,
    app: &AppHandle,
    reminder: &mut BreakReminder,
    quit: &QuitCoordinator,
    choice: ReminderChoice,
) -> Result<Option<ReminderResponse>, String> {
    let state = service.current_timer().checkpoint.state;
    if !quit.is_idle() || !reminder.is_current(&state, &choice.period_id, choice.sequence) {
        return Ok(None);
    }
    let response = match choice.action {
        BreakAction::Snooze => {
            let view = service.snapshot().map_err(|error| error.to_string())?;
            if !reminder.postpone(
                &state,
                &choice.period_id,
                choice.sequence,
                choice.duration_ms,
                &WindowsClock::new().sample(),
            )? {
                return Ok(None);
            }
            // A postponement changes only its private reminder schedule. It
            // never starts/finishes a period or advances recovery cadence.
            publish_reminder(app, reminder, false);
            DesktopResponse {
                view,
                draft: None,
                period_note: None,
                terminal: None,
            }
        }
        BreakAction::Accept => {
            let Some(request) = reminder.accept_request(
                &state,
                &choice.period_id,
                choice.sequence,
                choice.duration_ms,
            ) else {
                return Ok(None);
            };
            let response = service
                .execute(request)
                .map_err(|error| error.to_string())?;
            reminder.clear();
            publish_reminder(app, reminder, false);
            let _ = app.emit(
                "rhythm:timer",
                TimerUpdate {
                    checkpoint: response.view.checkpoint.clone(),
                    revision: response.view.revision,
                    terminal: response.terminal.clone(),
                    notes_revision: response.view.note_state.notes_revision,
                    note_period_id: response
                        .view
                        .note_state
                        .period_note
                        .as_ref()
                        .map(|note| note.id.clone()),
                },
            );
            response
        }
    };
    Ok(Some(ReminderResponse {
        response,
        reminder: reminder.descriptor(),
    }))
}

fn handle_control(
    service: &mut DesktopService<WindowsClock>,
    app: &AppHandle,
    request: ControlRequest,
    reminder: &mut BreakReminder,
) -> ControlResponse {
    let id = request.request_id;
    let result = match request.operation.as_str() {
        "snapshot" => service
            .snapshot()
            .map_err(|error| error.to_string())
            .and_then(|view| serde_json::to_value(view).map_err(|error| error.to_string())),
        "status" => {
            let timer = service.current_timer();
            let routines = service.routine_view();
            Ok(
                serde_json::json!({"checkpoint":timer.checkpoint,"revision":timer.revision,"invitations":routines.invitations,"routine_windows":routines.routine_windows,"selected_routine_window":routines.selected_routine_window}),
            )
        }
        "settings" => Ok(serde_json::json!(
            service.current_timer().checkpoint.settings
        )),
        "routines" => Ok(serde_json::json!(service.routine_view())),
        "invitations" => {
            let routines = service.routine_view();
            Ok(
                serde_json::json!({"invitations":routines.invitations,"routine_windows":routines.routine_windows,"selected_routine_window":routines.selected_routine_window}),
            )
        }
        "journal" => service
            .journal()
            .map(|entries| serde_json::json!({"reflections":entries,"limit":100}))
            .map_err(|error| error.to_string()),
        "drafts" => service
            .drafts()
            .map(|entries| serde_json::json!({"drafts":entries}))
            .map_err(|error| error.to_string()),
        "execute" => {
            let action = match request
                .request
                .and_then(|value| serde_json::from_value::<DesktopRequest>(value).ok())
            {
                Some(action) => action,
                None => {
                    return ControlResponse::failure(
                        id,
                        "invalid_request",
                        "Invalid or unsupported desktop action. Run rhythm-cli --json schema.",
                    );
                }
            };
            let clears_reminder = matches!(action, DesktopRequest::NewRhythm);
            service.execute(action).map(|response| {
                if clears_reminder && reminder.clear() { publish_reminder(app, reminder, false); }
                publish_terminal(app, response.terminal.as_ref(), &response.view.checkpoint.state, reminder);
                let _ = app.emit("rhythm:timer", TimerUpdate { checkpoint:response.view.checkpoint.clone(),revision:response.view.revision,terminal:response.terminal.clone(), notes_revision:response.view.note_state.notes_revision, note_period_id:response.view.note_state.period_note.as_ref().map(|note| note.id.clone()) });
                let _ = app.emit("rhythm:external_change", ());
                // Machine mutation responses contain only the changed draft and timer;
                // they do not echo the rest of the person's journal.
                serde_json::json!({"checkpoint":response.view.checkpoint,"revision":response.view.revision,"draft":response.draft,"period_note":response.period_note,"terminal":response.terminal,"routines":response.view.routine_state})
            }).map_err(|error|error.to_string())
        }
        _ => return ControlResponse::failure(id, "invalid_request", "Unsupported operation."),
    };
    match result {
        Ok(value) => ControlResponse::success(id, value),
        Err(error) => ControlResponse::failure(id, "action_rejected", error),
    }
}

fn publish_routines(
    service: &mut DesktopService<WindowsClock>,
    app: &AppHandle,
    previous: &mut Option<RoutineView>,
    announced: &mut HashSet<(String, i64)>,
) {
    let state = service.poll_routines();
    announced.retain(|key| {
        state
            .invitations
            .iter()
            .any(|item| (&item.routine_id, item.due_at_utc_ms) == (&key.0, key.1))
    });
    let ready = service.ready_for_invitation();
    let mut needs_notice = false;
    if ready {
        for invitation in &state.invitations {
            needs_notice |=
                announced.insert((invitation.routine_id.clone(), invitation.due_at_utc_ms));
        }
    }
    if needs_notice && !foreground_can_present(app) {
        let (title, body) = match language(app).as_str() {
            "zh-Hans" => (
                "留一点专注的时间",
                "一个日常提醒到了。准备好后，回到 Rhythm 选择是否开始。",
            ),
            "zh-Hant" => (
                "留一點專注的時間",
                "一個日常提醒到了。準備好後，回到 Rhythm 選擇是否開始。",
            ),
            _ => (
                "A little room for focus",
                "A routine invitation is ready. Return to Rhythm and begin when you choose.",
            ),
        };
        let callback_app = app.clone();
        if let Err(error) =
            notification::show(&app.config().identifier, title, body, move |error| {
                let _ = callback_app.emit("rhythm:error", error);
            })
        {
            let _ = app.emit("rhythm:error", error);
        }
    }
    if previous.as_ref() != Some(&state) {
        let _ = app.emit("rhythm:routines", &state);
        *previous = Some(state);
    }
}

pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _, _| {
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.unminimize();
                let _ = window.show();
                let _ = window.set_focus();
            }
        }))
        .setup(|app| {
            // The override supports isolated development/QA, never comes from web content.
            let directory = std::env::var_os("RHYTHM_DATA_DIR")
                .map(PathBuf::from).unwrap_or(app.path().app_local_data_dir()?);
            std::fs::create_dir_all(&directory)?;
            app.manage(start_worker(app.handle().clone(), directory.join("rhythm.sqlite3"))?);
            if let Some(window) = app.get_webview_window("main") {
                let prepare_app = app.handle().clone();
                let cancel_app = app.handle().clone();
                crate::lifecycle::install(&window,
                    move || prepare_shutdown(&prepare_app),
                    move || cancel_shutdown(&cancel_app),
                ).map_err(std::io::Error::other)?;
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![sync_status, connect_sync, retry_sync, disconnect_sync, snapshot, note_history, note_history_query, execute, respond_break_reminder, current_reminder, backup_data, export_markdown, save_encrypted_notes, choose_restore_data, cancel_restore_data, commit_restore_data, pending_profile_restore, acknowledge_profile_restore, platform_status, set_fullscreen, set_notification_language, test_notification, notification_status, quit, prepare_cli_quit_result, lifecycle_saved, ui_ready])
        .on_window_event(|window, event| match event {
            tauri::WindowEvent::CloseRequested { api, .. } => {
                if let Some(state) = window.try_state::<Desktop>()
                    && state.ui_ready.load(Ordering::Acquire)
                {
                    api.prevent_close();
                    // The editor flushes its draft before the user confirms an explicit quit.
                    let _ = window.emit("rhythm:close_requested", ());
                }
            }
            tauri::WindowEvent::Focused(true) => {
                if let Some(state) = window.try_state::<Desktop>() {
                    let _ = state.sender.send(Work::Wake);
                }
            }
            _ => {}
        })
        .run(tauri::generate_context!())
        .expect("Rhythm could not start. Check the local data folder and Windows WebView2 installation.");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cancelling_an_older_restore_dialog_keeps_the_new_selection() {
        let mut selected = Some(SelectedRestore {
            token: "new-selection".into(),
            path: PathBuf::from("current-backup.sqlite3"),
            profile: None,
        });
        assert!(!cancel_restore_selection(&mut selected, "older-selection"));
        assert_eq!(
            selected.as_ref().unwrap().path,
            PathBuf::from("current-backup.sqlite3")
        );
        assert!(cancel_restore_selection(&mut selected, "new-selection"));
        assert!(selected.is_none());
        assert!(!cancel_restore_selection(&mut selected, "new-selection"));
    }

    #[test]
    fn only_a_ready_visible_foreground_window_replaces_an_os_notice() {
        for ready in [false, true] {
            for visible in [false, true] {
                for focused in [false, true] {
                    for minimized in [false, true] {
                        assert_eq!(
                            presentation_is_foreground(ready, visible, focused, minimized),
                            (ready, visible, focused, minimized) == (true, true, true, false),
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn notices_use_fixed_copy_in_every_supported_language() {
        for locale in ["en", "zh-Hans", "zh-Hant"] {
            for kind in [ReminderKind::Focus, ReminderKind::Break] {
                let labels = break_action_labels(locale, kind);
                assert!(!labels.accept.is_empty());
                assert!(!labels.snooze.is_empty());
                assert_ne!(labels.accept, labels.snooze);
                assert!(!labels.snooze.chars().any(|ch| ch.is_ascii_digit()));
            }
            for kind in [
                None,
                Some(PeriodKind::Focus),
                Some(PeriodKind::ShortRest),
                Some(PeriodKind::LongRest),
            ] {
                let (title, body) = notification_copy(locale, kind);
                assert!(!title.is_empty());
                assert!(!body.is_empty());
                assert!(!body.contains("session_id"));
            }
        }
    }
}
