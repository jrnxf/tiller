-- Migration 8 down: the subjects are dropped; currency goes back to the card
-- and the run that opened the item.

ALTER TABLE attention DROP COLUMN subject_project_id;
ALTER TABLE attention DROP COLUMN subject_session_id;

UPDATE meta SET value = '7' WHERE key = 'schema_version';
