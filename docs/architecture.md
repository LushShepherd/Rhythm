# Rhythm architecture proposal

Status: architecture proposal, reviewed November 1, 2026. The initial Rust timer, application service, SQLite store, journal, and CLI are implemented; see [Rust foundation](rust-foundation.md) for the exact current scope. No GUI framework has been selected, and Tauri 2 remains the leading candidate.
Native GUI builds, device tests, performance measurements and release packages remain outstanding.

Rhythm should keep its timer, journal, storage and synchronization rules in a shared Rust core.
Tauri 2 is the leading shell candidate because it keeps the application layer in Rust and supports a shared responsive interface.
That recommendation remains provisional until journal editing, accessibility and lifecycle behavior pass the platform gates below.
The interface prototypes explore interaction design; they do not establish native platform readiness.

## Framework comparison

| Candidate | Fit | Costs and evidence needed |
| --- | --- | --- |
| Tauri 2 with a small web frontend | Shared desktop and mobile interface; Rust application layer; system WebViews; native extension points. | Test WebView2, WKWebView and WebKitGTK differences; Chinese IME; screen readers; native alarm and credential adapters; Fedora and Wayland. |
| Flutter with a Rust core | Shared UI, explicit localization facilities and accessibility support. | Adds Dart, Flutter tooling and an FFI boundary; validate desktop integration and Fedora deployment directly. |
| Slint with Rust | Native rendering with Rust integration; current Android and iOS targets make it a real candidate. | Test long-form editing, IME, assistive technology, touch and mobile services; choose an appropriate toolkit license. |
| Separate native shells | SwiftUI with AppKit/UIKit on Apple platforms, Compose on Android, WinUI on Windows, GTK on Fedora. | Most direct platform integration, but several UI implementations and repeated accessibility/localization work. Keep as a fallback for essential features that shared shells cannot deliver. |

Tauri renders HTML in platform WebViews and connects it to Rust through message passing.
Its mobile plugins can contain Kotlin/Java and Swift; shared UI does not eliminate native integration work.
[Tauri architecture](https://v2.tauri.app/concept/architecture/), [mobile plugins](https://v2.tauri.app/develop/plugins/develop-mobile/).

Flutter's supported deployment list includes Windows, macOS, Android and iOS, but lists Debian and Ubuntu for Linux, not Fedora.
This makes Fedora a direct validation requirement, rather than proof that Flutter cannot run there.
Its FFI support can call a Rust library through a C ABI; a generated Rust bridge would be an additional dependency to evaluate.
[Supported platforms](https://docs.flutter.dev/reference/supported-platforms), [native FFI](https://docs.flutter.dev/platform-integration/bind-native-code).

Slint currently documents Android and iOS support. Its iOS implementation uses the Winit backend and Skia renderer, with Rust as the supported application language.
Its mobile documentation also calls out touch and virtual-keyboard considerations.
[Android](https://docs.slint.dev/latest/docs/slint/guide/platforms/mobile/android/), [iOS](https://docs.slint.dev/latest/docs/slint/guide/platforms/mobile/ios/), [mobile considerations](https://docs.slint.dev/latest/docs/slint/guide/platforms/mobile/general/), [license choices](https://slint.dev/get-started).

Do not select a winner using assumed bundle sizes, battery use or startup performance.
Measure equivalent release builds on representative hardware after choosing the first interface direction.

## Rust module boundaries

| Module | Responsibility | Dependencies |
| --- | --- | --- |
| `rhythm-domain` | Period state machine, cycle policy, reflection types, validated settings and scheduling rules. | Small value-type libraries only; no GUI, database, network or OS access. |
| `rhythm-application` | Commands, queries, transaction orchestration and recovery decisions. | Domain and abstract ports for storage, clocks, alarms, credentials and providers. |
| `rhythm-storage` | SQLite migrations, repositories, transactional outbox and statistics queries. | Domain/application contracts and SQLite. |
| `rhythm-sync` | Versioned record exchange, ancestry/conflict rules and retry orchestration. | Domain/application contracts and abstract remote storage. |
| `rhythm-platform` | Clock samples, lifecycle events, notifications/alarms, credentials and focus capability reporting. | OS bindings and application contracts. |
| `rhythm-export` | Deterministic Markdown journals and user-controlled backups. | Read models and bounded file destinations. |
| `rhythm-review` | Explicitly authorized AI review requests and saved results. | Journal read models, provider adapters and credential references. |
| Shell adapters | Views, navigation, input handling, localization and native callback entry points. | Application API; never direct SQL or provider credentials. |

Dependency direction is inward: shells and infrastructure implement ports owned by the application/domain layers.
A composition root wires concrete adapters together. The domain must be testable without a window, network, filesystem or real clock.
Native alarm callbacks must reach application services without requiring a visible web or Dart UI.

Use coarse-grained, typed operations such as `start_period`, `pause_period`, `save_reflection`, `list_journal` and `get_snapshot`.
Commands validate inputs and carry operation IDs where replay is possible. State-change events carry revisions so stale UI updates can be ignored.
Database and network operations run outside the UI thread; timer correctness never depends on rendering ticks.

The domain counts completed focus periods only. A long rest becomes due after the third and remains due if skipped.
Completing that long rest or deliberately starting a new rhythm resets the cycle. Each period retains the settings snapshot with which it began.

For Tauri, use explicit command permissions and scoped capabilities; expose neither arbitrary SQL nor a general filesystem/shell bridge.
The UI receives application data, not cloud tokens. Treat journal Markdown as untrusted text when displaying it.
[Tauri IPC](https://v2.tauri.app/concept/inter-process-communication/), [capabilities](https://v2.tauri.app/security/capabilities/).

For Flutter, keep Rust FFI asynchronous at the application boundary and use native platform channels for OS services.
For separate Swift/Kotlin shells, UniFFI can generate bindings, but it does not package the Rust library for each target.
[Flutter platform channels](https://docs.flutter.dev/platform-integration/platform-channels), [UniFFI](https://mozilla.github.io/uniffi-rs/latest/).

## Clock and lifecycle policy

Proposed product rule: a running period is an elapsed-time interval, including ordinary backgrounding, screen lock and device sleep.
An explicit pause stops the interval. Recorded period duration is not evidence that the person was continuously concentrating or resting.
This avoids turning Rhythm into an attention surveillance system while keeping timers understandable.

Use a monotonic clock for elapsed intervals, not repeated decrementing counters or wall-clock subtraction.
Rust's `Instant` is opaque and must not be serialized as a restart timestamp.
Its documentation explicitly does not guarantee whether suspend time is included; a portable `Instant` alone cannot implement the proposed sleep policy.
[Rust Instant documentation](https://doc.rust-lang.org/std/time/struct.Instant.html).

The platform clock port must distinguish in-process monotonic samples, verified suspend-inclusive elapsed samples, boot identity and UTC observations.
Choose and verify suitable OS clocks during the native spikes; do not assume all platforms expose identical guarantees.
Keep UTC times for journal ordering and recovery context, and civil time plus an IANA time zone for recurring schedules.

Persist a checkpoint at start, pause, resume, completion and lifecycle boundaries, with periodic bounded checkpoints while active.
It contains the session ID, phase, configured duration, accumulated elapsed time, running/paused state, UTC observation, alarm ID and recovery metadata.
Raw `Instant` values are never persisted or synchronized. Local active timer state belongs to the originating device.

On wake or foreground entry, reconcile the current checkpoint with a trusted elapsed-clock sample.
If the deadline passed, complete that period once and move to an awaiting-confirmation state. Do not synthesize additional focus/rest periods for time away.
Cap recorded elapsed at the planned duration. Persist the effective completion instant at the calculated deadline separately from `detected_at`, when the app notices completion. Time after the deadline is waiting time, not focus or rest. Map the deadline to a journal date only when the clock anchors establish it reliably; otherwise flag its date allocation as uncertain instead of using the wake date as a substitute.
On process restart, a verified same-boot elapsed source may permit reconstruction; the adapter must prove that its samples remain comparable.
After reboot, missing metadata or conflicting clocks, restore a recovery state and offer continuation or ending the interrupted period.
Wall-clock estimates may inform that recovery screen but must not silently become precise elapsed statistics.

System clock changes must not alter an uninterrupted monotonic interval. Time-zone changes affect future routine scheduling, not elapsed duration.
Recurring schedules create invitations, never automatic starts. Use the next valid local time in a daylight-saving gap and the first occurrence in an overlap.
Deduplicate occurrence IDs and avoid queuing missed sessions as work debt.
The domain uses injected clocks so these cases can be tested deterministically.

Native notification delivery is an alert, not proof that a period was completed or that the user accepted the next one.
Use stable session/alarm identifiers and transactional state transitions to make duplicate callbacks harmless.
When permissions or platform rules prevent a reliable background alert, show that capability honestly before the user relies on it.
All paths, including notification actions and recovery, require an explicit action before the next period starts.

Desktop window close and application Quit are different actions. Closing a window may continue the period only where a tested background process or OS-scheduled alert remains available; explain that behavior on the first close during a period. Otherwise offer to keep the window open or end the period. Explicit Quit checkpoints and ends a running period early, cancels its alerts, and preserves notes; ask which behavior the person intends when a period is active. Canceling Quit keeps it running. An OS termination or crash follows recovery rules rather than pretending the person explicitly ended a period. Do not assume every Linux environment supports a tray icon. Mobile backgrounding is not explicit Quit and uses the native alert/recovery policy.

## Mobile behavior and rest interruption

Android's `AlarmManager` can trigger work outside the application lifetime, including while the device is asleep.
Exact alarms require an appropriate permission path on newer Android versions; check permission at scheduling time and handle revocation.
Use native alarms for session boundaries, with a documented fallback when exact delivery is unavailable.
Avoid a continuously running service merely to decrement a counter.
[Android alarm guidance](https://developer.android.com/develop/background-work/services/alarms).

Android restricts background activity launches and fullscreen notification eligibility.
Rhythm must not promise it can always replace another app with a fullscreen rest screen.
Use a notice with an explicit open action when outside Rhythm; show the rest experience immediately when Rhythm is foregrounded.
[Fullscreen intent limits](https://source.android.com/docs/core/permissions/fsi-limits).

iOS local notifications can be scheduled while Rhythm runs and delivered by the system while it is not running.
They are the baseline for supported iOS versions; reconcile application state when the user next interacts.
[Apple local notification scheduling](https://developer.apple.com/library/archive/documentation/NetworkingInternet/Conceptual/RemoteNotificationsPG/SchedulingandHandlingLocalNotifications.html).

On iOS/iPadOS 26+, AlarmKit offers separately authorized prominent alarms that can break through Focus and silent mode.
Explore a schedule-only alarm as an optional stronger rest reminder. Do not enable this automatically.
Countdown configurations introduce system countdown UI and require the corresponding widget integration, which may conflict with Rhythm's quiet focus experience.
[AlarmKit introduction](https://developer.apple.com/videos/play/wwdc2025/230/), [scheduling sample](https://developer.apple.com/documentation/AlarmKit/scheduling-an-alarm-with-alarmkit).

Neither a shared GUI nor native alarms guarantee device-wide blocking. Rest enforcement means an optional stronger experience within supported capabilities.
Provide an always reachable emergency exit, preserve the journal and leave the next focus period awaiting confirmation.
Never simulate background entitlement needs with unrelated audio, location or accessibility services.

## Platform focus capabilities

Expose capability states such as available, permission-needed and unavailable; keep the default focus experience useful without OS integration.
Never overwrite a person's existing quiet-mode preferences or turn off a mode Rhythm did not establish.

Windows can report Focus support and activity through `FocusSessionManager`.
Current documentation also lists start/deactivate methods as Limited Access Features requiring an unlock token, so general availability must not be assumed.
[Windows FocusSessionManager](https://learn.microsoft.com/en-us/uwp/api/windows.ui.shell.focussessionmanager?view=winrt-26100).

On Android, investigate an optional user-authorized notification-policy integration against the chosen target SDK.
On Apple platforms, investigate supported Focus/App Intents participation and user-configured automation.
On Fedora, detect the desktop environment and verify notification/window behavior under Wayland and X11.
These are capability spikes, not committed promises to toggle global Do Not Disturb on every device.

## Internationalization and accessibility

Use stable message keys from the first prototype, with English, Simplified Chinese and Traditional Chinese resources.
Represent Chinese script distinctions as `zh-Hans` and `zh-Hant`; resolve system locale with a documented fallback and offer manual selection.
Localize prompts, error states, native notifications and export labels; retain original user text without translation or normalization that changes meaning.

Do not concatenate translated fragments. Use locale-aware dates, plural forms and input validation; keep machine record formats language-independent.
Test IME composition, mixed-language notes, font fallback and larger text on each target.
Flutter provides localization facilities including explicit Chinese script subtags; all candidates still require review of the warm product language.
[Flutter internationalization](https://docs.flutter.dev/ui/internationalization).

Use semantic controls, visible keyboard focus, descriptive labels and readable contrast in both appearances.
Support screen readers, text enlargement and reduced motion. Avoid announcing timer ticks or moving attention during focus.
Test the emergency exit with keyboard, touch and assistive technology; a calm screen must remain discoverable and operable.
Framework support is a starting point, not a substitute for device verification.
[Flutter accessibility](https://docs.flutter.dev/ui/accessibility).

## Build and distribution prerequisites

| Target | Tauri candidate prerequisites and validation |
| --- | --- |
| Windows 11 | Rust, Microsoft C++ Build Tools, WebView2 and frontend build tooling; verify an installed release build and its notifications. |
| macOS | Rust plus Xcode command-line tools for desktop; signing and distribution configuration; Intel/Apple Silicon support chosen explicitly. |
| Fedora/Linux | Rust, native build packages and WebKitGTK; verify current Fedora on Wayland, dependency packaging and notifications. |
| Android | Android SDK/NDK, Java tooling, Rust Android targets, device/emulator and signing configuration. |
| iOS | A Mac, full Xcode, Rust device/simulator targets, signing and physical-device access. Windows alone cannot validate this target. |

Tauri publishes platform prerequisites, including Fedora packages and the macOS-only iOS requirement.
Its current notification plugin requires Rust 1.90 or later and documents Windows differences between development and installed apps.
Pin a compatible toolchain only when implementation starts.
[Tauri prerequisites](https://v2.tauri.app/start/prerequisites/), [notification plugin](https://v2.tauri.app/plugin/notification/).

Flutter also requires Windows C++ tooling and Xcode for iOS, alongside Flutter and the Rust toolchain.
Slint's iOS workflow likewise requires macOS and Xcode. Each option needs platform-specific signing and release validation.
[Flutter Windows](https://docs.flutter.dev/platform-integration/windows/setup), [Flutter iOS](https://docs.flutter.dev/platform-integration/ios/setup), [Slint iOS](https://docs.slint.dev/latest/docs/slint/guide/platforms/mobile/ios/).

## Framework selection gates

| Spike | Exit criterion |
| --- | --- |
| Calm interaction flow | Begin focus without setup; optional note; rest invitation; optional reflection; explicit next-period action; emergency exit works at every rest stage. |
| Journal editing | All three languages pass IME, selection, multiline editing and draft recovery on desktop and phone; no lost or duplicated text. |
| Accessibility | Keyboard and relevant platform screen readers complete the main flow; enlarged text does not hide controls; reduced motion is respected. |
| Clock recovery | Deterministic tests cover pause, sleep, clock changes, duplicate alarms, process restart and reboot; uncertain recovery is visible and never starts another period. |
| Native delivery | Physical Android/iOS devices cover locked, backgrounded, terminated and permission-denied states; observed limitations are documented. |
| Desktop interruption | Installed Windows/macOS/Fedora builds cover notifications, fullscreen entry/exit, sleep/wake and supported display configurations. |
| Storage | The same Rust application API and SQLite design work on every target, with migration and interrupted-write recovery evidence. |
| Distribution | Reproducible installable builds exist for all five targets before claiming cross-platform support. |
| Resource use | Equivalent release builds are measured for startup, idle memory and battery behavior; no numerical performance claim precedes measurement. |

Choose the shell only after these results are recorded. If Tauri fails essential editing, accessibility or mobile requirements, repeat the failing slice in Flutter and Slint before accepting separate native shells.
