# Rhythm Rust foundation

Implemented November 1, 2026. This is the first local core slice of Phase 1. Tauri 2 remains the leading candidate for the GUI; no framework selection or native platform certification is implied by the core tests.

## Modules and dependency direction

| Crate | Responsibility |
| --- | --- |
| `rhythm-domain` | Validated settings, clock samples, serializable checkpoints, timer transitions, session/interval records |
| `rhythm-application` | Repository port, optimistic transaction orchestration, clock injection, development `ProcessClock` adapter |
| `rhythm-storage` | SQLite implementation of the port, migrations, journal drafts/revisions, local outbox, durable history |
| `rhythm-cli` | Small interactive composition of the application and SQLite adapters |

The application depends on the domain. Storage implements the application-owned repository port; neither the domain nor the application depends on SQLite. The CLI wires them together. The [Windows prototype](windows-prototype.md) now adds a desktop service and a Tauri 2 shell using these APIs; period transitions remain in Rust.

## Timer behavior

Defaults are 25 minutes of focus, 5 minutes of short rest, 30 minutes of long rest, and a long rest after 3 completed focus periods. The engine begins ready. Every new focus/rest period needs an explicit command; polling, saving notes, reopening the database, or completing rest never starts another period.

Elapsed time comes from injected monotonic samples. There is no decrementing UI counter. Pause excludes paused duration. Late completion is capped at the planned duration and records the calculated deadline separately from detection time. Wall-clock jumps do not change elapsed time; uncertain UTC mapping remains marked as uncertain. The engine records intervals so later reporting can exclude pauses and waiting time.

Only completed focus periods increment the cycle. Long rest stays due if skipped or ended early, and completing it resets the cycle. A deliberate new rhythm resets the cycle while ready. Each started period snapshots its settings; updates apply to future periods. One-time durations do not change saved defaults. Durations are bounded from 1 millisecond to 24 hours in the domain, while the CLI accepts whole minutes.

The portable `ProcessClock` uses Rust `Instant` and a random process epoch. It deliberately makes no promise about counting device sleep. A saved running checkpoint seen under another epoch enters `RecoveryRequired`; the person can continue only the verified remaining time or end the interrupted period. Unknown downtime is never converted into recorded focus. A paused period can resume from a new clock anchor without counting time away.

Native adapters still need verified suspend-inclusive clocks, boot identity, notification scheduling, and lifecycle hooks. UTC observations are available now; timezone-aware daily allocation and descriptive statistics are later work. Current period history is a record of elapsed intervals, not proof of attention or quality of rest.

## Durable state and journal

`Rhythm::dispatch` evaluates a command on a cloned engine, asks the repository to atomically save its checkpoint and any terminal session, and only then publishes the new state. A failed write leaves the visible engine unchanged. A revision token prevents two service instances from silently overwriting each other's checkpoint. Reopen after a concurrency conflict instead of blindly retrying a stale command.

SQLite uses foreign keys, a bounded busy timeout, full synchronous writes, and WAL for file databases. Four checksummed migrations create the journal, timer, outbox, and local routine/delivery tables, including delivery and dismissal per time range. Upgrading a recognized older database takes a consistent SQLite backup first. Unknown/newer schemas and changed migration checksums are rejected. SQL files are fixed to LF line endings so checksums remain consistent across source checkouts.

Drafts are durable, device-local records. Autosaves use a draft version so an older editor cannot overwrite newer words. Finalization writes an immutable revision, its parent links, current heads, and configured outbox entries in the same transaction that removes the draft. If finalization fails, the saved draft remains recoverable.

Reflections can stand alone or refer to a session. Thought, intention, and future-self metadata are supported, including an optional reveal timestamp; no future-message notification or reveal-date UI is implemented. Text preserves Unicode, whitespace, and line breaks. Empty drafts are allowed; a finalized reflection requires some text. Each note is bounded to 1 MiB. Journal listing is bounded and ordered, and revision history remains available when older entries are edited.

Editing uses the heads actually read by the editor. If those heads changed, finalization reports a conflict and retains the draft. An explicit API can preserve it as another variant or resolve exactly the reviewed heads. No timestamp selects a winning reflection. The current CLI reports such conflicts and retains drafts; a richer conflict-resolution interface belongs to the GUI phase.

Provider bindings currently prepare local outbox rows only. Registering a binding backfills all immutable revisions and their ancestors. It performs no OAuth, uploads, downloads, provider connection, or cloud synchronization. There is no E2EE or local-database encryption in this slice. Manual backup/restore with identity renewal and basic Markdown journal export are now available through the Windows app; see [backup and export](backup-and-export.md). The complete network protocol remains separate work; do not use file synchronization on the live database.

Storage validates checkpoint/session data, immutable IDs, integrity digests, and transaction revisions. The application layer is responsible for producing a matching checkpoint/terminal transition pair. Frontends should call application services rather than exposing raw repository writes to untrusted input.

## Running the development harness

From the workspace root with a normal Rust installation:

```text
cargo test --workspace --locked
cargo run -p rhythm-cli --locked -- --database rhythm.sqlite3
```

In the current Windows workspace, the local toolchain helper provides the same commands:

```powershell
.\scripts\cargo.ps1 test --workspace --locked
.\scripts\cargo.ps1 fmt --all '--' --check
.\scripts\cargo.ps1 clippy --workspace --all-targets --locked '--' -D warnings
.\scripts\cargo.ps1 run -p rhythm-cli --locked '--' --database .\rhythm.sqlite3
```

PowerShell scripts need the quoted `'--'` to preserve Cargo's separator. The helper uses `.tools/cargo` and `.tools/rustup` when available, otherwise the installed `cargo`. These local tools and database files are ignored by Git. Build tools remain required; the current Windows build uses the installed Visual Studio C++ tools. Do not remove a database's WAL file while the application is running.

| Command | Effect |
| --- | --- |
| `focus` or `focus 10` | Start focus explicitly, with the default or a one-time minute length |
| `rest` or `rest 5` | Start the selected short/long rest explicitly |
| `status` or `poll` | Reconcile elapsed time and show the resulting state; no countdown |
| `pause`, `resume`, `stop` | Change only the current period |
| `new-rhythm` | Reset the cycle while ready |
| `recover continue`, `recover end` | Resolve an interrupted period explicitly |
| `note <text>` | Create and finalize a standalone reflection |
| `journal` | Display the latest 100 entries and their identifiers |
| `edit <record-id> <text>` | Save a new version while retaining history |
| `drafts`, `finalize <draft-id>` | Revisit or finalize a retained draft |
| `help`, `quit` | Show usage, or end an active period before closing |

The harness does not continuously poll or schedule operating-system alerts. `status` or `poll` observes a deadline. Explicit quit ends the current period (or records completion if its deadline has already passed). Closing input checkpoints the current state without ending an unfinished period; the next process requires recovery if it was running. A crash can lose only elapsed time since the latest durable checkpoint and in-memory edits that were never submitted; it must not erase previously committed notes.

## Verification and remaining work

Validated locally on Windows on November 1, 2026:

- `cargo test --workspace --locked --offline`: **64 tests passed** (27 domain, 5 application, 22 storage, 7 CLI, 3 end-to-end).
- `cargo clippy --workspace --all-targets --locked --offline -- -D warnings`: passed.
- `cargo fmt --all -- --check`: passed.
- Built the CLI and exercised start/pause/resume/stop plus journal writing and reopening a disposable database; English, Simplified Chinese, and Traditional Chinese text survived unchanged.

The Windows suite covers deterministic transitions, randomized command sequences, checkpoint serialization/recovery, clock jumps, delayed completion, cycle rules, atomic application commits, database reopen, migration upgrade/checksum refusal, draft/write failures, stale editors, conflicting versions, Unicode, projection integrity, and two simultaneous service instances. Property tests use 128 cases each and no sleeping clocks.

The above results describe the initial core slice. The expanded Windows prototype now passes 92 Rust tests and six frontend tests and has an installed NSIS build. A visible native notification and real Windows sleep/wake recovery passed on the development PC; see [the hardware verification record](windows-checks-2026-11-01.md) for evidence and limits. The GitHub Actions workflow has not been run remotely here. Android/iOS, hibernate/reboot behavior, other Windows hardware, IME, screen readers, and signing remain outstanding. The earlier browser concepts remain separate design demonstrations.

The Tauri 2 Windows integration, recurring weekday invitations, and machine-readable CLI control of the live desktop worker are implemented; other platform feasibility checks remain. Richer Markdown layouts, calendar statistics, full sync, encryption, and AI reviews remain in their roadmap phases. See [recurring invitations](recurring-invitations.md) and [CLI control](cli-control.md) for current delivery and transport limits.
