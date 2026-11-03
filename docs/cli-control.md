# AI-friendly desktop control

The CLI's `--json` mode controls the **running Windows desktop app**. Every timer,
journal, and routine change goes through the same serialized Rust worker as the UI.
It does not open the app's SQLite database, launch another timer, or require screen clicks.

Build the CLI with `scripts/cargo.ps1 build -p rhythm-cli --locked`, or use the CLI
executable supplied with the updated desktop installation. Start the updated Rhythm app
first, then run:

```powershell
.\target\debug\rhythm-cli.exe --json schema
.\target\debug\rhythm-cli.exe --json status
.\target\debug\rhythm-cli.exe --json focus 25
.\target\debug\rhythm-cli.exe --json pause
.\target\debug\rhythm-cli.exe --json resume
.\target\debug\rhythm-cli.exe --json stop
.\target\debug\rhythm-cli.exe --json routines
.\target\debug\rhythm-cli.exe --json invitations
.\target\debug\rhythm-cli.exe --json quit
```

`schema` works even when the app is closed. It lists commands, request examples, limits,
and exit codes. `rest [minutes]`, `new-rhythm`, `recover continue|end`, `journal`,
`drafts`, and `settings` are also supported. Reads are selected on the server: a status
request does not transmit journal text. Journal listing retains the desktop's latest-100
limit; basic Markdown export remains the complete archival export.

## Closing the app

`rhythm-cli --json quit` saves pending journal writing, ends an active focus/rest period
using the same rules as **Save and quit**, and closes the desktop app. `stop` only ends
the current period and leaves the app open. Quitting does not finalize a draft into a
letter or start another period.

The app first asks its editor to flush pending writing. It refuses to quit while another
UI operation is busy, text composition is in progress, a routine editor is open, saving
fails, or the editor cannot respond. Finish the current edit or resolve the save error
and try again. There is no force-kill option that bypasses saving.

The successful JSON result contains `"closing": true`. It confirms that writing and
the final timer state have been saved; the app exits after attempting delivery of that
reply. A rejected quit keeps the app open. As with other commands, an interrupted reply
can leave the outcome uncertain. A closed app returns the ordinary unavailable error.
The quit operation uses the same request-ID replay protection as other changes.

## Structured changes

For full control, write a JSON request to a UTF-8 file and use:

```powershell
.\target\debug\rhythm-cli.exe --json --request-id morning-routine-1 request --file request.json
```

For example, `request.json` can contain:

```json
{
  "op": "save_routine",
  "data": {
    "id": null,
    "routine": {
      "name": "Weekday rhythm",
      "weekdays": [1, 2, 3, 4, 5],
      "windows": [
        { "start": "08:30", "end": "12:00" },
        { "start": "14:30", "end": "18:00" }
      ],
      "timezone": "system",
      "focus_ms": 1500000,
      "enabled": true
    }
  }
}
```

Weekdays use Monday=1 through Sunday=7. `system` follows the device time zone; a named
IANA zone such as `America/Los_Angeles` stays fixed. Pass the existing routine ID to edit
it. Each routine accepts 1–12 non-overlapping time ranges. Starts use `HH:MM` and ends
must be later on the same day; `24:00` is allowed as an end. Adjacent ranges are allowed.
The legacy `local_time` field is still accepted instead of `windows` for an invitation
that expires after 15 minutes.

`routines` returns `routine_windows` for currently available ranges, including after an
app restart, and `selected_routine_window` for the window selected in this app instance.
Use the returned `routine_id` and `due_at_utc_ms` with `accept_invitation` to explicitly
begin focus. Further `focus` commands without a duration use that window's focus preset
until it ends. An explicit duration overrides the preset. Rest uses the saved short/long
rest settings and completed-focus count. Every focus and rest still requires a start.
At the window's end, a running period finishes normally and the preset stops applying
to new periods. After an app restart, accept an available window again to select it.
`dismiss_invitation` skips only that time range for the day, including after restart;
the afternoon range remains available when its start time arrives.

The same request interface supports creating/saving/finalizing journal drafts, editing
reflections, changing settings, and dismissing invitations. JSON strings preserve line
breaks and both Chinese scripts. Use draft IDs and versions returned by the app when
saving or finalizing; a stale version fails without replacing newer words.

`request --stdin` accepts one JSON object from standard input. `settings --file PATH`
and `settings --stdin` accept just the settings object. Unknown operations, fields, or
invalid JSON fail before reaching the desktop. No interactive confirmation is shown in
machine mode, so the controlling AI should send only changes the user authorized.

## Replies and retries

Standard output contains exactly one JSON response, without a banner or prompt:

```json
{
  "protocol_version": 1,
  "request_id": "morning-routine-1",
  "ok": true,
  "result": {}
}
```

Failures have `ok: false` and `error: { "code": "...", "message": "..." }` instead
of `result`. Exit codes are 0 for success, 2 for input errors, 3 for unavailable desktop
or unsupported platform, 4 for an application rejection, and 5 for an uncertain outcome
after the connection was established. Scripts should check both the exit code and `ok`.

No command is retried automatically. An interrupted reply can occur after the app has
committed a change. Inspect status or drafts first. Within the **same running app
instance**, retrying the same `--request-id` with identical content returns its cached
response without repeating the change. Reusing an ID with different content fails.
After an app restart this ledger is gone, so do not assume the old ID is still protected.

Responses are cached up to 256 entries/64 MiB. Older processed IDs remain marked and
return `response_expired` without repeating the change. The ledger allows 8,192 distinct
mutation IDs per app instance; new changes then fail with `control_capacity` until an
app restart. Requests are limited to 1 MiB encoded JSON and replies to 32 MiB. Large
journals should use the desktop's Markdown export instead of a single large CLI listing.

## Local access and development

Transport is a Windows named pipe with a protected current-user access list, remote
clients rejected, and a single owner for the initial pipe instance. It opens no TCP port
and installs no network service. Other applications running as the same Windows user
can control Rhythm, just as they can access that user's local files. Keep the CLI local;
do not expose it as an unauthenticated remote service.

The normal endpoint uses `%LOCALAPPDATA%\app.rhythm.quietroom`. For an isolated desktop
started with `RHYTHM_DATA_DIR`, use the same absolute directory:

```powershell
.\target\debug\rhythm-cli.exe --json --data-dir C:\path\to\test-data status
```

`RHYTHM_DATA_DIR` is also recognized by the CLI. In restricted execution environments
that remap local application data, explicitly pass the real app data directory. Read and
write timeouts bound abandoned pipe clients; a stalled client does not block the timer.

The original interactive CLI remains available without `--json` for an **isolated
development database**. Never point that older direct-database harness at the desktop's
live database. The new machine mode is the supported way to control the open app.

## Quit verification

The November 1, 2026 Windows build passed 166 Rust tests, 17 frontend tests, formatting,
and Clippy. An isolated desktop check verified that a simulated draft-save failure
refuses `quit` and leaves pending writing visible. Retrying after saving was restored
saved English and Chinese writing, ended an active focus period exactly once, returned
the closing receipt, and exited. The draft and completed period survived reopening.

The installed release was then checked using all seven read commands and `quit`.
It exited successfully and reopened with every row in all 13 database tables unchanged
from the pre-install safety backup. Local verification records are in
`.tools/quit-smoke/verification.json` and
`.tools/quit-install-2026-11-01/verification.json`.
