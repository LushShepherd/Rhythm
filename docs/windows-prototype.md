# Quiet room — Windows prototype

Phase 1 implementation · November 1, 2026; lifecycle policy updated November 2, 2026

The Tauri 2 Windows shell now uses the same Rust timer, SQLite store, and reflection journal as the command-line harness. Tauri remains the leading candidate; this prototype does not settle the macOS, Linux, Android, or iOS feasibility gates.

This document retains the early prototype walkthrough and validation record. For the current interface, use the [three-state design](three-state-design.md). The owner reports successful Windows use and Google connection after the November 2 proxy fix; public source preparation and native macOS, then Linux work are next. The remaining Windows release checks below are not prerequisites for those steps.

## Build and run

Requirements: Windows, Microsoft C++ Build Tools, WebView2, Node.js/npm, and the pinned Rust toolchain. This workspace's helper scripts use its existing local Rust installation without changing the system PATH.

```powershell
# Once, from apps/desktop:
npm ci --ignore-scripts

# From the repository root:
.\scripts\cargo.ps1 test --workspace --locked
.\scripts\cargo.ps1 run -p rhythm-desktop --locked

# Create a per-user installer with embedded frontend assets:
.\scripts\desktop.ps1 build --ci --bundles nsis '--' --locked
```

The NSIS installer is written under `target/release/bundle/nsis/`. Install the app to give Windows notifications the Rhythm name and icon. A development executable can use the PowerShell notification identity. Packages are unsigned development builds; code signing and distribution are still release work. The normal executable uses no local HTTP server or frontend development dependency.

The web interface consists of local HTML, CSS, and JavaScript modules in `apps/desktop/ui`. Its only native capability is receiving events; a narrow application command list exposes timer and journal actions. No web filesystem, SQL, shell, remote navigation, or network plugin is enabled. Journal strings are inserted as text, never interpreted as HTML.

## Historical prototype walkthrough

- **Now:** optionally write an intention, begin focus, pause/resume, or end a period. The active focus screen has no ticking countdown. Fullscreen has a visible exit and Escape route.
- **Rest:** a completed focus invites a separate, explicit rest start. Rest offers an emergency exit. A completed rest waits for the next explicit focus start.
- **Letters:** create, resume autosaved drafts, save, and edit reflections. Text, line breaks, and Chinese characters are preserved in SQLite; editing adds a revision. The view shows the newest 100 records and all drafts.
- **Advanced:** change future period lengths and cycle cadence, choose English/Simplified Chinese/Traditional Chinese and appearance, inspect platform capability, or send a test notification.
- **Backup and export:** Advanced also offers native file pickers for a consistent SQLite backup, restore with an automatic safety copy, and a complete Markdown journal export. See [backup and export](backup-and-export.md) for contents and recovery instructions.
- **Daily routines:** Advanced supports several custom start/end ranges on selected weekdays, time zones, focus presets for continuing focus/rest cycles, and next-occurrence previews. Each period starts explicitly; a running period finishes normally after its routine range ends. See [daily routines](recurring-invitations.md).
- **AI/CLI control:** the installer also includes `rhythm-cli.exe`. Its JSON commands control the running app through a current-user-only Windows pipe. See [CLI control](cli-control.md).
- **Closing:** the window asks before quitting. Drafts save first, and explicit quit ends an active period. Minimize the window to keep the timer running. There is no tray or timer service after the process exits.

The SQLite database is `rhythm.sqlite3` in Tauri's application-local data directory, normally `%LOCALAPPDATA%\app.rhythm.quietroom\`. Appearance/language preferences are stored in the local WebView profile. A process environment override `RHYTHM_DATA_DIR` selects an isolated development database; web content cannot change that path. Use a separate directory when testing or comparing builds. Never open the CLI against a database being actively used by the desktop app.

## Timer and lifecycle contract

One named Rust worker owns the application service and SQLite connection. The WebView sends user actions and receives committed views; it does not calculate or advance the timer. The worker polls at most about once per second while running and reconciles native power events and window activation. Minimizing the window or JavaScript timer throttling does not stop it.

`WindowsClock` now uses `QueryUnbiasedInterruptTimePrecise` for awake elapsed time. Native suspend/resume notifications latch an interruption generation, and paired inclusive/awake clock samples detect sleep when callbacks are late or missing. The desktop worker handles the interruption before advancing or changing a timer. It ends active or paused periods at their last committed elapsed boundary, archives their notes, and returns to an untimed Break. Sleep and hibernation do not complete a period or advance the cadence. Native power callbacks do no database work and do not wait for application code.

Every process uses a fresh random clock epoch. Startup closes any prior active or recovery-required period at its verified elapsed time, without offering Resume or inferring activity while the app was closed. All immediate and postponed period reminders and old native actions are cleared on desktop restart or wake. Missed routine notifications are suppressed; future windows remain available. Focus starts again only through an explicit choice. UTC changes affect date attribution certainty, not measured elapsed time. Explicit normal quit also ends the active period.

The native Windows session-ending handler requests note saving and interruption before exit. It allows up to two seconds for the editor and worker to acknowledge durable saves, pumping UI messages during that bounded wait. It does not indefinitely block Windows shutdown. Cancelling shutdown leaves Rhythm in Break and permits a later explicit Focus. This is implemented behavior with automated coverage; physical sleep, hibernate, shutdown, and reboot have not been repeated for this policy.

Completions are published only after the checkpoint and session transaction commits. The desktop sends a native Windows notification for a newly completed period, with fixed translated copy and no intention or journal text. It reads native notification settings, reports synchronous send errors, and observes later failure callbacks. There are no scheduled future notifications to become stale after pause or cancellation. Delivery is best effort: Windows notification settings, Do not disturb, and sleep can suppress/delay a banner. A successful test confirms WinRT submission, not visibility to the person. The committed UI state remains the authority. A crash between commit and notification can miss a banner; reopening cannot create duplicate session history.

Ordinary autosave is debounced and serialized, skips active IME composition, and flushes on normal navigation, file actions, and quit. Lifecycle preparation additionally snapshots text already exposed in the textarea and saves it against the original period ID, without ending composition. If that period has already archived, the late save still updates it; a later composition end can refine the same note. The CLI's `--json quit` command waits for the editor to save before closing. Save failure preserves in-memory text for retry, but forced termination can still lose uncommitted text, and text still owned by the IME is outside the textarea snapshot. The journal remains plaintext local SQLite. Manual encrypted archive exchange, backup/restore, Markdown export, recurring weekday invitations, and local CLI control are available. Direct encrypted Google Drive synchronization is also implemented; see [automatic sync](automatic-sync.md) for its scope and remaining live validation.

## Validation

The subsequent backup/export implementation passes 109 Rust tests and six frontend tests,
formatting, and Clippy with warnings denied. See [backup and export](backup-and-export.md)
for its additional recovery coverage. The earlier prototype checks below describe the
baseline before that addition.

Automated coverage includes real SQLite reopen/edit tests, invalid/stale writes, deterministic sleep before/after a deadline, duplicate wake, paused sleep, wall-clock changes, fresh-process recovery, native power registration/unregistration, and a narrow IPC request allowlist. No automated test changes the machine's power state.

Local verification completed on this Windows workspace:

- All 92 Rust tests and six Node frontend tests passed, including four native notification regression tests. Workspace formatting and Clippy for all targets with warnings denied also passed.
- Tauri 2.12.5 with a direct Windows notification adapter built an optimized executable and 2.75 MiB NSIS installer. A per-user test installation completed successfully.
- Native window inspection verified Quiet room, focus without a countdown, fullscreen/Escape, explicit rest start, and multilingual reflection text after reopening.
- A one-minute focus completed while minimized. Read-only inspection of the isolated SQLite database confirmed one completed session with exactly 60,000 ms and Ready/short-rest state. A second launch restored the existing window.
- Forced termination during rest reopened in RecoveryRequired and allowed an explicit early end. Saved English/Simplified Chinese/Traditional Chinese reflection text and line breaks survived.
- Editing that reflection and choosing Save and quit exited successfully. A read-only database check confirmed the edited draft persisted and history contained one completed focus and one early-ended rest. The app was then reopened against its separate normal data folder with 25/5/30/3 defaults.
- Follow-up hardware testing verified a visible native Rhythm banner with Do not disturb off and a real Windows sleep/wake cycle. Running/paused accounting, overdue completion deduplication, SQLite persistence, and the installed app's post-wake screen passed. The earlier plugin success message had discarded native errors; that defect is fixed.

QA uses `.tools/desktop-smoke-data/` and the local installation in `.tools/installed-rhythm/`; these are ignored build/test artifacts. The main app's normal data folder remains separate from the QA journal.

The detailed [Windows check record](windows-checks-2026-11-01.md) contains historical measurements under the former sleep-inclusive policy. The [installed acceptance record](windows-acceptance-2026-11-02.md) verifies native buttons and preserved notes/settings before the latest lifecycle change. Neither establishes physical-device acceptance of the new interruption policy. Before claiming a broadly validated Windows release, repeat sleep/hibernate/shutdown/reboot with the current build and check other hardware, Chinese IME composition, screen-reader/large-text behavior, blocked notification delivery, and signed installation/uninstallation. These are tracked release checks, not blockers to public source publication or native macOS/Linux development.

## Primary references

- [Tauri notification plugin](https://v2.tauri.app/plugin/notification/) — Rust notification API and installed Windows app identity.
- [Tauri commands](https://v2.tauri.app/develop/calling-rust/) and [configuration](https://v2.tauri.app/reference/config/) — the native bridge, embedded assets, and capabilities.
- [Windows interrupt time](https://learn.microsoft.com/en-us/windows/win32/sysinfo/interrupt-time) and [GetTickCount64](https://learn.microsoft.com/en-us/windows/win32/api/sysinfoapi/nf-sysinfoapi-gettickcount64) — sleep-inclusive elapsed readings and system tick resolution.
- [PowerRegisterSuspendResumeNotification](https://learn.microsoft.com/en-us/windows/win32/api/powerbase/nf-powerbase-powerregistersuspendresumenotification) — native lifecycle hints.
