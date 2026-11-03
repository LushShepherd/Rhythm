//! Windows toast adapter with observable native failures.
//!
//! tauri-plugin-notification 2.8.0 returns after preparing its payload and discards
//! the Windows send result on a spawned task. This adapter calls WinRT directly,
//! checks the app's actual notification setting, and retains Failed handlers.
//! A successful submission still cannot prove a banner was visible: Windows may
//! route it to notification center or suppress it under Do not disturb.
//!
//! The installer owns the AUMID shortcut. This module never registers identities,
//! changes notification settings, reads other apps' history, or includes journal text.

use serde::Serialize;
use std::{collections::VecDeque, panic::AssertUnwindSafe, sync::Mutex};
use uuid::Uuid;
use windows::{
    Data::Xml::Dom::XmlDocument,
    Foundation::TypedEventHandler,
    UI::Notifications::{
        NotificationSetting, ToastActivatedEventArgs, ToastFailedEventArgs, ToastNotification,
        ToastNotificationManager,
    },
    core::{HSTRING, IInspectable, Interface, h},
};

const MAX_TRACKED_TOASTS: usize = 32;
const GROUP: &str = "rhythm";

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct NotificationPermission {
    pub code: i32,
    pub name: &'static str,
    pub enabled: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct NotificationReceipt {
    pub app_id: String,
    pub tag: String,
    pub setting: NotificationPermission,
    /// WinRT Show returned successfully; this does not assert a visible banner.
    pub submitted: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct NotificationDiagnostics {
    pub app_id: String,
    pub setting: NotificationPermission,
    pub history_count: Option<u32>,
    /// Only this app's tags are inspected. No toast payloads leave the native adapter.
    pub history_tags: Vec<String>,
    pub history_error: Option<String>,
}

struct TrackedToast {
    toast: ToastNotification,
    failed_token: i64,
    activated_token: Option<i64>,
}

impl Drop for TrackedToast {
    fn drop(&mut self) {
        let _ = self.toast.RemoveFailed(self.failed_token);
        if let Some(token) = self.activated_token {
            let _ = self.toast.RemoveActivated(token);
        }
    }
}

// WinRT ToastNotification is agile (Send + Sync in the generated Windows API).
// Keep failure callbacks alive for the latest attempts without unbounded retention.
static TRACKED: Mutex<VecDeque<TrackedToast>> = Mutex::new(VecDeque::new());

/// Retire this app's outstanding reminders at a lifecycle interruption.
///
/// The worker must invalidate its persisted reminder first: native removal is
/// best effort, and an activation already in flight must still fail the worker's
/// period/sequence guard. No other application's notification history is read
/// or removed. This also retires Rhythm's stale group from earlier processes.
pub fn clear_history(app_id: &str) -> Result<(), String> {
    validate_app_id(app_id)?;
    let retired = {
        let mut tracked = TRACKED.lock().unwrap_or_else(|p| p.into_inner());
        std::mem::take(&mut *tracked)
    };
    let toasts: Vec<_> = retired.iter().map(|item| item.toast.clone()).collect();
    // Revoke event handlers outside the mutex; COM may re-enter a callback.
    drop(retired);

    let identity = HSTRING::from(app_id);
    let mut failure = None;
    match ToastNotificationManager::CreateToastNotifierWithId(&identity) {
        Ok(notifier) => {
            for toast in &toasts {
                if let Err(error) = notifier.Hide(toast) {
                    failure
                        .get_or_insert_with(|| native_error("hide an interrupted reminder", error));
                }
            }
        }
        Err(error) => {
            failure = Some(native_error("open Rhythm's notification identity", error));
        }
    }
    if let Err(error) = ToastNotificationManager::History()
        .and_then(|history| history.RemoveGroupWithId(&HSTRING::from(GROUP), &identity))
    {
        failure.get_or_insert_with(|| native_error("remove interrupted Rhythm reminders", error));
    }
    match failure {
        Some(error) => Err(error),
        None => Ok(()),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToastActivation {
    Open,
    Accept,
    Snooze,
}

pub struct BreakActionLabels<'a> {
    pub accept: &'a str,
    pub snooze: &'a str,
}

type ActivationCallback = Box<dyn Fn(ToastActivation) + Send + Sync + 'static>;

fn activation(arguments: &str) -> Option<ToastActivation> {
    match arguments {
        "rhythm:break:open" => Some(ToastActivation::Open),
        "rhythm:break:accept" => Some(ToastActivation::Accept),
        "rhythm:break:snooze" => Some(ToastActivation::Snooze),
        _ => None,
    }
}

fn validate_app_id(app_id: &str) -> Result<(), String> {
    if app_id.is_empty()
        || app_id.len() > 128
        || !app_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
    {
        return Err("The configured Windows notification identity is invalid.".into());
    }
    Ok(())
}

fn permission(setting: NotificationSetting) -> NotificationPermission {
    let name = match setting {
        NotificationSetting::Enabled => "enabled",
        NotificationSetting::DisabledForApplication => "disabled_for_application",
        NotificationSetting::DisabledForUser => "disabled_for_user",
        NotificationSetting::DisabledByGroupPolicy => "disabled_by_group_policy",
        NotificationSetting::DisabledByManifest => "disabled_by_manifest",
        _ => "unknown",
    };
    NotificationPermission {
        code: setting.0,
        name,
        enabled: setting == NotificationSetting::Enabled,
    }
}

fn require_enabled(setting: &NotificationPermission) -> Result<(), String> {
    if setting.enabled {
        return Ok(());
    }
    let reason = match setting.code {
        1 => "Notifications for Rhythm are turned off in Windows.",
        2 => "Windows notifications are turned off for this user.",
        3 => "Windows notifications are disabled by an administrator's policy.",
        4 => {
            "Windows does not recognize Rhythm as notification capable. Reinstall Rhythm using its installer."
        }
        _ => "Windows returned an unknown notification setting.",
    };
    Err(format!(
        "{reason} No notification was submitted (setting {}).",
        setting.code
    ))
}

fn native_error(operation: &str, error: windows::core::Error) -> String {
    // HRESULT is actionable without logging any title, body, or journal content.
    format!(
        "Windows could not {operation} (0x{:08X}): {}",
        error.code().0 as u32,
        error.message()
    )
}

fn build_content(
    title: &str,
    body: &str,
    labels: Option<&BreakActionLabels<'_>>,
) -> windows::core::Result<XmlDocument> {
    let document = XmlDocument::new()?;
    let toast = document.CreateElement(h!("toast"))?;
    // Ordinary, brief reminders let Windows honor notification preferences and
    // Do not disturb. Never use alarm/reminder scenarios or looping audio.
    toast.SetAttribute(h!("duration"), h!("short"))?;
    let visual = document.CreateElement(h!("visual"))?;
    let binding = document.CreateElement(h!("binding"))?;
    binding.SetAttribute(h!("template"), h!("ToastGeneric"))?;
    for value in [title, body] {
        let text = document.CreateElement(h!("text"))?;
        text.SetInnerText(&HSTRING::from(value))?;
        binding.AppendChild(&text)?;
    }
    visual.AppendChild(&binding)?;
    toast.AppendChild(&visual)?;
    if let Some(labels) = labels {
        toast.SetAttribute(h!("launch"), h!("rhythm:break:open"))?;
        let actions = document.CreateElement(h!("actions"))?;
        for (label, arguments) in [
            (labels.accept, "rhythm:break:accept"),
            (labels.snooze, "rhythm:break:snooze"),
        ] {
            let action = document.CreateElement(h!("action"))?;
            action.SetAttribute(h!("content"), &HSTRING::from(label))?;
            action.SetAttribute(h!("arguments"), &HSTRING::from(arguments))?;
            // For this running Win32 app Activated delivers both button choices.
            // The callback decides whether to reveal the window; Snooze does not.
            action.SetAttribute(h!("activationType"), h!("foreground"))?;
            actions.AppendChild(&action)?;
        }
        toast.AppendChild(&actions)?;
    }
    document.AppendChild(&toast)?;
    Ok(document)
}

fn new_tag() -> String {
    // Windows limits each tag to 16 characters. A random 64-bit prefix carries no
    // session identifier, wall-clock value, or private content.
    Uuid::new_v4().simple().to_string()[..16].to_owned()
}

/// Submit a normal Windows toast, preserving synchronous errors and later Failed events.
/// Run on a native worker or blocking pool. The callback should only queue/report an error.
pub fn show(
    app_id: &str,
    title: &str,
    body: &str,
    on_failure: impl Fn(String) + Send + Sync + 'static,
) -> Result<NotificationReceipt, String> {
    show_with_actions(app_id, title, body, None, None, on_failure)
}

/// Action callbacks live with this process. Closing Rhythm does not register an
/// OS background task, restart handler, or automatic launch command.
pub fn show_break(
    app_id: &str,
    title: &str,
    body: &str,
    labels: BreakActionLabels<'_>,
    on_activation: impl Fn(ToastActivation) + Send + Sync + 'static,
    on_failure: impl Fn(String) + Send + Sync + 'static,
) -> Result<NotificationReceipt, String> {
    show_with_actions(
        app_id,
        title,
        body,
        Some(labels),
        Some(Box::new(on_activation)),
        on_failure,
    )
}

fn show_with_actions(
    app_id: &str,
    title: &str,
    body: &str,
    labels: Option<BreakActionLabels<'_>>,
    on_activation: Option<ActivationCallback>,
    on_failure: impl Fn(String) + Send + Sync + 'static,
) -> Result<NotificationReceipt, String> {
    validate_app_id(app_id)?;
    let notifier = ToastNotificationManager::CreateToastNotifierWithId(&HSTRING::from(app_id))
        .map_err(|error| native_error("open Rhythm's notification identity", error))?;
    let setting = permission(
        notifier
            .Setting()
            .map_err(|error| native_error("read the notification setting", error))?,
    );
    require_enabled(&setting)?;
    let content = build_content(title, body, labels.as_ref())
        .map_err(|error| native_error("prepare the notification", error))?;
    let toast = ToastNotification::CreateToastNotification(&content)
        .map_err(|error| native_error("create the notification", error))?;
    let tag = new_tag();
    toast
        .SetTag(&HSTRING::from(&tag))
        .map_err(|error| native_error("label the notification", error))?;
    toast
        .SetGroup(&HSTRING::from(GROUP))
        .map_err(|error| native_error("group the notification", error))?;
    let handler = TypedEventHandler::<ToastNotification, ToastFailedEventArgs>::new(
        move |_, args| {
            let message = match args.as_ref().map(ToastFailedEventArgs::ErrorCode) {
            Some(Ok(code)) => format!("Windows could not display the submitted Rhythm notification (0x{:08X}).", code.0 as u32),
            Some(Err(error)) => native_error("read why the notification failed", error),
            None => "Windows reported that the submitted Rhythm notification could not be displayed.".into(),
        };
            // Keep a callback panic from unwinding across the operating-system boundary.
            let _ = std::panic::catch_unwind(AssertUnwindSafe(|| on_failure(message)));
            Ok(())
        },
    );
    let failed_token = toast
        .Failed(&handler)
        .map_err(|error| native_error("observe notification failures", error))?;
    let mut tracked = TrackedToast {
        toast,
        failed_token,
        activated_token: None,
    };
    if let Some(on_activation) = on_activation {
        let handler = TypedEventHandler::<ToastNotification, IInspectable>::new(move |_, args| {
            let action = args
                .as_ref()
                .and_then(|args| args.cast::<ToastActivatedEventArgs>().ok())
                .and_then(|args| args.Arguments().ok())
                .and_then(|args| activation(&args.to_string()));
            if let Some(action) = action {
                let _ = std::panic::catch_unwind(AssertUnwindSafe(|| on_activation(action)));
            }
            Ok(())
        });
        tracked.activated_token = Some(
            tracked
                .toast
                .Activated(&handler)
                .map_err(|error| native_error("observe notification actions", error))?,
        );
    }
    notifier
        .Show(&tracked.toast)
        .map_err(|error| native_error("submit the notification", error))?;
    let old = {
        let mut pending = TRACKED
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let old = if pending.len() >= MAX_TRACKED_TOASTS {
            pending.pop_front()
        } else {
            None
        };
        pending.push_back(tracked);
        old
    };
    // Revoke outside the mutex, because COM calls can re-enter user callbacks.
    drop(old);
    Ok(NotificationReceipt {
        app_id: app_id.into(),
        tag,
        setting,
        submitted: true,
    })
}

/// Read only this configured app identity's Windows permission and retained history tags.
/// Neither history presence nor Enabled proves that a visible banner reached the user.
pub fn diagnostics(app_id: &str) -> Result<NotificationDiagnostics, String> {
    validate_app_id(app_id)?;
    let identity = HSTRING::from(app_id);
    let notifier = ToastNotificationManager::CreateToastNotifierWithId(&identity)
        .map_err(|error| native_error("open Rhythm's notification identity", error))?;
    let setting = permission(
        notifier
            .Setting()
            .map_err(|error| native_error("read the notification setting", error))?,
    );
    let history = (|| -> windows::core::Result<(u32, Vec<String>)> {
        let history = ToastNotificationManager::History()?.GetHistoryWithId(&identity)?;
        let count = history.Size()?;
        let mut tags = Vec::new();
        for index in 0..count.min(MAX_TRACKED_TOASTS as u32) {
            tags.push(history.GetAt(index)?.Tag()?.to_string());
        }
        Ok((count, tags))
    })();
    let (history_count, history_tags, history_error) = match history {
        Ok((count, tags)) => (Some(count), tags, None),
        Err(error) => (
            None,
            Vec::new(),
            Some(native_error("read Rhythm's notification history", error)),
        ),
    };
    Ok(NotificationDiagnostics {
        app_id: app_id.into(),
        setting,
        history_count,
        history_tags,
        history_error,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_the_installed_identity_and_rejects_invalid_values() {
        assert!(validate_app_id("app.rhythm.quietroom").is_ok());
        for invalid in ["", "Rhythm App", "app\0rhythm", "app/other"] {
            assert!(validate_app_id(invalid).is_err());
        }
        assert!(validate_app_id(&"a".repeat(128)).is_ok());
        assert!(validate_app_id(&"a".repeat(129)).is_err());
    }

    #[test]
    fn disabled_and_unknown_settings_never_report_permission() {
        assert!(require_enabled(&permission(NotificationSetting::Enabled)).is_ok());
        for code in [1, 2, 3, 4, 99] {
            let setting = permission(NotificationSetting(code));
            assert!(!setting.enabled);
            assert_eq!(setting.code, code);
            assert!(require_enabled(&setting).is_err());
        }
        assert_eq!(
            permission(NotificationSetting(3)).name,
            "disabled_by_group_policy"
        );
    }

    #[test]
    fn native_payload_preserves_unicode_and_escapes_markup_as_text() {
        // Constructs XML only; this test never sends a notification.
        let document = build_content(
            "Rhythm & <quiet>",
            "留點空間。\n<script>not markup</script>",
            None,
        )
        .unwrap();
        let texts = document.GetElementsByTagName(h!("text")).unwrap();
        assert_eq!(texts.Length().unwrap(), 2);
        assert_eq!(
            texts.Item(0).unwrap().InnerText().unwrap().to_string(),
            "Rhythm & <quiet>"
        );
        assert_eq!(
            texts.Item(1).unwrap().InnerText().unwrap().to_string(),
            "留點空間。\n<script>not markup</script>"
        );
        assert_eq!(
            document
                .GetElementsByTagName(h!("script"))
                .unwrap()
                .Length()
                .unwrap(),
            0
        );
        assert_eq!(
            document
                .DocumentElement()
                .unwrap()
                .GetAttribute(h!("scenario"))
                .unwrap()
                .to_string(),
            ""
        );
        assert_eq!(
            document
                .DocumentElement()
                .unwrap()
                .GetAttribute(h!("duration"))
                .unwrap()
                .to_string(),
            "short"
        );
    }

    #[test]
    fn break_payload_has_exactly_two_safe_localizable_actions() {
        let labels = BreakActionLabels {
            accept: "休息 & <一下>",
            snooze: "5 分鐘後",
        };
        let document = build_content("A little room to rest", "Body", Some(&labels)).unwrap();
        let actions = document.GetElementsByTagName(h!("action")).unwrap();
        assert_eq!(actions.Length().unwrap(), 2);
        for (index, label, argument) in [
            (0, labels.accept, "rhythm:break:accept"),
            (1, labels.snooze, "rhythm:break:snooze"),
        ] {
            let element = actions
                .Item(index)
                .unwrap()
                .cast::<windows::Data::Xml::Dom::XmlElement>()
                .unwrap();
            assert_eq!(
                element.GetAttribute(h!("content")).unwrap().to_string(),
                label
            );
            assert_eq!(
                element.GetAttribute(h!("arguments")).unwrap().to_string(),
                argument
            );
            assert_eq!(
                element
                    .GetAttribute(h!("activationType"))
                    .unwrap()
                    .to_string(),
                "foreground"
            );
        }
        assert_eq!(
            document
                .DocumentElement()
                .unwrap()
                .GetAttribute(h!("launch"))
                .unwrap()
                .to_string(),
            "rhythm:break:open"
        );
        assert_eq!(
            build_content("Rest complete", "Body", None)
                .unwrap()
                .GetElementsByTagName(h!("action"))
                .unwrap()
                .Length()
                .unwrap(),
            0
        );
    }

    #[test]
    fn action_dispatch_rejects_unknown_or_partial_arguments() {
        assert_eq!(activation("rhythm:break:open"), Some(ToastActivation::Open));
        assert_eq!(
            activation("rhythm:break:accept"),
            Some(ToastActivation::Accept)
        );
        assert_eq!(
            activation("rhythm:break:snooze"),
            Some(ToastActivation::Snooze)
        );
        for invalid in [
            "",
            "accept",
            "rhythm:break:accept&other=1",
            "rhythm:break:snooze/",
            "powershell.exe",
        ] {
            assert_eq!(activation(invalid), None);
        }
    }

    #[test]
    fn tags_fit_windows_limit_without_private_identifiers() {
        for _ in 0..100 {
            let tag = new_tag();
            assert_eq!(tag.len(), 16);
            assert!(tag.bytes().all(|byte| byte.is_ascii_hexdigit()));
        }
    }
}
