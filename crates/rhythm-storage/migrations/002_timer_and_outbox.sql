CREATE TABLE local_timer_checkpoint (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
    generation INTEGER NOT NULL CHECK (generation > 0),
    checkpoint_json TEXT NOT NULL CHECK (json_valid(checkpoint_json))
);

CREATE TABLE terminal_sessions (
    session_id TEXT PRIMARY KEY NOT NULL REFERENCES records(record_id),
    revision_id TEXT NOT NULL UNIQUE REFERENCES record_revisions(revision_id),
    payload_json TEXT NOT NULL CHECK (json_valid(payload_json))
);

CREATE TABLE provider_bindings (
    binding_id TEXT PRIMARY KEY NOT NULL,
    active INTEGER NOT NULL CHECK (active IN (0, 1))
);

CREATE UNIQUE INDEX one_active_binding ON provider_bindings(active) WHERE active = 1;

CREATE TABLE sync_outbox (
    provider_binding_id TEXT NOT NULL REFERENCES provider_bindings(binding_id),
    revision_id TEXT NOT NULL REFERENCES record_revisions(revision_id),
    remote_upload_identity TEXT,
    PRIMARY KEY (provider_binding_id, revision_id)
);
