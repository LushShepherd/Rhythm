# Complete backups, restore, and note access

The Windows app provides **Create complete backup**, **Restore backup**, and **Export Markdown** in Settings → Advanced. Note writing is flushed before these operations. Save any edits in the Settings form first when you want those new preferences in a backup. Native file pickers select the destination; existing files are never overwritten.

## Create a complete backup

Choose **Create complete backup**, then a new `.sqlite3` filename. The result is one standalone file containing:

- Every period note, including the current note and empty records retained for late saves.
- The earlier journal's drafts, saved revisions, and unresolved variants, plus sessions and cycle state.
- Timer settings and routine definitions and delivery history.
- Saved language, appearance, and interface preferences.
- Both background selections and both retained custom images, including an image currently hidden behind a selected built-in background.
- Period-learning preferences, bounds, context, and summaries, plus reminder-delay preferences and learned summaries.

The backup has no note-count limit. It uses SQLite's online backup API, includes committed WAL content, verifies integrity, and needs no WAL sidecar. The supported frontend profile is validated and embedded into the backup copy; taking a backup does not overwrite the live preferences or create a pending restore in the running app.

The profile section preserves only Rhythm's supported settings and background image data. It does not copy browser caches, OS notification permissions, credentials, or unrelated localStorage. Supported custom images are bounded to five MiB each and the serialized profile has a size bound; invalid or unsupported preferences fail visibly instead of being replaced with defaults in a supposedly complete backup.

Complete backups are local and **unencrypted**. Keep them in a private location separate from the live application data folder when protecting against device loss. Encrypted note exchange is a separate format with narrower contents.

## Restore a complete backup

Switch to untimed Break before restoring. Choose **Restore backup**, select the snapshot in the native file picker, and confirm replacement in Rhythm's glass dialog. It shows the filename, explains whether this is a complete or older database-only backup, and describes the safety copy. **Cancel** or Escape returns to Settings without restoring. Restore replaces the local journal and saved profile; it does not merge two complete profiles.

Before replacement, Rhythm creates a complete `rhythm-before-restore-<unique ID>.sqlite3` safety backup alongside the current database. That copy includes the current notes, history, preferences, learning summaries, and backgrounds. If the safety copy fails, restore stops. The result identifies the safety copy so it can be retained or restored later.

The selected source is staged and validated: schema and migration checksums, SQLite integrity, record references, journal/session payloads, profile integrity and supported format, preferences, and image data. Supported older schemas migrate in staging. The source is unchanged; newer, unrelated, corrupt, or unsupported input is rejected. A failed attempt can leave a safety copy, but validation must not partially replace the journal.

The database replacement and pending profile restoration commit together. Rhythm then writes the background and preference stores and acknowledges that exact pending restore only after they succeed. If those stores cannot finish, normal editing remains blocked and the pending restore is retried on reopening. Applying the same snapshot again is safe. The complete safety backup remains available even if a failure prevents rolling back a browser preference write.

The restored installation receives a fresh device identity and clears provider bindings/outbox state; historical authorship is preserved. Restored active periods end at their saved awake boundary, with their note retained, and the desktop waits in Break. Restore does not resume Focus or replay old reminders. A deliberate Focus starts the next period.

## Older database-only backups

Existing Rhythm `.sqlite3` backups without a complete profile remain supported. They restore all data they contain and retain the current language, appearance, backgrounds, and learning preferences. They cannot recreate preferences that were never included. A complete safety backup is still made before replacement.

The complete snapshot format does not require copying an active WebView profile or swapping files beneath open browser databases. That separation keeps the source backup portable and allows interrupted preference application to be retried.

## Access every older note

**Past notes** in the small note panel shows five recent entries. Choose **Settings → Advanced → Read all notes** for a separate archive, available even on Init before starting Focus. Opening and closing it preserves Settings values and scroll position. Search words across the whole history, select an inclusive date range, filter by morning/noon/afternoon/evening/night or note type/source, and sort newest or oldest first. Every archived nonempty period note is accessible; ordinary timer snapshots do not limit the search. Windows history also includes unfinished earlier journal drafts, current saved variants, and prior saved reflection versions. Identical standalone thought drafts already copied during the earlier note migration are shown once; a changed draft remains separately accessible.

Pages use a stable creation-time/identifier order. If notes or search filters change between requests, the next page restarts instead of mixing results or skipping entries. A failed page keeps the already displayed notes available for retry. Current writing remains in its editor. History is read-only; editing older period notes remains future work.

UTC preserves chronological order across devices. New notes retain their original IANA time zone and UTC offset at creation; period notes use the period's start time. The archive can display and filter in the current device zone or the recorded original time. Morning is 05:00–11:59, noon 12:00–13:59, afternoon 14:00–17:59, evening 18:00–21:59, and night 22:00–04:59. Older notes without source metadata remain explicitly unknown; original-time calendar filters exclude them, and unfiltered original-time results label their UTC fallback. Moving devices or daylight-saving changes never rewrite the original instant or offset.

The browser preview also supports paged access to its stored notes and legacy content. Its storage remains separate from the native app; complete SQLite backup and restore are Windows desktop operations.

## Encrypted archived-note exchange

Encrypted export now reads all archived-period-note pages rather than only the first 100. The file format supports up to 1,000 notes and its existing per-note/package byte limits. An oversized archive or a history change during collection produces an error; no partial archive is silently exported. Use a complete backup to preserve all data beyond these exchange limits.

The encrypted file includes archived period notes only. It excludes current writing, earlier journal drafts/reflection revisions, sessions, settings, learning summaries, and backgrounds. Import preserves distinct conflicting variants and leaves the current note unchanged. It remains a manual, user-selected file operation, not automatic cloud synchronization.

Files containing original time-zone metadata use encrypted-note format 2; this build still reads format 1. Older builds reject format 2 instead of silently losing the metadata. Legacy-only exports remain format 1.

## Complete Markdown export

**Export Markdown** writes a UTF-8 document containing all period notes, every earlier saved reflection version and unresolved variant, and every unfinished draft. It includes records beyond any visible history page. UTC dates and stable identifiers identify the records; provider/account information is excluded.

Text remains literal inside Markdown fences, preserving Chinese text, whitespace, embedded backticks, and arbitrary Markdown. Body byte lengths distinguish original text from formatting newlines. One database read snapshot supplies the export.

Markdown export is one-way; edited Markdown cannot be imported. Timer history and settings belong to a complete backup. Date-range exports, daily/weekly/monthly layouts, and richer Obsidian formatting remain later work.

## Verification scope

The [installed complete-backup verification](complete-backup-verification-2026-11-02.md) records the tested Windows build, native file-dialog backup/restore, preference/image round trip, older-note access, and the scope of the observed results.

Automated coverage includes complete profile serialization, exact preference and image recovery, omitted-key handling, write failure/rollback/retry, WAL-consistent database copies, legacy backup compatibility, pending restore acknowledgement, source immutability, and invalid/corrupt input. History tests cover hundreds of notes, tied timestamps, Unicode whitespace, legacy variants and previous versions, changed-history restart, and stale asynchronous requests. Encrypted export tests verify collection beyond 100 notes and refusal of partial results.

These tests do not establish physical disk or power-loss behavior, native macOS support, or a final signed distribution. Installed acceptance results are recorded separately for the build actually tested; older records with smaller test counts describe their earlier implementations.
