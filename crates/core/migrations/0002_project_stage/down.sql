-- Migration 2 down: drop the review passes and the stage columns.

DROP INDEX review_passes_live;
DROP TABLE review_passes;
ALTER TABLE projects DROP COLUMN stage_changed_at;
ALTER TABLE projects DROP COLUMN reviewed_through_sha;
ALTER TABLE projects DROP COLUMN stage;

UPDATE meta SET value = '1' WHERE key = 'schema_version';
