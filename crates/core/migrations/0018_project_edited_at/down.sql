-- Migration 18 down: forget when each project last changed. A stale-rev
-- conflict then names the rev and the field only, as it did before 18.

DROP TRIGGER projects_edited_at_on_insert;

ALTER TABLE projects DROP COLUMN edited_at;

UPDATE meta SET value = '17' WHERE key = 'schema_version';
