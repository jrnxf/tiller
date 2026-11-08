-- Migration 13 down: drop the host's bug project, so every Tiller bug files
-- in `fleet` again.

ALTER TABLE hosts DROP COLUMN bug_project_id;

UPDATE meta SET value = '12' WHERE key = 'schema_version';
