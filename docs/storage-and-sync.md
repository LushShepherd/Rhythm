# Rhythm storage and synchronization design

Status: broader target design, researched November 1, 2026. The local SQLite store, journal revisions/drafts, timer transactions, complete backups, and exports are implemented. The November 2 [Windows automatic-sync implementation](automatic-sync.md) now supplies encrypted immutable record exchange and a direct Google Drive adapter; live provider acceptance still requires the application registration. The narrower first release excludes routine/preference synchronization, deletion/tombstone authoring, key rotation, additional providers, and calendar statistics. DDL below remains illustrative; the executable migrations are in `crates/rhythm-storage/migrations`.

Rhythm should save locally before doing anything with the network. Every device owns an independent SQLite database. Optional synchronization moves versioned logical records through the user's chosen storage provider; it never uploads a live SQLite database, WAL file, or shared-memory file.

The proposed model uses immutable revisions with explicit parent relationships. This makes concurrent writing recoverable without a Rhythm account, central coordination server, or a timestamp-based winner that can discard a reflection.

## Synchronization approaches compared

| Approach | Assessment for Rhythm |
| --- | --- |
| Shared or file-synchronized SQLite | Reject as the live sync mechanism. Concurrent writers, WAL state, and file-level conflicts do not provide reflection-level recovery. Consistent database snapshots remain useful for explicit backups. |
| Replace a whole journal snapshot | Simple initial transport, but competing offline snapshots need a full merge and can overwrite unrelated changes. Useful only as a verified recovery/export format. |
| Last-writer-wins records | Small and simple, but clock skew and simultaneous edits can silently discard words. Unsuitable for reflections. |
| Character-level collaborative CRDT | Can merge simultaneous text operations, but introduces operation history and editor integration that Rhythm does not currently need. A mechanically merged passage may also hide the intent of two independently written letters. Reconsider if live coediting becomes a requirement. |
| Immutable record revisions with explicit parents | Recommended: provider-independent, inspectable, retryable, and preserves independent versions. The cost is a conflict-review experience and conservative history retention. |

These are design tradeoffs, not performance benchmarks. Protocol v1 prefers recoverable whole reflections over automatic text blending.

## Data ownership and boundaries

- The Rust repository layer owns validation, transactions, migrations, revision creation, statistics, and export. The interface receives domain operations rather than arbitrary SQL.
- Synchronizable records are reflections, terminal session history, routine definitions, and explicitly portable preferences. AI reviews may become a separate opt-in record kind later.
- Active timers, cycle position, pending confirmations, unfinished reflection drafts, notification handles, rest enforcement, permissions, OAuth state, provider cursors, and credentials remain device-local.
- Two devices can record sessions independently. Synchronizing history does not start a timer, resume focus, dismiss a rest screen, or replay another device's schedule trigger.
- One provider per vault is sufficient for the first synchronization release. A vault is an opaque random identifier for one journal collection; ordinary users need not manage vault IDs.
- All revisions and tombstones are retained in protocol v1. History compaction and permanent erasure need a separate reviewed design.

## Local database

Use a serialized writer with short transactions, bounded busy retries, and local WAL storage. Enable foreign keys on every connection rather than relying on a default. SQLite documents that foreign-key enforcement must be enabled explicitly. [SQLite foreign keys](https://www.sqlite.org/foreignkeys.html)

Use opaque random record and revision IDs. Device identity is installation-specific, and a monotonically allocated device sequence identifies each authored revision. Sequence allocation and the revision insert occur in the same transaction. A restored or cloned installation receives a new device identity.

| Table | Intended fields and responsibility |
| --- | --- |
| `schema_migrations` | Version, checksum, application version, applied UTC timestamp |
| `devices` | Device ID, optional display label, creation metadata; no credentials |
| `records` | Stable record ID, kind, creation metadata |
| `record_revisions` | Immutable versioned payload, author and sequence, timestamp, tombstone, integrity digest |
| `revision_parents` | Parent edges within one record; several parents represent an explicit resolution |
| `record_heads` | Maximal revisions; several heads mean a conflict remains |
| `session_projection` | Terminal outcome, phase, planned/recorded duration, effective end and detection timestamps, effective local end day or uncertainty group, source zone |
| `session_intervals` | Active interval boundaries, measured elapsed time, source zone/offset, allocation confidence |
| `reflection_projection` | Current text variants, optional session link, intention/future-self classification, dates |
| `reflection_drafts` | Device-local draft ID, optional record/session ID, prompt/context, base revision IDs, text, updated timestamp, status (`editing`, `conflict`, `finalized`) |
| `schedule_projection` | Routine timezone, weekdays, local start time, durations, enabled state |
| `local_timer_checkpoint` | Device-only phase, generation, clock anchors, elapsed time, pending transition |
| `local_settings` | Language/appearance overrides, notification and enforcement choices |
| `sync_outbox` | Pending revisions, provider binding, stable remote upload identity, retry state |
| `sync_provider_state` | Vault/account binding, opaque cursor, last success; credential-store reference only |
| `sync_inbox` | Downloaded envelopes staged pending validation or missing ancestors |
| `sync_quarantine` | Invalid, corrupt, incompatible, or suspicious objects and a bounded diagnostic |

Projections are rebuildable indexes over accepted revisions, not a second source of truth. Preserve all unresolved reflection variants in the journal view. Session-to-reflection references are optional; a person can write without running a period.

### Illustrative revision schema

This DDL shows relationships and invariants, not a complete migration. A first migration will also define projections, indexes, inbox/outbox state, limits, and repository-level validation.

```sql
CREATE TABLE records (
    record_id TEXT PRIMARY KEY NOT NULL,
    kind TEXT NOT NULL,
    created_at_utc_ms INTEGER NOT NULL
);

CREATE TABLE record_revisions (
    revision_id TEXT PRIMARY KEY NOT NULL,
    record_id TEXT NOT NULL REFERENCES records(record_id),
    author_device_id TEXT NOT NULL,
    author_sequence INTEGER NOT NULL CHECK (author_sequence > 0),
    schema_version INTEGER NOT NULL CHECK (schema_version > 0),
    created_at_utc_ms INTEGER NOT NULL,
    tombstone INTEGER NOT NULL CHECK (tombstone IN (0, 1)),
    payload_json TEXT NOT NULL,
    payload_sha256 BLOB NOT NULL,
    UNIQUE (author_device_id, author_sequence),
    UNIQUE (record_id, revision_id)
);

CREATE TABLE revision_parents (
    record_id TEXT NOT NULL,
    revision_id TEXT NOT NULL,
    parent_revision_id TEXT NOT NULL,
    PRIMARY KEY (revision_id, parent_revision_id),
    CHECK (revision_id <> parent_revision_id),
    FOREIGN KEY (record_id, revision_id)
      REFERENCES record_revisions(record_id, revision_id),
    FOREIGN KEY (record_id, parent_revision_id)
      REFERENCES record_revisions(record_id, revision_id)
);

CREATE TABLE record_heads (
    record_id TEXT NOT NULL,
    revision_id TEXT NOT NULL,
    PRIMARY KEY (record_id, revision_id),
    FOREIGN KEY (record_id, revision_id)
      REFERENCES record_revisions(record_id, revision_id)
);

CREATE TABLE sync_outbox (
    provider_binding_id TEXT NOT NULL,
    revision_id TEXT NOT NULL REFERENCES record_revisions(revision_id),
    remote_upload_identity TEXT,
    retry_after_utc_ms INTEGER,
    attempts INTEGER NOT NULL DEFAULT 0 CHECK (attempts >= 0),
    PRIMARY KEY (provider_binding_id, revision_id)
);
```

Validate record-kind consistency, acyclic ancestry, author-sequence reuse, supported payload versions, valid UTF-8, bounded sizes, and payload content in Rust before applying a revision. Stage children with missing parents in the inbox instead of violating foreign keys or treating absent history as a new root.

The same revision ID with different bytes is corruption or equivocation, not an update. Quarantine it. A digest detects accidental changes; it does not authenticate a remote writer.

## Editing, conflicts, and deletion

Autosave unfinished writing to `reflection_drafts` in short local transactions. Preserve the draft ID, context, and base revision IDs across navigation and restart. Show an autosave failure and keep the in-memory text available for retry; never claim a draft is durable before its transaction commits. Partial drafts do not enter the synchronization outbox in v1.

Finalizing a draft atomically writes its immutable revision, parent edges, updated heads, typed projections, and outbox entries for configured provider bindings, then clears the draft or marks it finalized with the resulting revision ID in that same transaction. When sync has never been enabled there is no provider outbox entry; revisions remain durable locally. Connecting a provider backfills its queue from all revisions not yet confirmed at that binding, including their ancestry. A failure rolls back finalization and leaves the durable draft recoverable. A committed save succeeds while offline; failed network delivery never rolls it back.

For edits to an existing reflection, compare the stored base revision IDs with current heads inside the finalization transaction. If they moved, preserve the draft and surface a conflict; never silently rebase its text onto newer parents. The user may save it as a variant against its original bases, inspect the newer text, or explicitly combine reviewed heads. Only successful finalization or explicit discard clears the draft.

For each record, derive heads as the accepted revisions that have no accepted descendants. Arrival order and wall-clock timestamps do not determine which text survives.

1. Devices A and B both read reflection revision `r1`.
2. A writes `r2` with parent `r1`; B writes `r3` with parent `r1` while offline.
3. Synchronization retains both heads because neither descends from the other.
4. The journal presents both texts with neutral labels and actions to keep one, keep both, or combine them.
5. An explicit combined revision `r4` names both `r2` and `r3` as parents. Its ancestors remain recoverable.

Editing one branch alone must not implicitly resolve every other branch. A resolution names exactly the heads the user reviewed. A late offline branch can therefore reopen a conflict safely. “Keep both” creates a new record for one variant with provenance, then resolves the original explicitly.

A deletion is a tombstone revision whose parents are the versions intentionally deleted. A tombstone and a concurrent edit remain a visible conflict. Hide a record from the ordinary journal only when its accepted heads resolve to deletion. Undo creates a new revision; it does not mutate history.

Removing a file from the cloud is not a logical delete. It can mean revoked access, provider failure, manual removal, or damage. Preserve local data and surface recovery rather than translating missing remote objects into lost notes.

Do not garbage-collect old heads or tombstones by age. A long-offline device can still refer to them. Future compaction needs explicit device retirement, acknowledgement semantics, and a verified snapshot/rejoin protocol. Permanent deletion also has to address backups and remote copies; v1 must not promise it.

## Logical wire format

Use a documented encoding and version each protocol and record payload independently. The following plaintext envelope is illustrative; timestamp values describe authorship for display, not authority or causality.

```json
{
  "protocol": 1,
  "vault_id": "opaque-vault-id",
  "record_id": "opaque-record-id",
  "revision_id": "opaque-revision-id",
  "kind": "reflection",
  "parents": ["prior-revision-id"],
  "author": {"device_id": "opaque-device-id", "sequence": 42},
  "schema_version": 1,
  "created_at_utc_ms": 1793500000000,
  "tombstone": false,
  "payload": {"body": "A little more space today.", "session_id": null},
  "payload_sha256": "hex-digest-of-defined-canonical-payload-bytes"
}
```

Specify canonical serialization, Unicode handling, numeric ranges, maximum payload size, and digest coverage before implementation. Preserve user text without normalization that changes its meaning; canonicalize the container representation, not the prose. Keep names free of reflection text, dates, emotions, and email addresses.

Unsupported protocol versions remain stored but unapplied. An older client must not rewrite newer payloads after dropping fields it does not understand. Unknown data produces a recoverable compatibility state, not an empty replacement.

## Exchange and crash recovery

The transport abstraction should expose `list_changes(cursor)`, `read(object_id)`, `create_immutable(upload_identity, bytes)`, and declared capabilities. Cursors and provider IDs remain opaque. Full enumeration is the correctness fallback; delta feeds are optional optimizations.

1. Discover objects using a bounded page or full-scan batch and download their complete bytes.
2. Validate identity, size, integrity, protocol, and vault binding. Durably stage envelopes and pending dependencies.
3. Apply complete ancestry and rebuild affected heads/projections in a local transaction.
4. Commit the discovery cursor only when every item in that page has a durable disposition: accepted, pending, or quarantined with a visible error. A failed download leaves the page retryable.
5. Publish queued immutable revisions using stable identities, preferably in parent-before-child order. The receiver still handles any order.
6. Mark an upload delivered only after provider confirmation or a read-back that matches the expected identity and bytes. Retry ambiguity using the same upload identity.

Do not require remote rename, lock files, or atomic multi-object transactions. They do not normalize consistently across all target providers. Do not rely on filenames being unique in Google Drive. The revision ID inside validated content is the deduplication identity.

Use bounded exponential backoff with jitter for temporary failures; pause for credentials or quota action. Store retryable work durably. “Saved on this device” and “Synced” are different states. Never claim synchronized while ancestors, quarantined objects, or failed uploads remain unresolved.

Avoid a single mutable shared manifest that all devices overwrite. Immutable object discovery plus optional per-device hints is sufficient initially. Account changes create a separate provider binding; never mix cursor state across accounts or vaults.

## Google Drive first

Use only `https://www.googleapis.com/auth/drive.appdata`, create files with `parents: ["appDataFolder"]`, and enumerate with `spaces=appDataFolder`. This folder is app-specific and hidden from normal Drive browsing. Its files cannot be shared, moved across storage spaces, or trashed. Users can remove the app data, so independent backups remain necessary. [Drive application data](https://developers.google.com/workspace/drive/api/guides/appdata)

Allocate a pre-generated file ID with `space=appDataFolder` and persist it with the outbox item before upload. Google documents retries with such IDs: an already completed creation can return 409 without creating duplicates. Read back and verify matching content before accepting a conflict response as successful delivery. [Generate IDs](https://developers.google.com/workspace/drive/api/reference/rest/v3/files/generateIds), [Upload retry behavior](https://developers.google.com/workspace/drive/api/guides/manage-uploads#use_a_pre-generated_id_to_upload_files)

For bootstrap, obtain a change token, enumerate existing app-data files, then consume changes from the earlier token. Use `spaces=appDataFolder`; do not set `restrictToMyDrive=true`, which omits app-data changes. Store the next page token after durable processing and the new start token at the end. Drive documents these change tokens as non-expiring; account/state recovery can still require a new enumeration. [Drive changes](https://developers.google.com/workspace/drive/api/reference/rest/v3/changes/list)

Small bounded revision objects suit multipart uploads; resumable uploads are available for interrupted transfers. Never mark a partial download or unverified object applied. Keep object batching and compaction out of the first protocol unless measured API volume demands them. [Drive uploads](https://developers.google.com/workspace/drive/api/guides/manage-uploads)

The product maintainer registers the OAuth consent application and platform clients. Ordinary users should only connect and authorize. Verify that the chosen desktop, Android, and iOS client configuration reaches the intended shared app-data collection before promising cross-platform sync.

## Other storage adapters

| Provider | Planned adapter and limits |
| --- | --- |
| OneDrive | Use the application folder and delegated `Files.ReadWrite.AppFolder`. Users can edit/remove its files; validate every object. Test delta access under this narrow permission rather than broadening scope automatically. [App folders](https://learn.microsoft.com/en-us/graph/onedrive-sharepoint-appfolder) |
| WebDAV / Nextcloud | Use HTTPS and a dedicated folder, `PROPFIND`, GET/PUT, ETags, and supported preconditions. Probe capabilities instead of assuming identical behavior across servers. Nextcloud may require an app password when external authentication or 2FA is used. [Nextcloud operations](https://docs.nextcloud.com/server/latest/developer_manual/client_apis/WebDAV/basic.html) |
| S3-compatible / R2 | Use immutable keys and bucket/prefix-scoped credentials. AWS supports conditional creation with `If-None-Match`; check each endpoint's compatibility. R2 maintains a feature-level matrix, so AWS semantics cannot simply be assumed. [AWS conditional writes](https://docs.aws.amazon.com/AmazonS3/latest/userguide/conditional-writes.html), [R2 API compatibility](https://developers.cloudflare.com/r2/api/s3/api/) |
| Syncthing | Later advanced exchange-directory adapter containing immutable revision objects. Import by validated revision identity even if filenames change. Syncthing's conflict copies do not replace semantic reflection merging. Never sync the live database directory. [Syncthing conflicts](https://docs.syncthing.net/users/syncing) |

Provider switching copies and verifies the complete revision set before changing the active binding. Retain the former copy until the user chooses to remove it. Do not run bidirectional synchronization against several providers in v1.

## Authorization and credentials

Use external browser authorization and Authorization Code + PKCE. Google desktop clients are public clients; bundled client secrets are not confidential. Use provider-supported mobile authorization integrations rather than applying desktop loopback redirects to phones. [Google desktop OAuth](https://developers.google.com/identity/protocols/oauth2/native-app), [Native OAuth guidance](https://www.rfc-editor.org/rfc/rfc8252)

Store refresh tokens, S3 secrets, WebDAV passwords, and AI API keys behind an OS credential-store adapter: Windows credential protection, Apple Keychain, Android Keystore-backed storage, and Linux Secret Service. If unavailable, offer session-only credentials or an explicitly unlocked encrypted store; never silently fall back to plaintext.

Credentials do not enter revision payloads, Markdown exports, logs, or portable backup manifests. Revoked access pauses synchronization while local features continue. “Disconnect” removes the local authorization; deleting remote journal data is a separate explicit action.

Production OAuth setup is a release prerequisite. Google's external Testing status normally issues refresh tokens expiring after seven days for this Drive scope. Handle invalid grants by asking the user to reconnect, without repeatedly opening a browser. [Google token lifecycle](https://developers.google.com/identity/protocols/oauth2/#expiration)

## Optional end-to-end encryption

Reserve a versioned envelope with algorithm and key identifiers, authenticated metadata, nonce, ciphertext, and integrity information. Select a reviewed authenticated-encryption implementation and conduct a security review before shipping. A hash alone cannot protect against malicious modification; TLS and provider-side encryption are not end-to-end encryption.

Bootstrap trust through a recovery key or authenticated device-pairing exchange. Do not trust a replacement key merely because it appeared in the same cloud folder. Encrypt sensitive record metadata as well as text; document unavoidable leakage such as object count, size, upload timing, and routing identifiers.

Specify key rotation, device enrollment/revocation, and recovery before enabling the feature. Removing a device cannot retract plaintext it already received. Lost recovery material can make encrypted history unrecoverable. No central Rhythm server means no server-side key reset that can recover the user's data.

Freeze serialized ciphertext for an outbox retry; do not reuse a nonce with changed plaintext. Keep encryption versions independent from record schema versions. Test wrong keys, tampering, replay, and downgrade attempts. A brand-new device cannot prove that the provider supplied the newest history without an additional trusted checkpoint; document that limitation.

Cloud E2EE does not encrypt the local SQLite database or exported Markdown. Local encryption, encrypted backups, and plain-text export warnings are separate choices. AI processing remains a separate consent boundary even when the journal is encrypted during sync.

## Statistics and calendar allocation

Completed period counts belong to the period's local **end day**, using a trustworthy effective end, source timezone, and offset. The effective end is the verified timer deadline, distinct from `detected_at`, when the process observes completion. Waking after midnight must not move an earlier completion to the wake day. If the effective end cannot be reliably mapped to a local date, place the completion in an uncertainty group instead. Interrupted/aborted periods remain visible but do not count as completed. Deduplicate by stable session identity, never by similar timestamps or text.

Measured duration is authoritative and independent of wall-clock subtraction. For a period, `recorded_elapsed = min(verified_active_elapsed, planned_duration)`. Cap the final interval at the verified deadline; time between that deadline and detection or confirmation never counts. A delayed callback must not turn a 25-minute focus period into hours of recorded focus. Persist active intervals with start/end UTC observations, source zone/offset, measured elapsed duration, and allocation status. Exclude paused and waiting-for-confirmation intervals. Recovered segments with uncertain elapsed time remain separately identified rather than assumed to fill the planned duration.

Split trustworthy active intervals at local calendar boundaries and timezone changes. Allocate their measured elapsed duration across the resulting slices in proportion to the consistent elapsed mapping, preserving the interval total with deterministic rounding. Persist or reproducibly derive these daily allocations; SQL sums them for daily, weekly, and monthly views.

When a wall-clock correction, reboot, or missing lifecycle checkpoint leaves no reliable mapped wall-clock anchor, retain known measured duration as unallocated/uncertain instead of inventing UTC boundaries or a date split. Date-scoped totals exclude that portion and disclose it separately. A clock change must not create negative duration, count the wait after a deadline, or move completed-period counts by recalculating old dates.

Use one current accepted terminal session projection per session. If contradictory terminal revisions remain unresolved, flag/exclude the affected session from definitive statistics rather than double-counting. Use SQL for counts and sums; AI may discuss selected text but may not replace those calculations.

## Markdown, backups, and migrations

Export UTF-8 Markdown for daily, weekly, and monthly ranges, with stable IDs, source dates/timezones, optional intentions and reflections, and clearly labeled conflict variants. Obsidian-compatible filenames can use `YYYY-MM-DD.md`, `YYYY-Www.md`, and `YYYY-MM.md`; define ISO week semantics and avoid locale-dependent filename characters.

Write into a dedicated export destination. Do not silently overwrite a file the user edited in Obsidian: use a manifest with prior export hashes, ask for the intended replacement behavior, or create a separate new export. Use YAML escaping for front matter, preserve Markdown text, and keep credentials and internal transport metadata out. Export is one-way in v1; editing an exported note does not yet import changes.

Create portable backups with SQLite's online backup API or `VACUUM INTO`, then verify integrity and package the snapshot with schema/application versions and a manifest. Copying only a live database's main file can omit WAL state. Backups are explicit snapshots, not the synchronization transport. [SQLite backup API](https://www.sqlite.org/backup.html)

Before restoring, validate the snapshot and migration path, preserve the current database, then replace it atomically. The implemented local slice uses SQLite's destination backup transaction on the single storage worker, without renaming a database under an open connection. It assigns a fresh installation identity and clears local provider bindings/outbox state. Remote-history merging remains future synchronization work. Restoring an older backup must not overwrite the remote revision graph or silently resurrect every deleted record. See [backup and export](backup-and-export.md) for the current standalone SQLite format and its limits; packaged manifests remain a later enhancement.

Apply forward migrations transactionally where supported, verify checksums, and retain a pre-migration snapshot. A newer unsupported database opens in a safe recovery/export state rather than being downgraded. Test every supported upgrade path and an interrupted migration. Keep database schema versions distinct from wire and record versions.

## Release verification

| Scenario | Required result |
| --- | --- |
| Concurrent edit/edit and edit/delete | Every authored variant remains recoverable; no timestamp winner |
| Resolution followed by a late offline branch | New conflict appears without discarding the late branch |
| Duplicate, reordered, or missing-parent delivery | Idempotent application; incomplete ancestry remains pending |
| Corrupt, oversized, wrong-vault, or unknown-version object | Quarantine/compatibility state; no partial projection |
| Crash around save, upload, or cursor commit | Local save survives; replay and retry do not duplicate records |
| Draft autosave/finalize failure, restart, or moved base | Durable draft survives; atomic finalization and explicit conflict handling |
| Ambiguous upload timeout or pagination replay | Stable upload identity and content verification recover safely |
| Quota, revoked token, removed cloud data | Local work continues; actionable recovery preserves notes |
| Restored database or long-offline device | New device identity; old branches and tombstones remain valid |
| DST, travel, midnight, clock correction, uncertain recovery | Counts use effective local end day or uncertainty group; measured totals remain conserved |
| Completion detected long after its deadline | Recorded duration is capped; post-deadline waiting and wake-day reassignment are excluded |
| Backup/restore and schema upgrades | Integrity checks pass and historical data remains readable |
| Different delivery orders across devices | Same accepted revisions, heads, and definitive statistics |

The synchronization release gate is two-device convergence under fault injection, with a recoverable copy of every authored reflection. Provider connection success alone is not sufficient.
