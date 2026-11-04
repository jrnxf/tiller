-- Migration 9: the daemon publishes a direct-pr run (state.md, Publish).
-- A builder on a direct-pr project only commits. It records the PR title and
-- body with `tiller pr describe`, and when it reports done the daemon pushes
-- the branch and opens the PR outside the sandbox. One row per run holds the
-- description and the last push, so a retry or a restart pushes the same head
-- again at most and never opens a second PR.

CREATE TABLE publishes (
  run_id        TEXT PRIMARY KEY NOT NULL REFERENCES runs(id),
  title         TEXT NOT NULL,
  body          TEXT NOT NULL,
  described_at  TEXT NOT NULL,
  pushed_sha    TEXT,                     -- the head the daemon last pushed
  pushed_at     TEXT,
  pr_id         TEXT REFERENCES prs(id),  -- the PR it opened or found
  attempted_sha TEXT,                     -- the head of the last failed try
  attempted_at  TEXT,
  error         TEXT,                     -- the last failure, cleared on success
  rev           INTEGER NOT NULL DEFAULT 1,
  CHECK ((pushed_sha IS NULL) = (pushed_at IS NULL)),
  CHECK ((error IS NULL) OR (attempted_at IS NOT NULL))
) STRICT;

UPDATE meta SET value = '9' WHERE key = 'schema_version';
