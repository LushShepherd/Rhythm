# Windows desktop interruption behavior

Implemented, rebuilt, and installed November 2, 2026. This supersedes the restart and sleep policy described in the [earlier acceptance run](windows-acceptance-2026-11-02.md).

## Behavior

- Sleep, hibernate, and Windows session ending interrupt an active period. Its saved note is archived with the period; pending editor text is flushed to that same note, including a late save after archiving.
- Only the last committed awake time is retained. The interruption cannot complete Focus or Break, advance completion counters, or credit suspended/closed time. Conservatively, an uncommitted fraction of the last awake heartbeat can be omitted.
- Waking or reopening presents untimed **Break**, waiting for an explicit **Focus** action. Reopening also retires due or postponed reminders from the previous process.
- Missed routine invitations and old notification actions do not cause catch-up transitions. Future routine windows and reminders from a newly started period still work.
- Repeated or delayed resume signals cannot interrupt a fresh Focus. The worker uses the clock's interruption generation as its authority, and clears missed invitations separately on resume.

The Windows clock excludes suspended time and detects a missed suspend callback from paired native counters. The shutdown handler allows the editor and database up to two seconds to finish saving while keeping Windows messages moving. Cancellation leaves Rhythm in Break. Abrupt power loss or termination before a save finishes can retain only previously committed text; uncommitted IME candidate text is outside that guarantee.

## Verification

- **211 Rust workspace tests passed**, including interrupted commits and retry, awake-only clock/native callback handling, note archiving and late saves, reminder retirement, routine suppression, and shutdown coordination.
- **101 JavaScript tests passed**, including stale choices, late reminder events, composing editor text, and save failures. CI now runs the complete interface suite.
- Formatting and all-targets Clippy with warnings treated as errors passed. The Windows release and NSIS installer were rebuilt successfully.
- **18 installed-app assertions passed** in separate database and WebView profiles. A forced close during Focus preserved a multilingual note and exactly **1,964 ms** of committed awake time. Reopening recorded one ended-early session, archived the unchanged note, and waited in Break. Explicit Focus worked; its subsequent completed reminder was cleared on reopening without a catch-up period.
- The recovered installed interface was inspected and displayed **Break** with an **Enter Focus** control.

No physical sleep, hibernate, shutdown, or reboot was performed. Native callback simulations, deterministic recovery tests, and isolated installed-process restarts supply the evidence for this pass. Current-policy physical-device and native macOS acceptance are not claimed; the native shell remains Windows-only.

## Existing data and installation

The previous executables and a cold copy of the original profile were retained before updating the existing installation. Before reopening the normal app, its **15 SQLite tables / 89 rows** and schema matched the backup, and all **14 inventoried WebView preference-storage files** matched byte-for-byte. These comparisons passed both after installation and after isolated QA.

After reopening the normal app, a separate read-only check confirmed database integrity, all **4 existing notes** including their metadata, all **19 existing sessions**, and timer settings were preserved. Rhythm was left open in Break with no pending reminder or invitation. Windows Do not disturb was not changed.

Local evidence and backups are in `.tools/desktop-interruption-2026-11-02/`. The directory contains private profile material and is ignored workspace data. Reports contain note hashes rather than note text. Cold file equality describes the pre-reopening measurement; later browser metadata can change during ordinary use.
