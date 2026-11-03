-- Migration 2: a project's stage and its batched reviews
-- (docs/design/pre-launch-validation.md). Every existing project reads
-- 'launched' from the column default, so no project loses validation.

ALTER TABLE projects ADD COLUMN stage TEXT NOT NULL DEFAULT 'launched'
  CHECK (stage IN ('pre-launch', 'launched'));
-- the last commit a batched review covered
ALTER TABLE projects ADD COLUMN reviewed_through_sha TEXT;
ALTER TABLE projects ADD COLUMN stage_changed_at TEXT;

CREATE TABLE review_passes (
  id             TEXT PRIMARY KEY,
  project_id     TEXT NOT NULL REFERENCES projects(id),
  card_id        TEXT NOT NULL REFERENCES cards(id),   -- the pass card (kind scout)
  kind           TEXT NOT NULL CHECK (kind IN ('weekly', 'final')),
  base_sha       TEXT NOT NULL,                        -- = reviewed_through_sha at start
  head_sha       TEXT,                                 -- default-branch head, fixed at start
  chunks         TEXT NOT NULL DEFAULT '[]',           -- JSON chunk plan
  state          TEXT NOT NULL DEFAULT 'pending' CHECK (state IN
                   ('pending', 'running', 'finished', 'abandoned')),
  report_artifact_id TEXT REFERENCES artifacts(id),
  fix_card_ids   TEXT NOT NULL DEFAULT '[]',           -- JSON
  obligation_id  TEXT REFERENCES obligations(id),      -- the weekly due date that started it
  created_at     TEXT NOT NULL,
  started_at     TEXT,
  finished_at    TEXT,
  rev            INTEGER NOT NULL DEFAULT 1
) STRICT;
CREATE UNIQUE INDEX review_passes_live ON review_passes(project_id)
  WHERE state IN ('pending', 'running');

UPDATE meta SET value = '2' WHERE key = 'schema_version';
