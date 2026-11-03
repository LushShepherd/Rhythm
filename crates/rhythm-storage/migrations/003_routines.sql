CREATE TABLE local_routines (
    routine_id TEXT PRIMARY KEY NOT NULL,
    input_json TEXT NOT NULL CHECK (json_valid(input_json))
);

CREATE TABLE local_routine_deliveries (
    routine_id TEXT NOT NULL REFERENCES local_routines(routine_id) ON DELETE CASCADE,
    local_date TEXT NOT NULL,
    due_at_utc_ms INTEGER NOT NULL,
    PRIMARY KEY (routine_id, local_date)
);
