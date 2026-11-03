# Rhythm

A private, calm companion for focus and recovery, with a small space for personal thoughts.

The Rust foundation powers a **Tauri 2 Windows application with three screens: Init, Focus, and Break**. The same interface has an optional offline web shell for desktop and mobile browsers. Windows has the native background worker, notifications, sleep/wake reconciliation, and SQLite storage; the other native platforms remain development targets. See the [three-state design](docs/three-state-design.md) for current behavior and limits.

## Project status and setup

Rhythm is a working Windows preview, published under the [MIT license](LICENSE). The maintainer reports that Windows use and the Google Drive connection work after the proxy fix. macOS, then Linux, are the next native ports. Remaining device and release checks are tracked in the [roadmap](docs/roadmap.md); they do not block working on the source.

| Platform | Available now |
| --- | --- |
| Windows | Native desktop app, local journal, backup/restore, and optional Google Drive sync |
| macOS and Linux | Shared Rust core/tests and browser preview; native desktop shell is not implemented yet |
| Android and iOS | Browser preview; native apps are future work |

To try the interface with Node.js 22 or newer, run `node scripts/preview.mjs` and open <http://localhost:8767>. See [CONTRIBUTING.md](CONTRIBUTING.md) for fresh-checkout dependencies, Windows builds, tests, the source map, and the macOS/Linux porting plan. The source checkout does not require local journals or Google credentials; builds without a separately supplied desktop OAuth registration keep automatic sync unavailable.

## Current interface

Enter Focus directly from Init, then switch between Focus and Break. A discreet gear opens Settings over the current screen. The default rhythm is 25 minutes of focus, five minutes of short recovery, and a 30-minute long break after three completed short breaks. Skipping or shortening a short break does not advance that count. Existing saved cadence values are retained; Settings expresses the cadence as the number of short breaks.

The main screens use a state icon, a gentle sentence, and one primary transition. The opening Focus button and the exit dialog share the frosted-glass finish used by Focus, Break, Settings, and Notes. When a reminder is due, the switch button expands into a compact pair of choices in place. There is no reminder modal, overlay, or visible delay duration. Break offers optional ideas such as looking away or changing scenery. There are no period countdowns or separate Pause, Finish, Summary, and Review pages. Full screen is available through an explicit control.

At a Focus deadline, the Focus scene stays in place and the switch offers **Break** or **Stay focused**. At a Break deadline, the equivalent choices are **Focus** or **Keep resting**. In both directions, a small numberless circle fills over twelve seconds before accepting the opposite state by default. This interval runs only while the invitation is visible and the app is focused, pauses during interaction with its controls, and restarts after ordinary backgrounding. Text composition, active operations, and open dialogs defer the default transition.

Choosing to stay postpones the reminder by nine minutes by default. Advanced Settings contains a manual delay from 1–60 minutes, an independent learning switch, and a reset control. Postponement keeps the person in their current state without adding a completed Focus or Break period or advancing the long-break cadence. Delay preferences are stored locally; the Windows app clears outstanding invitations, including postponed ones, on wake or any desktop restart.

On Windows, sleep, hibernation, shutdown, and reopening interrupt an existing period and return to an **untimed Break**. Only time already verified before the interruption is retained; downtime does not complete Focus or Break, advance the cadence, or train learning. Old notifications and queued choices are cleared, with no catch-up invitation. Focus begins again only through an explicit choice. Pending note text is flushed against its original period, which is archived with the interruption. Windows shutdown allows a bounded save opportunity; abrupt termination can still lose text that has not reached storage. These native lifecycle changes have automated coverage but have not yet been rechecked through physical sleep, hibernate, or reboot.

In the background, Rhythm leaves the foreground app alone. Windows notifications offer **Take a break** / **Stay focused**, or **Focus** / **Keep resting**, without showing a duration. Accepting explicitly brings the existing app window forward; delaying keeps it in the background. Neither action forces full screen. Notification actions and delivery require the desktop process to remain running, and stale or duplicate responses cannot replace a newer period. Windows controls notification appearance, permissions, and Do Not Disturb. No duplicate OS reminder is sent while Rhythm is active.

Each Focus and Break period has its own optional note. A period's note is archived when that period ends, and the next period starts with an empty editor. **Past notes** shows five recent entries inside the small note panel. **Settings → Read all notes** opens a separate archive with word search across the whole history, date and time-of-day filters, note types, and newest/oldest sorting. Windows includes earlier journal drafts, preserved variants, and saved reflection versions. New notes retain their UTC instant and original time zone/offset across sync; the archive can display and filter in current-device or original time. Legacy origins remain explicitly unknown. The Past notes control keeps its history icon and highlights it while selected. The note icon remains fixed in the bottom-right corner while the panel opens upward and to the left. Closing the panel preserves the current text, and saved notes survive reopening the app. Init has no note field, and ordinary saves show no status caption. Desktop notes use SQLite; browser notes use separate local storage.

Focus and Break have independent backgrounds in Settings. Choose Forest, Coast, Dusk, or an image from your device; image choices take effect when Settings is saved. The photographic defaults are bundled locally, and personal images remain in local IndexedDB storage without an upload. State changes use a gentle crossfade, with motion disabled when reduced motion is requested.

## Optional learning and private copies

Period learning and reminder-delay learning are independent and both start off. Period learning uses deliberate transitions and actual short recovery durations observed during the current app lifetime. It groups these choices by activity context and local time of day. At least five new observations in a group are needed for each adjustment of at most one minute. Settings shows the next suggested lengths and observation count, allows minimum and maximum lengths, and can forget learned patterns. Changing base durations resets duration learning; long breaks keep their own configured length.

Reminder-delay learning uses repeated explicit postponements and visible response times, with separate summaries for Focus and Break. Five new explicit responses permit a change of at most one minute. Repeated postponements or recurring late responses after a postponement can lengthen the delay; prompt acceptance gradually returns a learned extension toward the manual value. Extensions are capped at the smaller of nine minutes or half the manual delay, and the total never exceeds 60 minutes. Manual delay edits clear learned offsets, disabling learning immediately uses the exact manual value, and resetting keeps the manual preference. Context-specific reminder timing is a future enhancement; this first version pools observations by personal state.

Ignored reminders and automatic defaults never train reminder delays. They supply no evidence of continued focus or reduced need for rest. Learning runs quietly and stores only bounded summaries in localStorage, including in the desktop webview. Complete Windows backups include these summaries and their preferences alongside the database. Restarted or externally controlled periods are excluded from period learning when the interface cannot reliably identify the observed period. This is an initial behavioral model, not a physiological assessment.

**Create complete backup** in Windows Settings saves a complete, standalone `.sqlite3` file: every note and saved version, drafts, sessions, routines, timer settings, language, appearance, background selections and custom images, and both learning models with their preferences. The database has no backup note-count limit. Restoring first creates a complete safety backup, then replaces the journal and applies the saved preferences. A durable pending restore is retried on reopening if preference storage could not finish. Older database-only backups still restore; they retain the current appearance and learning preferences. These backups are local and unencrypted. See [backup and export](docs/backup-and-export.md).

Windows **Automatic sync** is implemented ahead of other native platforms, with broader release validation still pending. It connects directly to Google Drive and exchanges encrypted logical records: archived period notes, saved reflection versions, and ended session history. A separate native worker checks for changes while Rhythm is open or minimized; the live SQLite database is never uploaded. Active notes, drafts, timers, routines, settings, learning, and backgrounds remain device-local. Settings provides connection, check-now, reconnect, and disconnect controls without adding a screen. Google authorization and the encryption key are protected by Windows DPAPI outside portable backups. Ordinary users connect their Google account and use their sync passphrase; they do not move files or manage cloud folders. See [automatic sync](docs/automatic-sync.md) for scope, maintainer setup, and verification limits.

The maintainer's configured Windows build includes a Google desktop registration, and the maintainer confirmed the connection works after the Windows proxy fix. Public source excludes that registration; developers supply their own file as described in the sync guide. Real exchange between two installations, offline/revoked-access recovery, and physical-device checks remain release validation tasks. Automated isolated-profile tests do not establish those live results.

Advanced Settings also retains **manual encrypted file exchange** for archived period notes. Export reads all history pages rather than stopping at the latest 100. Each file supports up to 1,000 notes within its size limits; a larger archive or a change during export produces an error instead of silently exporting a partial set. Choose a passphrase of at least 12 characters and keep the file with any provider or storage location you prefer. Import preserves distinct conflicting versions and leaves the current note unchanged. Active notes, earlier journal drafts/reflection versions, settings, learned patterns, backgrounds, and physiological data are outside this manual note-exchange format. Complete Windows backups and complete Markdown export provide broader coverage. Browser history paging and encrypted file exchange are supported; automatic Google Drive sync and complete SQLite backup/restore belong to the Windows desktop app.

Wearable ingestion, physiological baselines, heart-rate/HRV/activity/EDA interpretation, EEG, eye tracking, and more advanced recovery models are not implemented. No sensor information is collected by this version.

## Open Rhythm

Run the current interface locally from the repository root:

```powershell
node scripts/preview.mjs
```

Open [the local preview](http://localhost:8767). It serves the same interface as the desktop app and needs no package installation. Stop the server with Ctrl+C. Browser data stays separate from the desktop database.

On a supported secure origin, the web shell caches the bundled interface for later offline use and offers browser-managed installation. Localhost supports development; the preview server binds only to this computer. The cache contains application assets, never notes, imported files, personal images, or sync traffic. Updates wait for old app windows to close instead of reloading an open editor. Actual installation and offline behavior still need acceptance on each target browser/device.

Browser notifications are opt-in and require the app to remain open. A suspended or closed browser cannot reliably run scheduled reminders; installing the web shell does not remove that limitation. Native routines and reliable background timer execution currently belong to the Windows app.

For the Windows executable and installer, retain the existing build workflow:

```powershell
# Once, from apps/desktop:
npm ci --ignore-scripts

# From the repository root:
.\scripts\cargo.ps1 run -p rhythm-desktop --locked
.\scripts\desktop.ps1 build --ci --bundles nsis '--' --locked
```

The [Windows prototype guide](docs/windows-prototype.md) retains the build requirements, packaging instructions, storage location, and earlier platform checks. Its old screen walkthrough is superseded by the current three-state design.

## Run and test

With Rust installed, use:

```text
cargo test --workspace --locked
cargo run -p rhythm-cli --locked -- --database rhythm.sqlite3
```

On this Windows workspace, a project-local Rust toolchain is available through the helper:

```powershell
.\scripts\cargo.ps1 test --workspace --locked
.\scripts\cargo.ps1 run -p rhythm-cli --locked '--' --database .\rhythm.sqlite3
```

Quote `'--'` when passing it through the PowerShell script. Rust 1.99.1 and the dependency lockfile are pinned. The local toolchain is under the ignored `.tools` directory; the system PATH was not changed. SQLite is bundled, so no separate database installation is required.

The CLI retains `focus`, `pause`, `resume`, `poll`, `stop`, `note A little space helped.`, `journal`, and `help` for compatibility and development. These legacy commands do not add screens to the current interface. New defaults are 25/5/30 minutes with three completed short breaks before the next long break. The serialized setting retains the legacy name `focus_before_long_rest`; its new default is `4`, and the UI displays `value − 1` short breaks. Existing persisted values are not silently changed. `focus 10` starts a one-time 10-minute period. The core/CLI do not start the next timer automatically; the active graphical interface supplies the inline invitation and its default choice in either direction. The isolated interactive CLI retains `recover continue` and `recover end`; the Windows desktop instead closes interrupted periods and waits in Break for a fresh Focus.

This is a development harness: use `status` or `poll` to observe elapsed deadlines. It does not continuously run a UI or schedule operating-system alerts. Notes in the CLI are single-line; the underlying journal supports multiline English and Chinese text. Full usage, architecture boundaries, validation results, and remaining platform work are in [the Rust foundation guide](docs/rust-foundation.md).

Run the interface, persistence, learning, encrypted exchange, automatic-sync controls, and offline-shell tests with:

```powershell
npm test --prefix apps/desktop
```

Earlier local Windows verification includes a **real sleep/wake cycle and a visibly delivered native notification**; the [Windows check record](docs/windows-checks-2026-11-01.md) records the former sleep-inclusive behavior and the host's delayed wake. A later [installed-app acceptance pass](docs/windows-acceptance-2026-11-02.md) verified all four fresh notification buttons and preserved notes/settings. Both records predate the new interruption policy: their postponed-reminder restart and recovery-state results are historical, and they do not validate physical sleep/hibernate/shutdown with the current behavior. CI runs the full JavaScript test command plus Windows/Linux/macOS core checks. Other native platforms, remote runs, accessibility/IME acceptance, and physical-device lifecycle checks remain outstanding.

## Review the design

| Document | Contents |
| --- | --- |
| [Current three-state design](docs/three-state-design.md) | Three states, reminders, optional learning, automatic sync controls, private file exchange, offline shell, and verification limits |
| [Windows prototype](docs/windows-prototype.md) | Build, native alerts, lifecycle behavior, packaging, and earlier device verification; its screen walkthrough is historical |
| [Backup and export](docs/backup-and-export.md) | Local snapshots, safe restore, and complete Markdown journal export |
| [Automatic sync](docs/automatic-sync.md) | Windows Google Drive integration, encrypted record exchange, configured desktop registration, and outstanding live release validation |
| [Daily routines](docs/recurring-invitations.md) | Multiple weekday time ranges, focus/rest cycles, time zones, and reminder delivery rules |
| [AI-friendly CLI](docs/cli-control.md) | Structured commands that control the running desktop app through local IPC |
| [Rust foundation](docs/rust-foundation.md) | Implemented modules, run instructions, tests, recovery and persistence guarantees, and current limits |
| [Earlier product design](docs/product-design.md) | Historical requirements and concepts; screen responsibilities and navigation are superseded by the three-state design |
| [Architecture proposal](docs/architecture.md) | Tauri 2, Flutter plus Rust, Slint, native alternatives, platform constraints, module boundaries, and selection gates |
| [Storage and synchronization](docs/storage-and-sync.md) | Broader architecture proposal for durable drafts, provider synchronization, revision conflicts, encryption, backups, and exports; current automatic-sync scope is documented separately |
| [Development roadmap](docs/roadmap.md) | Current implementation, Windows synchronization and release checks, deferred native platforms, journal tools, and optional reviews |
| [Earlier prototype verification](docs/prototype-review.md) | Checks performed and limits of the superseded design demonstration |
| [Original request](docs/original-request.txt) | Unmodified earlier product specification, retained as project history |

The Windows app uses an independent Rust core and a Tauri 2 shell. Native tests of alerts, lifecycle handling, Chinese journal input, accessibility, and packaging remain necessary for the other target platforms; revisit shell technology if a target exposes a material limitation. Historical architecture documents link primary sources reviewed on November 1, 2026.

## Earlier concepts — superseded

The [standalone concept prototype](prototypes/rhythm-preview.html) and its [editable fragment](prototypes/rhythm-concepts.html) are retained as historical design artifacts. Their Quiet room/Letters picker, Finish simulation, reflection pages, and journal navigation are superseded. They do not represent the current application's screens or workflows.

The older preview remains self-contained. To inspect it for historical comparison only:

```powershell
python -m http.server 8766 --bind 127.0.0.1 --directory prototypes
```

Then open [the historical preview](http://127.0.0.1:8766/rhythm-preview.html). Stop the local server when finished. Use the current preview on port 8767 for current interface testing.

All letters in the historical concept are examples. Its new words and edits disappear on reload; only its language and appearance preferences may be remembered. Do not use that old concept as a journal for words you need to keep. The current preview on port 8767 persists current and archived period notes.

The Windows implementation is in `apps/desktop`, with platform adapters and the desktop service in `crates`. Advanced Settings retains routine configuration and data controls; the JSON CLI remains available for controlling the running app and is bundled by `scripts/desktop.ps1 build`. Use `rhythm-cli --json schema` for discovery and `rhythm-cli --json status` for live state. Accessibility and IME acceptance, distribution, and the other native target platforms remain work on the current roadmap. Physical power-transition checks remain deferred device validation.
