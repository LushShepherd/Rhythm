# Rhythm — current three-state interface

Implementation notes · November 2, 2026

This document describes the current interface in `apps/desktop/ui`. It supersedes the screen and navigation proposals in `product-design.md`, the screen walkthrough in `windows-prototype.md`, and the older browser concepts in `prototypes/`. Earlier platform measurements and storage documentation remain useful evidence within their stated scope.

## Three screens

`Init → Focus ⇄ Break`

| Screen | Main action | Personal note |
| --- | --- | --- |
| Init | Enter Focus with the default rhythm | No input or note control |
| Focus | Switch directly to Break | A separate note for this period; closed by default |
| Break | Switch directly to Focus | A separate note for this period, using the same anchored icon |

Rhythm describes a person's current state. It has no Pause, Resume, Finish, Letters, review, or summary workflow in the interface. The gear opens a Settings dialog over the existing screen; it is not a fourth screen or navigation destination. Future reflections should fit inside Focus and Break.

The state icon sits beside its name, with one short sentence that supports the current state. Photographic scenery, subdued glass surfaces, restrained typography, and short control labels keep the interface quiet. The opening Focus button uses the same glass material as the Focus and Break transition buttons; the exit dialog shares the glass surface used by Settings and Notes. There is no period countdown or ticking clock. Period lengths and learned suggestions remain visible in Settings. When a reminder is due, the existing switch button expands in place into two compact choices, with a small numberless ring indicating the default-choice interval. It does not open a modal, add an overlay, or display the postponement duration. There is no looping decorative animation.

Break offers optional recovery ideas: stand up and change the view, look away from the screen, have some water, or let breathing settle naturally. Tapping the suggestion chooses another idea. Full screen is available through an explicit control and can be exited; a deadline never enters full screen or locks the person out of another application.

## Direct transitions and timing

The default settings are 25 minutes of Focus, five minutes for a short Break, 30 minutes for a long Break, and a long Break after three completed short Breaks. The normal sequence is Focus → short Break, repeated three times, then Focus → long Break. Early or skipped short Breaks do not advance this count. A long Break remains due until completed.

For compatibility, the stored setting retains the name `focus_before_long_rest`; its new default is `4`, and the number of short Breaks is this value minus one. Settings displays the short-break count directly. Existing saved values are retained rather than overwritten. The new completed-short-break counter starts at zero for older checkpoints; previously recorded long-break debt remains intact. Duration and cadence edits apply to subsequent periods. An active period keeps its snapshot.

Selecting the opposite state ends the current timed period and begins the requested one in a single durable native transaction. Repeating a request for the current running state does not restart its timer. An early switch out of Focus preserves the partial session without counting it as a completed Focus period.

When a Focus deadline is reached, the Focus scene remains visible and its switch expands into **Break** / **Stay focused**. When a Break deadline is reached, the Break scene remains visible and its switch expands into **Focus** / **Keep resting**. The default choice starts the opposite timed state after twelve seconds in both directions, indicated by a small circle without a numeric countdown. The circle measures the visible invitation interval, not the Focus or Break period. Headings, scenery, notes, and the rest of the page remain in place while the choice appears.

The twelve-second interval runs only while the invitation is visible and the application is focused. Interacting with the choice controls pauses it. Ordinary focus loss or backgrounding resets the interval so returning to the app gives a fresh opportunity to choose. IME composition, an active operation, and an open dialog defer the default transition. Choosing **Stay focused** or **Keep resting** postpones the reminder for nine minutes by default. The person remains in the current state while the completed timer stays ready: delaying does not create an extra completed period or advance the long-break cadence. Delay preferences and the invitation schedule are stored locally, but Windows lifecycle interruption clears outstanding invitations as described below. The background deadline and an unanswered invitation supply no evidence of continued concentration or reduced need for recovery.

The Windows worker sends native notifications only while the application is not focused, avoiding a duplicate OS alert alongside the active interface. A completed Focus notification offers **Take a break** / **Stay focused**; a completed Break notification offers **Focus** / **Keep resting**. Neither shows a numeric delay. These actions are tied to that completed period; stale or duplicate actions cannot replace a newer period. Delivery and actions require the existing desktop process to remain running. An explicit native acceptance brings the existing window forward after applying the choice, without entering full screen. Postponing keeps the app in the background. Merely delivering a reminder never takes foreground focus.

Windows controls native notification appearance, permission, and Do Not Disturb behavior. The twelve-second default belongs to the visible in-app invitation, not an unattended Windows banner. Browser reminders require explicit permission and an open, runnable page; they are silent, nonpersistent invitations. The service worker provides an offline shell, not a reliable scheduler while the browser is closed or suspended.

Windows sleep, hibernation, shutdown, and every desktop restart are interruption boundaries. An active, paused, or recovery-required period ends early using only its last committed elapsed time; the checkpoint becomes Ready for Focus and the interface shows an **untimed Break**. Time while the machine or process was unavailable never completes a period, advances completed-short-Break cadence, or trains either learning model. Existing notes remain available, and Focus starts only after an explicit choice. There is no Resume or recovery screen.

Both immediate and postponed period invitations are cleared on interruption or reopening, along with queued native choices, the inline default circle, and pending learning observations. Missed routine notifications are claimed silently rather than replayed on wake. New future routine windows remain available. An old notification cannot start a new period after the interruption. A fresh explicit Focus enables reminders for its new period.

The Windows clock uses awake elapsed time plus native power notifications and a paired-clock fallback to detect missed sleep callbacks. The worker handles interruption before processing timer-changing requests, and a process-wide generation rejects requests issued before a newer interruption. Desktop startup performs the same interruption before loading any saved reminder. The browser adapter has no equivalent native power/session boundary and does not establish these Windows guarantees; its visible invitation clock still resets after a suspended-page gap.

The Rust timer and CLI retain their existing commands for compatibility, including active Focus extension, pause/resume, early end, recovery, and journal operations. Their availability in a development or automation interface does not introduce those workflows into the three-screen UI.

## Optional behavioral learning

Period learning and reminder-delay learning have independent controls and both start off. Period Settings can choose Everyday/Work/Study/Creative context, set minimum and maximum Focus and short-Break lengths, inspect the next suggestions and observation count, and forget the learned history. Default learning bounds are 10–60 minutes for Focus and 3–15 minutes for a short Break. The separately configured long-Break length is not learned.

Two independent models are maintained:

| Model | Evidence | Effect |
| --- | --- | --- |
| Personal rhythm | Intentional state transitions and observed actual recovery duration | Suggests subsequent Focus and short-Break lengths within the person's bounds |
| Reminder delay | Deliberate responses, visible response latency, and earlier postponements in the same reminder chain | Gradually adjusts the delay before another invitation, independently for Focus and Break |

Period observations are grouped by activity context and local morning/afternoon/evening/night. Each group and state needs five new valid observations before a change of at most one minute. The period model uses the median observed difference from planned duration, so one unusually long or short period does not set the next suggestion. Accidental switches under one minute are excluded. Settings explains the current suggestions; it does not expose the timer continuously on the main pages.

The interface observes actual periods only during the current app lifetime. It excludes a Focus period whose deadline arrives in the background rather than interpreting silence as sustained focus. Interrupted periods, restarted periods, externally replaced periods, and long breaks are excluded from duration learning. Native lifecycle events discard the pending observation before any resumed UI work can train it. Waiting beyond a deadline does not create another completed period. Postponing teaches the separate reminder-delay model; it does not infer additional concentration from the unanswered period. The model does not infer physiological recovery, stress, or health from these actions.

Advanced Settings contains the manual reminder delay, from 1–60 minutes with a nine-minute default, an independent learning switch, and a reset action. The delay model uses a median signal from each five new explicit responses and adjusts by at most one minute. Repeated postponements or recurring late responses after a postponement can lengthen the delay. Prompt acceptance can reduce a learned extension toward the manual baseline; it does not infer a shorter-than-requested delay. A learned extension is capped at the smaller of nine minutes or half the manual delay, and the total is capped at 60 minutes. Focus and Break have separate summaries. Context and time-of-day inputs are reserved for a future model; this first version pools observations by personal state.

Reminder responses never update Focus or recovery duration. Ignored reminders and automatic acceptance after the invitation interval do not train delays, postpone future reminders, or imply that rest is unnecessary. Learning runs quietly without repeated questions or main-page time labels. Only bounded summaries are stored, not raw response timestamps or sensor information.

Period-learning preferences and history are persisted in localStorage under `rhythm.adaptive.v1`; reminder-delay preferences and summaries use `rhythm.reminder-delay.v1`, including in the desktop webview. These live stores are separate from native SQLite, but complete Windows backups include both models and their preferences. Changing a manual delay clears its learned offsets and history. Disabling reminder learning immediately uses the exact manual duration and stops collection; resetting clears learned summaries while preserving the manual value and learning switch. Changing base period durations likewise clears learned rhythm offsets. Wearable signals, physiological baselines, EDA, EEG, and eye tracking are future work, separate from behavioral learning; none are collected or interpreted by this implementation.

## Separate notes for each period

Every timed Focus, timed Break, and untimed Break has its own note. The note starts closed and opens upward and to the left from a fixed bottom-right 52-pixel shell. The icon stays at the same viewport position throughout the 260-millisecond expansion and collapse. The panel and icon share one continuous background and border.

Clicking the icon again, using the close control, pressing Escape, or clicking outside collapses the panel without clearing text. A manual switch, timer deadline, or normal quit archives the ending period's note. A new period receives an empty editor. Completing a timed Break starts a separate untimed Break note even though the visible state remains Break. Nonempty archived notes are readable through the history control inside the same dock; this adds no screen, editor, or navigation destination. The history icon stays unchanged while switching between current and past notes, and a highlight indicates its selected state.

The desktop app stores period notes in SQLite. Archiving and the timer transition commit together. Writes are debounced and serialized, respect IME composition, and check body versions. A save that finishes after a deadline or interruption still updates its original period's archived note, never the new note. State changes and normal quit flush pending text. Native lifecycle preparation also saves the text currently exposed in the textarea without committing or cancelling an active IME composition; a later composition end can refine that same archived note. Windows session ending gives the editor and worker a bounded two-second save opportunity while the UI continues processing messages. This cannot guarantee text still owned by the IME or writes cut short by forced termination or storage failure. Ordinary saving and saved states display no caption. A failed save preserves the in-memory text and offers retry.

The browser uses separate localStorage data under `rhythm.browser.v1`; it does not directly read or automatically synchronize the desktop database. It preserves current and archived notes across reloads, rejects stale saves from another tab, and reports unavailable or full browser storage. Blank archives remain hidden. Earlier nonempty shared Thought drafts are copied to the archive once, without deleting the original drafts.

The note dock's Past notes shows five recent entries. Settings → Read all notes opens an independent, read-only archive with whole-history word search, inclusive date ranges, time-of-day/type/source filters, and newest/oldest creation-time sorting. Windows history includes all archived nonempty period notes, earlier unfinished journal drafts, current reflection variants, and older saved reflection versions. An identical migrated Thought draft is shown once; a subsequently changed draft is separately available. Stable cursors restart the displayed history when content or filters change between page requests. Browser history searches its own stored notes and legacy content. New notes preserve UTC plus their original time zone/offset, and the archive offers current-device or original-time display and filtering; unknown legacy origins are labeled rather than inferred. Editing older period notes remains future work.

## Automatic Google Drive sync on Windows

Advanced Settings now includes the automatic-sync controls for the Windows integration. The initial scope is archived Focus and Break notes, saved reflections and their earlier versions, and ended session history. Current notes, unfinished drafts, active timers, cadence, routines, settings, learning, and backgrounds stay on the current device. Receiving history never starts a timer or replaces the open note editor.

The person enters and repeats a sync passphrase, then connects Google Drive through the external browser. The native worker uses the app-specific Drive area; the person does not choose a folder or manually move files. Writing is encrypted before upload. The same passphrase unlocks the existing encrypted history when connecting another installation. The passphrase is not saved or recoverable; Windows DPAPI protects the remembered authorization and derived key outside the database and portable backups.

The worker checks for changes while the desktop process is open or minimized. Settings shows connection progress, errors, and the last check with Drive, with **Check now**, **Reconnect Google Drive** when authorization expires, and **Disconnect** controls. A successful check is not a claim that every other device has synchronized. Disconnect removes access on the current device and preserves local notes and the encrypted remote copy. Sign-in and status updates do not lock the timer/editor or replace unsaved Settings fields. New history refreshes the read-only history view without changing the current note.

The Google OAuth desktop registration is missing from the current build, so Settings reports that connection is not yet available. Live sign-in, actual Drive exchange, and second-device behavior have not been validated. The browser shell does not run automatic sync. See [automatic sync](automatic-sync.md) for setup, protocol boundaries, and pending acceptance.

## Private file exchange

Advanced Settings retains export of nonempty archived period notes to an encrypted `.rhythm` file, and import on another installation. Export reads every history page up to the format's 1,000-note and byte limits, rather than only the first 100 notes. An oversized archive or history change during collection fails visibly without a partial export. This operation is manual file exchange, separate from the Windows Google Drive integration. The person chooses where to save the file and may move it through a storage provider of their choice. It needs no account and performs no automatic upload. A replaceable file-provider contract accepts encrypted envelopes rather than plaintext or passphrases.

Encryption uses the platform Web Crypto implementation with AES-256-GCM and a passphrase-derived key. A passphrase must contain at least 12 characters. The application does not persist or recover the passphrase. Import validates the file, rejects an incorrect passphrase or changed ciphertext, and preserves every distinct note version instead of replacing a conflicting local note. Reimporting an unchanged variant does not duplicate it. The current active note is not replaced.

This portable copy excludes active notes, earlier journal drafts/reflection revisions, settings, learning history, backgrounds, and any future sensor information. Complete Windows backup is a separate Advanced operation: its standalone SQLite file includes every database note and saved version plus the supported language, appearance, background images, and learned preferences. There is no backup note-count limit. Restore creates a complete safety copy first and durably records pending preference application until the frontend finishes it; reopening retries an interrupted application. Legacy database-only snapshots retain the current frontend preferences. See [backup and export](backup-and-export.md) for restore behavior and full Markdown export.

## Independent backgrounds and motion

Settings offers separate background choices for Focus and Break: Forest, Coast, Dusk, or a personal image. Init uses the Focus background. Choices remain pending until Save settings is selected; dismissing Settings does not apply them. Saving one state's choice does not alter the other state's choice.

The default photographic scenes are bundled as `apps/desktop/ui/assets/focus-still.png` and `apps/desktop/ui/assets/break-still.png`. They were created with the built-in `image_gen` tool. The exact prompts and asset provenance are recorded in [background-sources.txt](../apps/desktop/ui/assets/background-sources.txt). The application loads these assets locally.

Personal images are decoded on the device, resized to a maximum 1,920-pixel long edge, and converted to JPEG. The choices and image data are stored in local IndexedDB. Selecting an image does not upload it or make a network request. A photo stored for Focus is independent of the photo stored for Break. Complete Windows backups include both choices and both retained custom images, even when a built-in background is currently selected.

Focus and Break scenery crossfades over 850 milliseconds. State controls use a brief, restrained entrance transition, while the note panel expands around its fixed anchor. The reduced-motion preference disables these animations and transitions.

## Settings and preview

Settings keeps clear textual labels for period lengths, completed-short-break cadence, and the two background choices. Optional period learning has a collapsed section. Advanced controls contain the manual reminder delay and its learning/reset controls, appearance and language preferences, browser reminder permission, Windows automatic sync, manual encrypted note exchange, and the desktop's existing routine configuration and local data operations. Users can enter Focus without opening Settings.

From the repository root, run:

```powershell
node scripts/preview.mjs
```

Open [Rhythm locally](http://localhost:8767). The preview runs the same HTML, CSS, and interface logic as the desktop app, with a browser storage and timer adapter. It requires no package installation. Browser file downloads/imports and notification permission use browser capabilities; Windows-only operations and routines are not simulated as successful browser actions.

The manifest and service worker supply an optional installable/offline web shell on supported desktop and mobile browsers. Registration is skipped in Tauri and on unsupported or insecure origins. Localhost is suitable for development; deployment to a phone requires a suitable secure origin, since the preview binds to localhost. The static cache includes the bundled interface and backgrounds only. It excludes notes, custom images, imported copies, credentials, and sync traffic. Obsolete-cache cleanup is confined to this application's scope.

The worker never forces a page reload or takes over an open editor. Updated assets become active after older windows close. When changing bundled shell files, increment `VERSION` in `apps/desktop/ui/sw.js`, then close all Rhythm tabs/windows before reopening to allow the new worker to activate. This web shell is not evidence of native macOS/Linux/Android/iOS support, reliable closed-browser notifications, or tested mobile lifecycle behavior.

The established Windows executable and installer build commands are unchanged; see [the build guide](windows-prototype.md#build-and-run). The browser's timestamp-based timer is a preview implementation, not evidence of native background delivery or cross-platform lifecycle reliability.

## Verification and limits

Automatic sync was implemented after the earlier acceptance records below. Its UI controller tests cover stale-response fencing, reconnect/disconnect state, bounded Unicode passphrases, and all three interface languages. Storage, transport, full workspace checks, the Windows build, and the isolated-profile UI check are recorded separately in [automatic sync](automatic-sync.md). Missing OAuth registration means no live Google sign-in or Drive exchange has been performed. No second physical device has been tested. These limits remain even when local automated checks pass.

The [desktop interruption verification](windows-interruption-2026-11-02.md) records the current Windows build: 211 Rust tests, 101 JavaScript tests, 18 installed restart assertions, and preservation of the normal profile. The app was left open in waiting Break. No physical power transition was required for that pass.

The [installed Windows acceptance record](windows-acceptance-2026-11-02.md) adds user-assisted checks of all four fresh native notification buttons and preservation of notes/settings. It predates the interruption policy: its postponed-reminder restart and recovery-required results describe the earlier build. Current desktop reopening clears the reminder and ends the interrupted period directly into Break.

The lifecycle update passes **101 JavaScript tests**, including stale-choice fencing, repeated interruption handling, preservation of visible composing text in its original archived note, and save-failure retry. CI now uses the same complete JavaScript test command as local verification. These automated checks do not substitute for physical Windows sleep, hibernate, shutdown/reboot, installed IME composition, or accessibility acceptance; those current-policy device checks have not been performed in this pass.

Run current JavaScript verification with:

```powershell
npm test --prefix apps/desktop
```

Run the Rust workspace suite with `cargo test --workspace --locked`, or `.\scripts\cargo.ps1 test --workspace --locked` on this workspace. The added suites cover gradual learning and independent reminder evidence, encrypted round trips and conflict retention, and complete offline import coverage with cache isolation. They supplement device checks rather than proving platform acceptance.

### Current inline-reminder verification · November 2, 2026

- **96/96 JavaScript tests and 21/21 native desktop tests passed.** Clippy passed with `-D warnings`, and the Windows debug build succeeded. The native count is the desktop package run, not a new full-workspace result.
- An isolated live Windows profile verified both compact choice pairs and both twelve-second active default transitions. Postponing in each direction saved `delay_ms: 540000`, retained the ready state, and left the completed-period cadence unchanged.
- Advanced Settings was inspected with the nine-minute default, reminder learning off, and its reset control. The local verification captures are excluded from the public source.

Windows OS banner actions were not clicked in this pass. A live responsive-browser check was unavailable. These results do not establish native support or device acceptance on other platforms.

### Historical verification

The preceding adaptive companion implementation was verified on November 1, 2026, before the twelve-second invitation and native action changes:

- All 71 JavaScript tests and 189 Rust workspace tests passed; the Windows development executable rebuilt successfully.
- Live browser checks verified Init → Focus → Break, multilingual archived notes, saved settings, and a real one-minute foreground deadline entering Break automatically. The 25-minute default was restored after testing.
- At a 390 × 844 viewport, Focus and the note editor fit the screen. The note control had identical coordinates before and after expansion.
- After the preview server was stopped, a browser reload still opened the cached application in Break. The server was restarted after this offline check. Physical-device installation and closed-browser reminders remain separate acceptance work.

The following records describe the preceding three-screen redesign, before the adaptive companion additions:

- All 44 JavaScript tests passed, covering the interface model, period-note boundaries and late saves, browser persistence, failure rollback, state switches, background preferences, and legacy-note migration.
- Targeted backend runs passed 91 tests, including period-note archiving and persistence boundaries. This count describes those runs; it is not a claim that every workspace or platform suite was rerun.
- Browser checks confirmed separate archived notes at both manual switches and a real one-minute deadline, the absence of the main-screen minute label and ordinary save caption, persistence of a custom Focus background after reload without changing Break's background, and identical note-toggle coordinates while its panel expands.
- The final Windows native build passed. The rebuilt `target/debug/rhythm-desktop.exe` was opened and visually verified with the new photographic background and caption-free interface.

These earlier check counts are historical, not measurements of every new interaction. The Windows sleep/wake and notification checks are recorded separately. They do not establish installed-app acceptance of the new actionable Windows banners or the twelve-second invitation. Installed-app IME composition, screen-reader behavior, large text, lifecycle recovery, new notification routing, and offline installation still need device-level acceptance for the current design.

The present deliverable covers the three-state interface, local notes, conventional scheduling, opt-in behavioral learning, an offline web shell, and manual encrypted archive exchange, with direct Google Drive automatic sync under Windows integration and release validation. Remaining staged work includes broader live sync acceptance, complete native support and packaging across macOS/Linux/Android/iOS, additional synchronization providers, device acceptance, optional wearable integration with local baseline processing, and more sophisticated personalized recovery models.
