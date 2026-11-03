# Rhythm product design

Design proposal · November 1, 2026

Rhythm protects space for attention, rest, and personal reflection. Its primary action is to begin a period intentionally; its primary record is a person's own words. This proposal preserves the supplied requirements and defines the behavior needed before application implementation. Unspecified policies below are proposed defaults, not additional user requirements.

## The first experience

Open Rhythm to a warm, uncluttered home with **Begin focus**, a small **Leave an intention** action, and access to the journal. No account, schedule, AI setup, or introductory questionnaire is needed. The first period uses 25 minutes of focus, followed by a five-minute rest; after three completed focus periods, offer a 30-minute rest. Every new period requires an explicit start action.

Request notification permission when its purpose becomes clear, around the first start. Explain denied permission without preventing local use. A person can always start a session, write a note, revisit the journal, and export their words offline. Advanced settings contain durations, cycle length, schedules, interruption preferences, appearance, language, sync, and AI.

## Two interface directions

The interactive concepts explore the same product rules with different emphasis. Both are design demonstrations with sample content, not working timer applications.

| Direction | Composition | What to evaluate |
| --- | --- | --- |
| Quiet room | A centered invitation, generous empty space, minimal navigation, quiet focus screen | Is the next action evident without a countdown? Does focus feel spacious rather than empty? |
| Letters | A journal-led layout that places a small personal message beside the next invitation | Does continuity between sessions feel meaningful without making writing seem compulsory? |

Provisional design preference: Quiet room for the everyday start and focus experience, with the warmth of Letters in the journal. Keep both available for review; do not choose a GUI framework based on the appearance of a browser prototype.

The prototype covers beginning focus, optional intentions, ending a simulated period, optional reflection, explicitly beginning rest, leaving rest in an emergency, beginning the next focus period, and editing sample journal text. It also explores the three initial languages and advanced duration settings. Scheduling, operating-system interruption, sync, export, statistics, and AI remain design specifications unless explicitly demonstrated. A prototype control advances a simulated period; it does not establish timer reliability.

## Screen responsibilities

| Screen or state | Primary content and action | Quiet secondary actions |
| --- | --- | --- |
| Home | Begin focus | Optional intention, journal, advanced settings, one-time duration |
| Focus | A short intention or calm sentence; no countdown, progress bar, ticking, or looping motion | Capture a thought, pause, end early, leave fullscreen |
| Focus complete | A rest invitation and optional “What would you like to leave here?” reflection | Save or skip; defer the invitation; end this rhythm |
| Rest ready | Begin short or long rest, with its planned duration visible before starting | Return later, leave a note |
| Rest | A spacious walking, water, eye-rest, or relaxation suggestion | Emergency exit, optional thought, accessible remaining-time detail on request |
| Rest complete | Optional “What feels a little different?” reflection; Ready when you are | Begin focus explicitly, stay away, end this rhythm |
| Journal | Chronological letters, editable text, day/week/month navigation | Find text, export a selected range, inspect preserved versions |
| Advanced settings | Grouped rhythm, routines, appearance/language, connections, and data controls | Restore defaults; capability explanations beside affected controls |

Focus should support desktop fullscreen and an uncluttered mobile screen. Entering fullscreen must never trap the user or hide operating-system escape controls. No daily target, streak, leaderboard, task backlog, or productivity score belongs in the primary interface.

## Period and cycle rules

The application owns one active period per device. Changes to settings affect the next period; each started period captures its duration and cycle policy so an edit cannot unexpectedly end it.

| Current state | Event | Result |
| --- | --- | --- |
| Ready | Person selects Begin focus | Start a focus period and persist its start |
| Focus running | Pause | Freeze the remaining duration; cancel its pending completion alert |
| Focus paused | Resume | Start a new clock anchor for the remaining duration and replace the alert |
| Focus running | Deadline reached | Complete once, offer optional reflection, and wait for rest confirmation |
| Focus running or paused | End early | Preserve the partial period and any note; do not increment completed focus count |
| Rest ready | Person selects Begin rest | Start the proposed short or long rest |
| Rest running | Deadline reached | Complete once, offer optional reflection, and wait for focus confirmation |
| Rest running | Emergency exit | End rest immediately; preserve text; return to a ready state without starting focus |
| Any running state | Crash or ambiguous recovery | Present a recovery choice; do not start another period or silently infer attentive work |
| Any ready state | Schedule reminder or remote sync | Offer an invitation or update records; never start a period |

Optional reflection must not hold the user at a keyboard before resting. “Skip” and “Begin rest” remain available. A saved reflection does not itself start a timer. The implementation may combine saving and an explicitly labeled next-period start in a single action, but never treat background saving as consent.

Only a completed focus period increments the current cycle's count. Focus periods one and two lead to short rests; period three leads to a long rest. Completing the long rest resets the count. If a long rest is skipped or ended early, it remains due; do not punish the person or claim they completed it. Ending a rhythm leaves history intact; starting a deliberately new rhythm resets that local cycle. Preserve a continuing cycle through ordinary app restarts.

A one-time duration replaces the next period's duration only. A completed one-time focus period counts toward its cycle. Paused time, waiting for confirmation, and writing after a period ends are excluded from period duration. These rules need deterministic tests, independent of the UI.

If a completed period is noticed late, count no more than its planned duration. Its effective end is the established deadline, not the later wake or notification-click time. Keep detection time separately. When recovery cannot establish the effective date reliably, show that uncertainty rather than inventing a precise journal allocation.

Closing the desktop window keeps a period running only if the platform adapter supports reliable continuation and explains it. Otherwise offer to keep Rhythm open or end the period. Explicitly quitting during a running period offers End period and quit or Keep running. The first choice saves notes, ends the period early, and cancels its alerts. A crash or operating-system termination follows recovery rules. Mobile backgrounding continues the period using native alerts; it is not treated as an explicit quit.

## Rest interruption and user control

Use three capability-dependent preferences, explained in ordinary language:

1. **Gentle reminder:** a system notification and an in-app rest invitation.
2. **Bring rest forward:** an opt-in, noticeable rest surface where the operating system permits it, with sound optional.
3. **Stay with rest:** an in-app rest screen that discourages returning early and always provides an immediate emergency exit.

Default to a visible in-app rest invitation and an ordinary system notification. “Stay with rest” governs Rhythm's interface, not all other applications. Stronger system alarms are a separate opt-in capability. Avoid promises of blocking a phone, forcing fullscreen over other apps, or bypassing notification choices. Specific platform limits and integration candidates are in [architecture.md](architecture.md).

Emergency exit needs no explanation, typing exercise, or mandatory delay. It must work by pointer and keyboard and remain accessible with large text and a screen reader. Use neutral copy such as “Leave rest now,” without warnings about failure or lost progress.

## Reflections as letters

Separate an intention from a period's closing reflection, but allow either to exist alone. Standalone journal entries need no associated session. A message for a future self may have an optional reveal date; it appears on a future visit without unsolicited reminder notifications by default. Reveal dates organize content; they are not an encryption or access-control boundary.

Use a small, localized prompt collection, not a required questionnaire. Example prompts: “What would help you feel present?”, “What is worth remembering?”, and “What can you set down for a while?” Do not ask for output, hours earned, a score, or a justification for resting.

Autosave drafts locally and expose save failure without discarding text. Preserve the draft across navigation and restart. Finalized edits create new revisions. Conflicting versions remain available with their source device and time; offer Keep both or an explicit edit combining them. Never overwrite a reflection solely because another device reports a later timestamp.

## Routines and recurring schedules

The default repeating cycle requires no schedule. Advanced routines contain one or more time ranges on selected weekdays, such as Monday–Friday 08:30–12:00 and 14:30–18:00. Each routine has a focus duration preset; short and long rests follow the saved cycle settings. Scheduling is a convenience for asking “Would now be a good time?”; every focus and rest still requires start confirmation.

Store weekday rules and local start/end times with an IANA time zone and an explicit choice to follow the device zone or stay in a chosen zone. Default to following the current device zone. Ranges must end later on the same day; midnight can be expressed as 24:00. Reject overlapping ranges within a routine and allow adjacent ranges. For daylight saving transitions, move a nonexistent local time to the next valid local time that day and use only the first occurrence of a duplicated time. Skip a range whose resolved end is not after its start. Show the next occurrence in settings so this behavior can be inspected.

If a reminder arrives during a period, keep the current period unchanged and offer the routine while its range remains open. Combine simultaneous reminders into one choice. Accepting a range selects its preset for subsequent explicit focus starts in that range; an explicit one-time duration takes precedence. A range ending clears that selection and lets any running period finish normally. Dismissing a range skips only that occurrence, not later ranges that day. Do not replay a backlog of missed ranges after sleep or travel. Expired invitations remain missed invitations, not failed or incomplete work. A routine edit replaces future reminders and does not change a running period.

Historical timestamps retain UTC and the applicable zone/offset. November 1, 2026 in America/Los_Angeles, when 01:30 repeats, is a specific scheduling test case. Use a time-zone library rather than fixed UTC offsets for future occurrences.

## Journal exports and descriptive statistics

Offer daily, weekly, and monthly Markdown exports using stable dates, a simple heading hierarchy, and optional YAML metadata compatible with Obsidian. Include personal text and selected session context. Preserve both conflicting versions with clear labels. Let the person choose a folder, preview filenames, and decide how existing exports are handled; do not overwrite unrelated vault content.

Keep statistics in a secondary journal view: completed focus periods, time recorded in focus and rest, early endings, and available reflections. Call these records of periods, not verified attention or quality of rest. No score or target is inferred from them. SQL defines all totals; AI may not invent or recompute statistics.

Use explicit local date ranges. A proposed consistent convention is to assign completed-period counts to their end date, while allocating measured duration across day boundaries using the stored intervals. Exclude pause and confirmation time. Display uncertain recovered intervals separately, and do not deduplicate two devices' overlapping sessions by guessing that they describe the same activity. See [storage-and-sync.md](storage-and-sync.md) for queries and record details.

## Privacy and optional connections

Core use requires no Rhythm account or Rhythm server. Keep network activity absent until a person enables a network feature. Local storage, Markdown export, independent backups, remote synchronization, and cloud AI are separate choices.

Connection setup should normally ask only for a provider and authorization. Advanced endpoints and credentials belong behind advanced provider setup. Store secrets in the operating-system credential facility, never in journal records, exported Markdown, or frontend state. Local SQLite is not automatically encrypted; remote end-to-end encryption and local disk protection solve different problems. State the actual protection offered by the shipped version.

For an AI review, select a day/week/month, preview the exact reflections and SQL statistics to be sent, identify the provider/model/endpoint, and explicitly run the review. Default to no scheduled AI calls. Support a cloud API adapter and an Ollama adapter, with an editable endpoint and an honest distinction between this-device and remote Ollama. A localhost service must be reachable from the device running Rhythm.

Treat journal content as untrusted input to the model, with no tool execution or access to provider credentials. AI output is an editable, separately attributed suggestion with links to its included entries, not a psychological assessment. Keep the original notes unchanged. Remote encrypted sync does not prevent a chosen cloud AI provider from seeing the decrypted excerpts deliberately submitted for review.

## Language and accessibility

Initial language identifiers are `en`, `zh-Hans`, and `zh-Hant`. Detect system language with script-aware fallback, while preserving a manual choice. Map Traditional Chinese preferences, including relevant region fallbacks, to `zh-Hant`; do not infer all Chinese text from a single `zh` translation. Keep all prompts, notifications, errors, settings, exports, dates, and accessibility labels in the localization plan.

Use warm neutral surfaces, system-following light/dark appearance, readable contrast, text enlargement, visible keyboard focus, generous touch targets, and reduced-motion support. Chinese font fallback and IME composition must work in every journal editor. Only announce meaningful timer state changes to screen readers, not second-by-second ticks. Translation and tone require human review in both Chinese scripts.

## Requirement coverage

| Supplied requirement area | Design coverage | Delivery phase |
| --- | --- | --- |
| Philosophy and immediate defaults | Quiet home, optional writing, 25/5/30 and every third completed focus | Foundation and local alpha |
| Focus, rest, emergency exit | State machine, explicit confirmation, capability-based interruption | Foundation and platform alpha |
| Personal reflection and history | Drafts, standalone notes, intentions, future letters, editable revisions | Local alpha |
| Flexible schedules and one-time periods | Weekdays, routines, time zones, DST and overlap rules | Local alpha |
| SQLite and migrations | Local source of truth, transaction boundaries, recovery | Foundation |
| Journals and statistics | Daily/weekly/monthly exports and SQL-defined totals | Data portability phase |
| Optional sync | Google Drive first, then OneDrive, WebDAV/Nextcloud, S3/R2; Syncthing export directory later | Sync phases |
| AI reviews | Explicit selection, payload preview, cloud and Ollama, immutable originals | Optional reviews phase |
| Internationalization and appearance | Three languages, script-aware selection, system theme, accessible responsive layouts | Prototype and every release |
| Five target platforms | Shared Rust domain with native adapters and explicit release gates | Platform spikes and rollout |
| Tests and documentation | Deterministic core tests, lifecycle matrix, migration/sync tests, device evidence | Every phase |

The delivery sequence and concrete acceptance gates are in [roadmap.md](roadmap.md). No native app, database, provider connection, or model integration has been implemented in this design pass.
