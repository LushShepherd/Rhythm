CREATE TABLE local_desktop_profile (
    singleton INTEGER PRIMARY KEY CHECK(singleton = 1),
    restore_token TEXT NOT NULL,
    payload_json TEXT NOT NULL CHECK(json_valid(payload_json)),
    payload_sha256 BLOB NOT NULL CHECK(length(payload_sha256) = 32)
);
