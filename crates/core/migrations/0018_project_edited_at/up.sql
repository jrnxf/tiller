-- Migration 18: when a project last changed (state.md section 8). Every
-- mutation that bumps `projects.rev` sets `edited_at` in the same statement,
-- so an edit refused for a stale rev can say when the other change happened.
--
-- A row from before this reads the newest of: when it was added, its newest
-- project event, and its newest card (card.create bumps the project's rev).
-- An insert that leaves the column out takes `added_at`, so a new row is
-- never without one.

ALTER TABLE projects ADD COLUMN edited_at TEXT;

UPDATE projects SET edited_at = max(
  added_at,
  coalesce((SELECT max(at) FROM events
            WHERE entity = 'project' AND entity_id = projects.id), added_at),
  coalesce((SELECT max(created_at) FROM cards
            WHERE project_id = projects.id), added_at)
);

CREATE TRIGGER projects_edited_at_on_insert AFTER INSERT ON projects
WHEN NEW.edited_at IS NULL
BEGIN
  UPDATE projects SET edited_at = NEW.added_at WHERE id = NEW.id;
END;

UPDATE meta SET value = '18' WHERE key = 'schema_version';
