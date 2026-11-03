# Automatic Google Drive sync

Windows implementation and release-validation notes · November 2, 2026

The Windows integration is implemented and the maintainer's Google OAuth desktop registration is bundled in the configured Windows build. On November 2, the owner reported that the app and Google connection worked after the Windows proxy fix. This is user-reported success, not an independently observed two-profile or two-device exchange. The next milestone is public source preparation and native macOS, then Linux development; the remaining live sync checks can proceed alongside that work.

## User experience

Settings → Advanced → Automatic sync contains the direct Google Drive connection. In a configured build, enter a passphrase of at least twelve Unicode characters, repeat it, and select **Connect Google Drive**. Finish authorization in the external browser, then return to Rhythm. The browser's existing Google login may simplify account selection, but does not register or authorize Rhythm by itself.

Rhythm stores encrypted records in its private application-data area. Users do not create or select cloud folders, install Drive for desktop, or manually move files. Google describes this area as hidden from ordinary Drive browsing and accessible to the application; users can still remove its contents through Drive's app controls. Keep independent backups. [Google application data](https://developers.google.com/workspace/drive/api/guides/appdata)

The native sync worker runs while Rhythm is open or minimized. Closing the process stops synchronization. Local saving remains independent of network availability. Settings reports connection progress, an error when attention is needed, and the last successful check with Drive. **Check now** requests another pass; **Reconnect Google Drive** handles expired authorization; **Disconnect** removes remembered local authorization and key material while preserving local notes and the encrypted remote copy. A successful check does not prove that every other device has received the data.

The Windows network client honors the configured system proxy, with explicit proxy environment variables taking precedence. A first connection that fails before its authorization and encryption state can be saved requires choosing **Connect Google Drive** again. Automatic retries apply to a saved, active connection; a failed initial enrollment no longer promises retries that cannot run.

The connection uses a separate native worker so signing in or waiting for Drive does not hold up timer operations. Settings updates sync status in place and keeps unsaved settings controls intact. An import updates the read-only note history without replacing the active note editor. The browser shell retains manual encrypted exchange and does not run automatic Google Drive sync.

## What moves between installations

| Included | Kept on this device |
| --- | --- |
| Archived Focus and Break note variants | Current note and unfinished reflection drafts |
| Saved reflections, their parent relationships, and earlier versions | Active timer, cycle position, pending invitations, and notification state |
| Ended session history, including interrupted periods | Routine definitions and execution, settings, learned preferences, and background images |
| Stable record identities and integrity metadata | OAuth credentials, encryption keys, local sync progress, and installation identity |

Each installation keeps an independent SQLite database. Sync exchanges immutable logical-record archives, never the live database, its WAL, or a replacement journal snapshot. A record archive includes its ancestry so it can be validated and imported atomically. Repeated delivery preserves stable identities; concurrent reflection variants remain recoverable rather than choosing a timestamp winner. Unsupported or inconsistent objects fail visibly instead of partially replacing the journal. Missing remote data is not interpreted as an instruction to delete local notes.

Period-note variants retain their original note identity and content identity across re-export. Independently written identical notes remain separate; a late save to an already archived period creates another retained variant. Copies of unfinished legacy drafts remain local. Contradictory ended-session revisions are preserved but excluded from definitive session history instead of counting both as settled outcomes.

Notes keep their UTC instants and, for new writing, their original IANA time zone and offset at creation. Sync preserves that context instead of substituting the receiving device's zone. The searchable archive can show current-device time or original time; legacy origins remain unknown. Archives containing this metadata use storage sync protocol 2, while legacy-only objects keep protocol 1 and their stable hashes. This build reads both; older builds reject protocol 2. Update each test installation before exchanging new writing.

The transport abstraction permits local mock storage for fault tests. Google Drive is the only provider in this initial native integration. Full enumeration is the initial discovery mechanism. Upload retries retain a pre-generated Drive ID and the exact ciphertext; an already-existing ID counts as success only after a matching read-back. Record and encrypted-envelope size limits are explicit and fail rather than truncating writing. Additional providers, compacted history, preference synchronization, and cross-device live timer handoff are outside this release.

The storage archive limit is 64 MiB and 100,000 combined revisions/note variants. A period-note body is limited to 1 MiB; encoded revision payloads allow up to 8 MiB for escaped content, with at most 1,024 parents. Encrypted envelopes are bounded to 96 MiB. Objects group one reflection/session record with complete ancestry, or one archived period-note variant. Limits produce an error without a partial export or import.

## Encryption and recovery

Records use AES-256-GCM with a fresh 12-byte nonce and a 128-bit authentication tag. PBKDF2-HMAC-SHA256 derives the key using 600,000 iterations and a random 32-byte salt for the vault. Authenticated metadata binds the format, vault identity, and logical object identity. A vault is the encrypted journal collection behind the connection; users need not manage its identifier.

The public vault descriptor contains a salt and encrypted verifier. Another installation must unlock it using the same passphrase. An enrolled installation retains the expected descriptor rather than trusting an unexpected replacement. Passphrases must contain at least twelve Unicode characters and at most 1,024 UTF-8 bytes; text is not normalized. The application cannot recover a forgotten passphrase from Google. There is no key-rotation or device-revocation workflow in this first integration.

Windows DPAPI protects remembered OAuth tokens and the derived encryption key in device-local state. These secrets do not enter the SQLite journal, complete portable backup, manual note exchange, or Markdown export. The passphrase itself is not persisted. A fresh installation must authorize Google and unlock its history; copying a database backup does not transfer cloud access.

Restoring a complete backup first stops and disconnects automatic sync. Queued imports are fenced before the local journal is replaced. Reconnect explicitly afterward to merge the restored history with Drive. A failed restore can also leave sync disconnected; it never deletes the remote history. Disconnect and restore cancellation are checked between network requests and upload chunks; an in-flight request is bounded by a thirty-second timeout.

Encryption protects record contents, not every observable property. Google can observe object counts, sizes, transfer timing, and opaque vault/object identifiers. The local SQLite journal and complete backups remain unencrypted as documented in [backup and export](backup-and-export.md). Cloud sync is not a substitute for an independent backup, and disconnecting cannot retract plaintext already received by another installation. A review of the enrollment and recovery design remains a release requirement.

## Maintainer setup

This is one-time application setup for the maintainer. Ordinary users should receive a configured build and only connect their account.

1. In [Google Cloud Console](https://console.cloud.google.com/), create or select a project named **Rhythm**, then enable **Google Drive API** in **APIs & Services → Library**.
2. Open **Google Auth platform → Branding → Get started**. Use **Rhythm** as the app name and your email for support and contact. For a personal Google account, choose **External** audience; keep it in **Testing** and add your Google account under **Audience → Test users**. Review any Google agreements yourself. Under **Data Access**, request only `https://www.googleapis.com/auth/drive.appdata`; Google classifies this as a non-sensitive scope. Do not broaden it to full Drive access. [Consent setup](https://developers.google.com/workspace/guides/configure-oauth-consent), [Drive scopes](https://developers.google.com/workspace/drive/api/guides/api-specific-auth)
3. Open **Google Auth platform → Clients → Create client**, select **Desktop app**, and name it **Rhythm Windows test**. Create it and download the JSON. No manually entered website redirect URL is needed for this desktop client. Desktop clients are public clients; their bundled client-secret field is not confidential. The app uses PKCE and an ephemeral loopback redirect to receive browser authorization. [Google desktop OAuth](https://developers.google.com/identity/protocols/oauth2/native-app)
4. Keep that registration file outside the repository and point the build process at it using `RHYTHM_GOOGLE_OAUTH_CLIENT_FILE`. The JSON must have Google's `installed` desktop-client structure. The build embeds the application registration, not a user's access or refresh tokens.

For example, replacing the example path with the local downloaded registration:

```powershell
$env:RHYTHM_GOOGLE_OAUTH_CLIENT_FILE = 'C:\private\rhythm-google-desktop.json'
.\scripts\desktop.ps1 build --ci --bundles nsis '--' --locked
```

The same environment setting can be used when running a development build through `scripts/cargo.ps1`. With no registration, Settings reports that the Google Drive connection is not available in this build. Do not treat that as a signed-in user's browser problem.

Complete Google's production publishing requirements before distributing the feature. For an external consent application left in **Testing**, Google normally expires refresh tokens after seven days for this Drive scope; the profile-only exception does not cover `drive.appdata`. The app must handle expired or revoked access by asking for reconnection while local work continues. [Google refresh-token expiration](https://developers.google.com/identity/protocols/oauth2#expiration)

## Verification and release status

The frontend suite includes automatic-sync controller tests for Unicode and size validation, stale status reads, disconnect races, overlapping operations, failed reads, secret-free UI state, and English/Simplified Chinese/Traditional Chinese copy. Run it with:

```powershell
npm test --prefix apps/desktop
```

Verified locally on Windows on November 2, 2026:

- `cargo test --workspace --locked --offline`: **279 tests passed**, including 84 storage tests, 19 transport/crypto/OAuth tests, and 10 exchange-engine tests within the desktop suite.
- `npm test --prefix apps/desktop`: **155 tests passed**.
- Workspace formatting and `cargo clippy --workspace --all-targets --locked --offline -- -D warnings`: passed.
- The Windows release executable and NSIS installer build passed with the supplied Google Desktop app registration. Rhythm's native OAuth parser accepted that file, and an exact-byte check confirmed it is embedded in the release executable. The installed build does not need the original JSON file. This check does not establish that the project has Drive API enabled or the Google account has been added as a test user.
- The preceding automatic-sync build was checked in an isolated native Windows profile with a separate application identifier. Advanced Settings displayed the automatic-sync scope and unavailable-registration message without changing the installed app's journal. The subsequent archive logic has automated coverage and the routine-scroll fix has source review; native visual acceptance remains pending because computer use stopped when Firefox URL-policy enforcement was unavailable.

After a reported failure immediately after Google authorization, a credential-free comparison reproduced the Windows proxy issue. With proxy environment overrides absent, the old client could not reach Google's token service or Drive; the corrected client automatically used the existing Windows proxy and received the expected unauthenticated HTTP 404 and 401 responses. No access tokens, journal data, or Google account requests were used in this probe. This establishes endpoint connectivity, not successful authenticated sync. The correction passed all 19 sync tests, all 42 native desktop tests (including three new failure-message regressions), formatting, and strict Clippy checks for both changed crates.

The owner subsequently reported that everything worked. This is manual confirmation of the current Windows experience and Google connection; it does not prove remote receipt or convergence on another installation. No additional live test was performed for this update.

The tests use isolated local stores and mock transport. They cover offline edits, convergent conflicts, duplicate/reordered delivery, tampering and wrong passphrases, upload-response loss with exact identity/ciphertext replay, disk-write failure, missing/renamed remote objects, incomplete metadata, token revocation, pagination, resumable uploads, cancellation between chunks, and restoration with fresh import tracking. These results do not constitute live Google acceptance or an independent cryptographic security audit.

Remaining live release checks are exchange between isolated profiles through real Drive, reconnect after revocation, and restart behavior. Keep the same recovery passphrase for each test installation. A physical-device exchange should follow when the next platform is available. These checks do not block publishing source or starting the native ports; they remain necessary before claiming fully validated cross-device synchronization.

The earlier manual `.rhythm` encrypted note exchange and complete SQLite backup/restore remain available. Their format and validation results are separate from automatic synchronization.
