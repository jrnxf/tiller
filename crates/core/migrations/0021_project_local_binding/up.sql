-- Migration 21: whether a project's builders and verifiers may bind and
-- connect to local ports in their sandbox (authority.md, Board API
-- credential and G24; state.md section 8). Off for every project, as the
-- sandbox was before: only the owner, or the project's own orchestrator,
-- turns it on.

ALTER TABLE projects ADD COLUMN local_binding INTEGER NOT NULL DEFAULT 0
  CHECK (local_binding IN (0, 1));

UPDATE meta SET value = '21' WHERE key = 'schema_version';
