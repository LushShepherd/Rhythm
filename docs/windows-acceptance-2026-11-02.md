# Installed Windows notification and recovery acceptance

Historical build: the later [desktop interruption update](windows-interruption-2026-11-02.md) changes sleep/reopen recovery to untimed Break and clears old postponed reminders. The button observations below remain evidence for this earlier build.

Completed November 2, 2026, America/Los_Angeles. This run began before midnight on November 1, so its local evidence directory remains `.tools/latest-install-2026-11-01/`.

The current Windows release and NSIS installer were rebuilt and installed. All four native notification actions were exercised with the user's assistance, and forced-process-termination recovery preserved the committed note and verified elapsed time. The normal profile's stored notes and preferences matched its cold backup after installation and after isolated testing. This record covers this installed build on the current Windows host.

## Build and data preservation

The full Rust workspace run passed **198 tests**, and the JavaScript run passed **96 tests**. The release executable and NSIS package were built from the current source. Comparing the installed executable with `target/release/rhythm-desktop.exe` found exactly three differing bytes: the bundle marker changed from `_TYPE_VAR_UNK` to `_TYPE_VAR_NSS`. That packaging substitution accounts for the difference; the two executable files are not byte-identical.

A cold copy of the original application profile and the previous executables was retained before replacement. Tests used an isolated SQLite/reminder directory, `qa-data`, and a separate WebView2 profile, `qa-webview`. A database override alone would not isolate WebView preferences, learning history, or personal backgrounds.

Read-only comparisons in `preservation-after-install.json` and `preservation-after-qa.json` both found:

- SQLite integrity passed; all **15 tables, 89 rows, and the schema** matched the original semantically, using stable hashes of every row.
- All **14 inventoried WebView storage files** matched byte-for-byte: six Local Storage files, six IndexedDB files, Preferences, and Secure Preferences.
- No table or inventoried file was added, removed, or changed.

The inventory covers those preference-storage locations, not every browser cache or profile file. `preservation-baseline.json` records hashes without note text; `verify_preservation.py` performs the comparisons. These exact comparisons were made while the normal profile was closed. The normal application was reopened after QA, so they do not assert that later browser metadata or normal app activity leaves all files unchanged.

After reopening, a final independent comparison again found all **15 original database tables unchanged** and integrity intact. `final-verification.json` passed all **12 aggregate checks** for notification actions, postponement persistence, crash recovery, and normal-profile preservation. The normal app was visibly restored to Init. The WebView byte-hash result above remains the cold, before-reopening measurement.

## Native notification actions

The automation surface could not reliably operate Windows Notification Center. The user opened it and clicked the actual notification buttons; the app's installed CLI status and persisted SQLite/reminder state supplied the corresponding backend evidence. The CLI prepared short, one-time QA periods and read state; the accepted notification actions below came from the Windows buttons.

| Action | Observed result | Local evidence |
| --- | --- | --- |
| **Stay focused** | The completed-Focus invitation changed from sequence 1 to 2, became unavailable, and recorded one postponement with `delay_ms: 540000`. The timer remained Ready for short rest. Terminal sessions stayed at 7; completed Focus stayed at 5 and completed short Breaks at 0. | `notification-action-baseline.json`, `stay-focused-result.json` |
| Restart while postponed | The same period ID, sequence, due timestamp, nine-minute delay, ready state, session count, and cadence survived restart. | `stay-focused-result.json`, `postponed-after-restart.json` |
| **Take a break**, from a fresh notification | A fresh 45-second Focus completed once: sessions increased from 9 to 10 and completed Focus from 6 to 7. The button began a five-minute short Break and cleared the invitation. The user confirmed that Break opened. | `fresh-accept-running.json`, `fresh-take-break-result.json` |
| **Keep resting** | A 20-second short Break completed once: sessions increased from 11 to 12 and completed short Breaks from 0 to 1. The button left the timer Ready for Focus and recorded one nine-minute postponement, without another completed period. The user confirmed that the app stayed in the background. | `short-break-running.json`, `keep-resting-result.json` |
| **Focus** | A fresh 45-second short Break completed once: sessions increased from 12 to 13 and completed short Breaks from 1 to 2. The button began a 25-minute Focus and cleared the invitation. The user confirmed that Focus opened. | `final-break-running.json`, `focus-button-result.json` |

An earlier Take a break attempt produced no observed mutation: the state and reminder in `before-take-break.json` and `take-break-result.json` remained the same. The specific notification selected and the reason were not established. This attempt is inconclusive; it is not evidence that stale-action rejection passed. The later fresh-notification attempt establishes successful acceptance for that case.

Windows Do not disturb was restored to **On** at the end and visually checked in Settings. This run does not establish notification delivery while Do not disturb suppresses banners.

## Forced-close recovery and note preservation

The isolated app was forcibly terminated during a 25-minute Focus after a multiline English/Simplified Chinese/Traditional Chinese test note had been committed. `recovery_audit.py` read the committed database, including WAL state, after termination; it did not infer elapsed time from an earlier live snapshot.

| Stage | Assertions passed | Result |
| --- | --- | --- |
| Cold checkpoint | 7/7 | The interrupted Focus had **22,250 ms** of verified elapsed time, no terminal record, and an unarchived nonempty note. Database integrity passed. |
| Relaunched app | 17/17 | The same period entered `recovery_required`, retained exactly **22,250 ms**, and set its recovery flag. Session count stayed at 13; settings and cadence were unchanged. Every existing note body hash and metadata field matched. |
| Explicit **Focus** in the app | 22/22 | A fresh Focus began. Exactly one terminal record was added for the interrupted period: `ended_early`, `recovered: true`, **22,250 ms**, and no fabricated end timestamp. Its note was archived with the same body hash and version. Exactly one new, empty current note was created; unrelated notes, settings, and cadence were unchanged. |

Evidence is in `recovery-cold.json`, `recovery-reopened.json`, and `recovery-completed.json`. This verifies committed multilingual text through this crash/reopen/transition path. It does not verify Chinese IME composition or survival of characters that had not reached a save boundary.

## Limits and retained evidence

This was an installation and acceptance pass; no application behavior was changed. The local evidence directory contains the original profile backup, previous executables, isolated QA profiles, JSON snapshots, and the audit helpers. It is ignored workspace data and contains private profile material; the document reports results without reproducing personal notes.

No physical sleep, hibernate, or reboot occurred in this pass. The [earlier Windows sleep/wake record](windows-checks-2026-11-01.md) remains separate evidence for its older build and tested paths. This run also does not establish screen-reader or large-text acceptance, installed IME composition, permission-denied behavior, full-screen transitions, signed distribution/uninstallation, or native acceptance on other platforms. The twelve-second inline default is covered by the [separate interface check](three-state-design.md#current-inline-reminder-verification--november-2-2026); it was not requalified here as part of every installed interaction.
