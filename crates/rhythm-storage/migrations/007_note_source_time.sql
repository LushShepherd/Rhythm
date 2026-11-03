ALTER TABLE local_period_notes ADD COLUMN source_timezone TEXT;
ALTER TABLE local_period_notes ADD COLUMN source_utc_offset_minutes INTEGER CHECK(source_utc_offset_minutes BETWEEN -1439 AND 1439);
ALTER TABLE reflection_drafts ADD COLUMN source_timezone TEXT;
ALTER TABLE reflection_drafts ADD COLUMN source_utc_offset_minutes INTEGER CHECK(source_utc_offset_minutes BETWEEN -1439 AND 1439);
ALTER TABLE record_revisions ADD COLUMN source_timezone TEXT;
ALTER TABLE record_revisions ADD COLUMN source_utc_offset_minutes INTEGER CHECK(source_utc_offset_minutes BETWEEN -1439 AND 1439);

-- Existing UTC timestamps are authoritative. Their original writing zone was
-- never recorded, so migration deliberately leaves provenance unknown.
