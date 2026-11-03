# Glass restore confirmation verification

Windows installed-app checks · November 2, 2026

Backup replacement now uses Rhythm's glass dialog instead of a system warning box. The native file picker remains available for choosing the backup. The dialog identifies the selected file, describes replacement and the safety copy, and offers explicit **Restore backup** and **Cancel** actions. Escape also cancels and returns to Settings.

## Automated checks

- **121 JavaScript tests passed.**
- **27 targeted desktop Rust tests passed**, including cancellation that cannot discard a newer restore selection when presented with a stale token.
- Formatting and strict Clippy passed.

These counts describe the focused verification for this interface change. The broader backup and history verification remains in the [complete-backup record](complete-backup-verification-2026-11-02.md).

## Installed behavior

The rebuilt Windows app displayed the glass confirmation in dark appearance. Escape returned to Settings; comparison found every database table unchanged and no new safety backup. Selecting a backup therefore did not itself restore or write a safety copy.

Explicitly confirming restore in an isolated test profile recovered all **237 test period notes**, completed preference restoration and acknowledgement, and returned to untimed Break. The safety copy preserved the destination's previous test draft.

The installed glass dialog was also visually verified in light appearance. Its **Cancel** button returned to Settings without another restore or safety copy: the isolated profile still contained 237 period notes, one existing safety backup, and no pending preference restore.

## Personal-profile preservation

After installation and before reopening the normal app, the database and inventoried WebView preference-storage files matched the saved original copy exactly. The SQLite comparison included WAL content; this was not a cold-profile comparison.

After reopening, all **16 database tables**, schema hashes, and row-value hashes remained unchanged, and database integrity passed. The app's control interface reported Ready for Focus—the untimed Break state—with no pending invitations. The normal app was left open.

These comparisons are recorded in `.tools/glass-restore-2026-11-02/preservation-before-reopen.json` and `preservation-after-reopen.json`. This ignored evidence directory contains private profile material; do not publish its databases or profile files.

The work changes the confirmation's presentation and cancellation flow. It does not add another backup format or change complete restore into a merge. See the [backup guide](backup-and-export.md) for contents, safety copies, and recovery behavior, and the [roadmap](roadmap.md) for remaining priorities.
