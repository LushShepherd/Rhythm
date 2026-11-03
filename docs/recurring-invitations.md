# Daily routine windows

Advanced settings includes **Daily routine windows**. Choose weekdays and add one or more
custom start/end ranges, for example Monday–Friday **08:30–12:00** and **14:30–18:00**.
Each routine accepts up to 12 ranges. Ranges cannot overlap within a routine; adjacent
ranges are allowed. An end must be later on the same day, with `24:00` representing
midnight. A routine can follow the device time zone or stay in a named IANA zone such
as `America/Los_Angeles`. Settings shows the next eligible range.

During a range, the app shows its name and start/end times. **Begin focus** explicitly
starts the routine's focus preset and selects it for subsequent focus starts in that
range. An explicit one-time duration overrides the preset. Short rests, long rests,
and the count of completed focus periods follow the existing cycle settings. Every
focus and rest requires an explicit start; no range starts a timer automatically.

At the end time, the range stops offering its preset for new periods. A focus or rest
already running finishes normally, even if that takes it past the end time. An active,
paused, or recovery-required period is never interrupted or replaced by a schedule.
Starting a manual period clears reminder cards but leaves active ranges available.
Skipping a range persists for that occurrence and leaves later ranges that day available.

Reopening the app within a range makes that range available again without repeating its
notification. The selected focus preset belongs to the current app instance: after
reopening, select an available range again to use its preset for subsequent focus.
Existing single-time invitations keep their original 15-minute availability until edited;
editing one converts it to an explicit start/end range.

Rhythm must remain open or minimized for reminders. It does not register Windows
scheduled tasks, wake the computer, or run after the desktop process exits. Windows
notification preferences and Do not disturb still apply. Notifications use fixed copy,
without journal text or routine names.

## Calendar rules and durability

- Weekdays use Monday=1 through Sunday=7.
- A nonexistent daylight-saving boundary moves to the first valid minute on that date.
- A repeated local boundary uses its first occurrence. A range whose end resolves at or
  before its start is skipped, as is a civil date skipped entirely by a zone change.
- Each range is notified at most once per local date. A range is identified by its start
  time, so changing its end does not repeat a notification or undo a dismissal. Changing
  the start defines a new occurrence. Legacy invitations retain their previous identity.
- Next-occurrence previews skip already delivered occurrences, including after restart
  or clock rollback. The second range on a day has its own delivery and dismissal state.
- Delivery claims commit before reminders appear. A crash between claiming and showing
  can miss a notification, but an undismissed active range remains available in the app.
- Device-zone resolution errors appear in settings. The timer and journal remain usable.

Definitions and delivery history are local SQLite data. Migration 4 adds per-range
delivery/dismissal state while preserving legacy definitions and history. Upgrading
creates a consistent pre-migration backup and leaves journal records unchanged. Manual
backup and restore include the routine data. Routine synchronization is not implemented.

## AI and command-line control

The same actions are available through the [machine-readable CLI](cli-control.md).
Use `rhythm-cli --json routines` to list definitions, next occurrences, active ranges,
the selected range, errors, and pending invitations. `rhythm-cli --json schema` includes
the split weekday example and describes range and cycle behavior. Scheduling and timer
decisions remain in Rust for both the interface and CLI.

## Verification

The Windows workspace has 157 passing Rust tests and 12 passing frontend tests;
formatting and Clippy with warnings denied also pass. Coverage includes split ranges,
exact boundaries, short/long rest cycles, time-zone changes, DST gaps/overlaps, the
midnight before a skipped civil date, legacy schema upgrade, and backup/restore.

An isolated desktop copy verified the live CLI, continuing focus presets, one-time
overrides, invalid-overlap rejection, and preservation of a multilingual test journal.
Native form checks added a second range and saved custom start/end values. Reopening
preserved those values and dismissals while restoring the active range without replaying
its notification. Local evidence is in `.tools/routine-windows-smoke/verification.json`.
The test identity was not installed, so it could not submit native Windows notifications;
this check does not claim a visible reminder banner. The test app was closed afterward.

The updated Windows installer builds and bundles the CLI. The routine-window build was
installed in place on November 1, 2026 after saving and closing the app and creating a
verified journal backup. The actual installed app passed isolated split-range edits,
focus/rest cycles, and restart checks. Its interface exposed both start/end pairs for
08:30–12:00 and 14:30–18:00. The normal journal was then reopened: all existing journal,
draft, history, settings, identity, and routine rows were preserved, schema 4 and database
integrity were verified, and seven read-only CLI commands passed. Installation evidence
and the safety backup are in `.tools/window-install-2026-11-01/`.
