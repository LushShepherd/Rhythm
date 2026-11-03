# Complete backup and older-note verification

Windows build and installed-app checks · November 2, 2026

The complete-backup and note-history implementation was rebuilt and installed. The checks below used an isolated source database and WebView profile, followed by a separate fresh destination database and WebView profile. The source fixture contained **237 period notes: 236 archived notes and one current note**, plus an earlier saved reflection and unfinished journal draft.

## Automated verification and build

- **220 Rust workspace tests passed.** Coverage includes database backup/restore, complete profile validation and persistence, legacy compatibility, durable pending restore acknowledgement, and paginated history with older journal variants and versions.
- **121 JavaScript tests passed.** Coverage includes exact preference/image restoration, failure and retry, complete archived-note collection, and history paging across changes and asynchronous responses.
- Formatting and all-targets Clippy with warnings treated as errors passed.
- The Windows release and installer were rebuilt, and the updated application was installed.

## Installed native backup and restore

The source backup was created through the actual installed Windows interface and native file dialog. The destination restore also used the actual interface, native file selection, and replacement confirmation. A second complete backup was then created through the interface after restoration to verify the resulting saved preferences and image data.

| Stage | Assertions | Result |
| --- | --- | --- |
| Source complete backup | **6/6 passed** | Every one of the 237 period notes, the current note, earlier reflection, and unfinished draft was included. Raw saved preference values and both custom background images matched the source fixture exactly. |
| Restore into fresh independent profile | **17/17 passed** | Database integrity, note and journal tables, saved revisions and ancestry, sessions, routines, and timer settings were preserved. The app waited in Break, generated a fresh device identity, and completed and acknowledged preference restoration. |
| Destination safety backup | Included in the restore assertions | Exactly one safety copy retained the destination's pre-restore draft and default preferences. |
| Backup created after restore | **6/6 passed** | All source note coverage, raw preference strings—including learned preferences—and both custom image byte sequences still matched exactly. |

These checks establish that the complete backup carried data beyond the former 100-note view and that restored preferences and images were actually available for a subsequent backup. They do not infer preference recovery merely from a successful database replacement.

## Older-note access and personal-data preservation

The actual installed **Settings → Read all notes** control was exercised through five pages, using **Load older** four times. Accessibility inspection found all **235 unique imported notes**, including the final oldest note `001`; that oldest note was also visibly readable, and no Load older control remained. A separate check from the fresh destination's Init screen exposed its unfinished draft without starting Focus.

Before reopening the normal personal profile, a cold comparison confirmed that all **15 original database tables / 89 rows**, their schema, and their values matched the saved copy. All **14 inventoried WebView preference-storage files** also matched byte-for-byte, including Local Storage, IndexedDB, Preferences, and Secure Preferences. This is the pre-reopening measurement, recorded in `preservation-before-reopen.json`.

After reopening the normal profile, `preservation-after-reopen.json` confirmed database integrity and exact preservation of all **14 original data tables**, including their rows and hashes. The migration record advanced to six applied migrations, and the new pending-restore table was empty. These expected schema changes did not alter the existing notes, sessions, settings, or other original data.

The normal app visibly reopened in **Break** with **Enter Focus** available. Its saved dark appearance, Forest Focus background, Coast Break background, and 25/5/30-minute settings were retained. The updated app was left running with that normal profile. The byte-for-byte WebView comparison above remains the cold pre-reopening measurement; ordinary browser metadata can change during launch.

## Evidence and limits

Local evidence is retained under `.tools/complete-backup-2026-11-02/`:

- `complete-source.sqlite3.audit.json` records the source backup assertions.
- `installed-restore-results.json` records the restore and safety-copy assertions.
- `complete-roundtrip.sqlite3.audit.json` records the backup made after restoration.
- `installed-history-results.json` records the five-page installed history check, 235 unique imported notes, and visibly accessible oldest note.
- `preservation-before-reopen.json` records the cold normal-profile comparison.
- `preservation-after-reopen.json` records the normal profile's integrity and data-preserving schema migration after launch.
- The complete source/round-trip snapshots, isolated profiles, expected fixture data, and verification helper are retained alongside those reports.

The evidence directory is ignored workspace data and includes private profile material. Do not publish its database or profile files. The document reports comparisons without reproducing personal notes or preference payloads.

No physical sleep, hibernate, shutdown, or reboot was performed. This pass does not establish physical disk-failure behavior, native macOS support, or signed public distribution. Complete SQLite backups remain local and unencrypted; encrypted note exchange is a separate operation with explicit size/count limits. See the [backup and restore guide](backup-and-export.md) for the supported behavior and [current roadmap](roadmap.md) for remaining work.
