CREATE TABLE local_period_notes (
    period_id TEXT PRIMARY KEY,
    kind TEXT NOT NULL CHECK (kind IN ('focus', 'break')),
    body TEXT NOT NULL DEFAULT '',
    version INTEGER NOT NULL DEFAULT 1 CHECK (version > 0),
    created_at_utc_ms INTEGER NOT NULL,
    updated_at_utc_ms INTEGER NOT NULL,
    archived_at_utc_ms INTEGER,
    origin_draft_id TEXT UNIQUE
);

CREATE TABLE local_period_note_state (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
    current_period_id TEXT REFERENCES local_period_notes(period_id),
    generation INTEGER NOT NULL DEFAULT 0 CHECK (generation >= 0)
);

INSERT INTO local_period_note_state(singleton, current_period_id, generation) VALUES(1, NULL, 0);
CREATE INDEX local_period_note_archive ON local_period_notes(archived_at_utc_ms DESC, period_id);
