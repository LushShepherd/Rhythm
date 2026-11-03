CREATE TABLE local_routine_deliveries_v4 (
    routine_id TEXT NOT NULL REFERENCES local_routines(routine_id) ON DELETE CASCADE,
    local_date TEXT NOT NULL,
    window_key TEXT NOT NULL,
    due_at_utc_ms INTEGER NOT NULL,
    dismissed INTEGER NOT NULL DEFAULT 0 CHECK (dismissed IN (0, 1)),
    PRIMARY KEY (routine_id, local_date, window_key)
);

INSERT INTO local_routine_deliveries_v4 (routine_id, local_date, window_key, due_at_utc_ms)
SELECT routine_id, local_date, 'legacy', due_at_utc_ms FROM local_routine_deliveries;

DROP TABLE local_routine_deliveries;
ALTER TABLE local_routine_deliveries_v4 RENAME TO local_routine_deliveries;
