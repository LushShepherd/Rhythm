# Rhythm development roadmap

Current implementation and next steps · November 2, 2026

Rhythm now has a working Windows desktop app. The [original request](original-request.txt), [product proposal](product-design.md), and [architecture exploration](architecture.md) remain historical design references. Where later decisions differ, the current [three-state design](three-state-design.md), [README](../README.md), and implementation records define the delivered behavior. The original prototype phase is complete; its unimplemented ideas are not current features.

## Implemented foundation and Windows experience

| Area | Current implementation |
| --- | --- |
| Interface | Init, Focus, and Break; optional notes; explicit fullscreen; English, Simplified Chinese, and Traditional Chinese; appearance and independent Focus/Break backgrounds; backup replacement confirmation uses the same glass material as Settings |
| Rhythm | 25-minute Focus, five-minute short Break, and 30-minute long Break defaults; long Break follows three completed short Breaks by default; existing saved settings are retained |
| Invitations | Compact in-app choices with a twelve-second visible default; independent postponement in either direction; native Windows notification buttons while the app remains running |
| Interruption | Sleep, hibernate, shutdown, and reopening end an active period at its last committed awake time, preserve notes, and return to untimed Break; explicit Focus is required afterward, with no catch-up reminder |
| Durable data | SQLite, checksummed migrations, atomic session/note transitions, autosave, preserved journal revisions and variants, and complete Markdown export |
| Complete backup | One standalone SQLite backup containing every note, history, drafts, sessions, routines, timer settings, language, appearance, custom backgrounds, and learned preferences; complete safety backup before restore; legacy database-only compatibility and durable preference-restore retry |
| Older notes | Five recent entries in the note dock; a separate paged archive searches all history by words, date range, time of day, type and source, sorted by creation time; current/original time-zone display and preserved source offsets |
| Routines and control | Multiple weekday time windows, time zones and DST handling, per-window focus preferences, durable delivery deduplication, and a local CLI controlling the same Windows worker |
| Optional learning | Independent local models for period length and reminder delay; explicit controls, bounds, reset, and no inference from ignored reminders |
| Manual exchange | Encrypted archived-period-note files, collecting all pages up to the explicit 1,000-note/size limits without silent truncation; preservation of distinct conflicting text |
| Automatic sync integration | Direct Google Drive connection, encrypted immutable record exchange, Windows Settings controls, and a configured desktop OAuth registration; the owner reports successful use after the Windows proxy fix; broader live acceptance remains outstanding |
| Browser shell | Separate browser storage, responsive three-state interface, older-note paging, encrypted note exchange, and an optional offline shell; it does not supply native background or power handling |

The native shell remains Windows-only. On November 2, the owner reported that the Windows app and Google connection worked after the proxy fix. The working Windows preview is sufficient to prepare public source and begin the next platform; the release checks below remain tracked work, not additional feature prerequisites. AI reviews, physiological interpretation, and wearable ingestion are not implemented. The complete backup format is a local unencrypted file; encrypted manual note exchange is a separate, narrower operation.

## Next milestone: public source, macOS, then Linux

Automatic sync implementation was completed ahead of other native platform work. The next step is to organize the repository for public source publication and continue native development on macOS, followed by Linux. Keep private profiles, credentials, backups, and local tools out of the published source, and document build requirements and current platform limits. No remaining Windows feature work blocks that transition.

The first connection is **direct Google Drive**, using its application data storage and browser authorization with PKCE. Ordinary users connect their Google account rather than manage folders or move encrypted files. A native background worker exchanges encrypted immutable logical records while Rhythm is open or minimized. The initial scope is archived period-note variants, saved reflections and earlier revisions, and ended session history. Active notes, unfinished drafts, timers, routine execution, preferences, learning, and backgrounds remain device-local. Each device keeps its own local SQLite database.

Settings contains a passphrase-based connection, current connection status, last check with Drive, manual retry, reconnect, and disconnect. Credentials and the derived encryption key use Windows DPAPI and stay outside portable backups. Preserve the existing manual encrypted note exchange and complete backup/restore workflows. See [automatic sync](automatic-sync.md) for the narrower current contract and maintainer setup.

The supplied Google OAuth desktop registration is validated and bundled in a successfully built Windows installer. The owner's post-fix confirmation records a working Windows/Google connection; it does not establish a measured two-profile or two-device exchange. Remaining release evidence includes convergence through real Drive, interrupted upload and expired-access recovery, an explicit key/recovery review, and verification that local note saving remains usable during network failures. The next platform provides an opportunity for that cross-device test. Report automated checks, owner feedback, and observed device checks separately.

Additional providers, portable preferences, routine definitions, and mobile synchronization are later extensions. Native platform work resumes when suitable build and test access is available.

## Windows usability and release readiness

Retain these checks before claiming a broadly validated Windows release: Chinese IME composition, keyboard-only navigation, screen readers, enlarged text, readable contrast, reduced motion, and denied notification delivery. Also retain clean installation, supported upgrades, a data-preserving uninstall/reinstall policy, and signed distribution. These checks can proceed alongside macOS/Linux development and do not block publishing source. Record startup and idle measurements on named hardware before making performance claims. The [glass restore confirmation](glass-restore-verification-2026-11-02.md) is delivered interface polish.

Keep the interaction focused on entering Focus, resting, and writing. Read-only archive search, date and time-of-day filters, and chronological paging are implemented; editing older period notes remains optional later work. Complete backup restoration into a fresh profile, including custom images, preferences, legacy writing, and a recoverable safety copy, has passed [installed verification](complete-backup-verification-2026-11-02.md). Retain the failure/retry regression coverage as backup formats evolve.

Exit evidence is a usable Windows release with reproducible recovery and accessibility results, plus a clear installation and data-retention policy. Physical sleep/hibernate/reboot checks remain deferred device validation; they are not the next feature milestone and do not require interrupting the user's current PC work.

## Native macOS, then Linux

Implement and validate the macOS shell over the shared Rust service, including notifications, explicit acceptance, note saving, awake-time interruption, fullscreen behavior, and packaging. Preserve the same return-to-Break policy on waking or reopening. A Mac build and device results are required before claiming macOS support.

Follow with Fedora/Linux. Evaluate native background behavior, notifications, input methods, accessibility, secure credential storage, and lifecycle boundaries on each target. The existing browser shell and CI configuration do not establish native acceptance. Android/iOS feasibility remains later work; iOS requires appropriate Apple build and physical-device access. Revisit shell technology only if a platform exposes a material limitation.

## Journal organization and descriptive summaries

Extend the existing complete Markdown export with date ranges, daily/weekly/monthly layouts, and Obsidian-friendly metadata. Add optional SQL-derived summaries away from the main Focus screen. Define time-zone boundaries, interrupted periods, and overlapping devices explicitly. Future-self messages have stored metadata in the legacy journal; delivery and reveal-date interface work remains separate.

Exit evidence: Unicode and Markdown remain intact, all preserved versions remain recoverable, exported totals agree with deterministic fixtures, and a person can use their journal without Rhythm.

## Optional reviews and more advanced learning

Add explicit, user-requested reflection reviews through configurable cloud APIs or local Ollama. Let the person select entries and dates, inspect the exact outgoing content and destination, and keep the original writing unchanged. Supply SQL-derived statistics as facts; AI output remains attributed and editable.

Context-specific reminder learning, more sophisticated recovery models, and optional wearable integration are later work. These features need clear consent, local data handling, and evidence appropriate to their claims. No physiological or sensor interpretation is part of the present app.

## Evidence and documentation

Use deterministic domain tests for timing and state invariants, SQLite integration tests for commits and restore, and focused UI tests for editor/choice races and complete preference recovery. Keep device checks separate from simulated tests and describe the exact tested build. Current records include [glass restore confirmation](glass-restore-verification-2026-11-02.md), [complete backup and older-note verification](complete-backup-verification-2026-11-02.md), [Windows interruption verification](windows-interruption-2026-11-02.md), [backup/export behavior](backup-and-export.md), and the historical [notification acceptance run](windows-acceptance-2026-11-02.md).

Update this roadmap when implementation or user decisions change. Historical documents may retain earlier counts and interaction policies; they should not be treated as an unfinished checklist for behavior already replaced.
