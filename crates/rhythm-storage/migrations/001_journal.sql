CREATE TABLE local_identity (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
    device_id TEXT NOT NULL UNIQUE,
    next_sequence INTEGER NOT NULL CHECK (next_sequence > 0)
);

CREATE TABLE records (
    record_id TEXT PRIMARY KEY NOT NULL,
    kind TEXT NOT NULL CHECK (kind IN ('reflection', 'session')),
    created_at_utc_ms INTEGER NOT NULL
);

CREATE TABLE record_revisions (
    revision_id TEXT PRIMARY KEY NOT NULL,
    record_id TEXT NOT NULL REFERENCES records(record_id),
    author_device_id TEXT NOT NULL,
    author_sequence INTEGER NOT NULL CHECK (author_sequence > 0),
    schema_version INTEGER NOT NULL CHECK (schema_version = 1),
    created_at_utc_ms INTEGER NOT NULL,
    payload_json TEXT NOT NULL CHECK (json_valid(payload_json)),
    payload_sha256 BLOB NOT NULL CHECK (length(payload_sha256) = 32),
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

CREATE TABLE reflection_drafts (
    draft_id TEXT PRIMARY KEY NOT NULL,
    record_id TEXT REFERENCES records(record_id),
    bases_json TEXT NOT NULL CHECK (json_valid(bases_json)),
    payload_json TEXT NOT NULL CHECK (json_valid(payload_json)),
    version INTEGER NOT NULL CHECK (version > 0),
    created_at_utc_ms INTEGER NOT NULL,
    updated_at_utc_ms INTEGER NOT NULL
);

CREATE INDEX revisions_by_record ON record_revisions(record_id, author_sequence);
CREATE INDEX records_by_creation ON records(created_at_utc_ms DESC, record_id);
CREATE INDEX drafts_by_update ON reflection_drafts(updated_at_utc_ms DESC, draft_id);

CREATE TRIGGER immutable_revisions_update BEFORE UPDATE ON record_revisions
BEGIN SELECT RAISE(ABORT, 'revisions are immutable'); END;
CREATE TRIGGER immutable_revisions_delete BEFORE DELETE ON record_revisions
BEGIN SELECT RAISE(ABORT, 'revisions are retained'); END;
